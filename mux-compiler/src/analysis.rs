//! Compiler frontend entry point for editor integrations.
//!
//! Each call analyzes a fresh source snapshot. This keeps editor buffer changes
//! from reusing the CLI's path and module caches until the compiler has an
//! explicit incremental analysis model.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::diagnostic::{
    Diagnostic, FileId, Files, SourceRange, TextEdit, ToDiagnostic,
    fix::{self, AppliedEdits, RecoveryIntervals},
};
use crate::{module_resolver::ModuleResolver, semantics::SemanticAnalyzer, syntax};

/// A complete frontend result for one editor source snapshot.
pub struct Analysis {
    /// Registered root and imported sources, with IDs local to this result.
    pub files: Files,
    /// Diagnostics from parsing and semantic analysis.
    pub diagnostics: Vec<Diagnostic>,
    /// Name expressions resolved by semantic analysis for editor navigation.
    pub resolved_identifiers: Vec<crate::semantics::ResolvedIdentifier>,
    /// Exported members of modules imported by the root source, used for
    /// namespace and import completion even when a module is runtime-backed.
    pub imported_modules: HashMap<String, HashMap<String, crate::semantics::SymbolKind>>,
    /// ID of the root source in `files`.
    pub root_file: FileId,
    /// Parser recovery ranges that safe edits must avoid.
    pub recovery: RecoveryIntervals,
}

/// Analyze source for editor use without emitting output or generating code.
///
/// Parse errors are retained while valid regions continue through semantic
/// analysis. `file_path` should be an absolute path when imports are present.
pub fn analyze_source(file_path: &Path, source: &str) -> Analysis {
    analyze_source_with_overlays(file_path, source, &HashMap::new())
}

/// Analyze source against the current contents of open files.
///
/// Overlay keys must be absolute paths to existing files. The resolver keys
/// them by canonical path so paths containing symlinks match disk imports.
pub fn analyze_source_with_overlays(
    file_path: &Path,
    source: &str,
    overlays: &HashMap<PathBuf, String>,
) -> Analysis {
    let mut files = Files::new();
    let root_file = files.add(file_path, source.to_owned());
    let parsed = syntax::parse_source(source);
    let mut diagnostics = parsed
        .errors
        .iter()
        .map(|error| error.to_diagnostic(root_file))
        .collect::<Vec<_>>();
    let mut recovery = fix::RecoveryIntervals::new();
    for byte_range in parsed
        .errors
        .iter()
        .filter_map(syntax::FrontendError::byte_range)
        .chain(parsed.recovery_byte_ranges())
    {
        if let Ok(range) = fix::source_range_for_byte_range(source, byte_range) {
            recovery.add(root_file, range);
        }
    }

    let base_path = file_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let resolver = Rc::new(RefCell::new(ModuleResolver::new(base_path)));
    {
        let mut resolver = resolver.borrow_mut();
        resolver.set_emit_diagnostics(false);
        resolver.set_source_overrides(overlays.clone());
    }
    let mut analyzer = SemanticAnalyzer::new_with_resolver(Rc::clone(&resolver));
    analyzer.enable_editor_references();
    analyzer.set_current_file(file_path.to_path_buf());
    analyzer.set_current_file_id(root_file);

    if !parsed
        .errors
        .iter()
        .any(|error| matches!(error, syntax::FrontendError::Lexer(_)))
    {
        let nodes = if parsed.has_errors() {
            Ok(parsed.lower_recovered())
        } else {
            parsed.lower().map_err(|error| error.to_string())
        };
        match nodes {
            Ok(nodes) => {
                let errors = analyzer.analyze(&nodes, Some(&mut files));
                let imported_errors = analyzer.take_imported_errors();
                let mut semantic_diagnostics = errors
                    .iter()
                    .chain(imported_errors.iter())
                    .map(|error| error.to_diagnostic(root_file))
                    .collect::<Vec<_>>();
                semantic_diagnostics.retain(|diagnostic| {
                    diagnostic.file_id != Some(root_file)
                        || !diagnostic.labels.iter().any(|label| {
                            label.span.byte_range.is_some_and(|range| {
                                recovery
                                    .touches(root_file, SourceRange::new(range.start, range.end))
                            })
                        })
                });
                diagnostics.extend(semantic_diagnostics);
            }
            Err(error) => diagnostics.push(
                Diagnostic::new(crate::diagnostic::DiagnosticCode::InternalCompiler)
                    .with_message(format!("Failed to lower parsed source: {error}"))
                    .with_file_id(root_file),
            ),
        }
    }

    diagnostics.extend(resolver.borrow_mut().take_diagnostics());
    recovery.extend(resolver.borrow_mut().take_recovery_intervals());
    crate::diagnostic::sort_diagnostics(&mut diagnostics, &files);
    let resolved_identifiers = analyzer.take_editor_references();
    let imported_modules = analyzer.take_editor_imported_modules();

    Analysis {
        files,
        diagnostics,
        resolved_identifiers,
        imported_modules,
        root_file,
        recovery,
    }
}

/// Apply machine-applicable edits in memory and rerun frontend analysis before
/// returning them to an editor. This never writes to the filesystem.
pub fn apply_fixes_and_validate(
    analysis: &Analysis,
    root_path: &Path,
    edits: &[TextEdit],
) -> Result<AppliedEdits, fix::FixError> {
    fix::apply_and_validate(&analysis.files, edits, &analysis.recovery, |updates| {
        let root_source = updates
            .get(&analysis.root_file)
            .map(String::as_str)
            .or_else(|| analysis.files.source(analysis.root_file))
            .ok_or_else(|| format!("root file {} is not registered", root_path.display()))?;
        let mut overlays = HashMap::new();
        for (file_id, path, source) in analysis.files.iter() {
            if path.to_string_lossy().starts_with("<embedded>/") {
                continue;
            }
            overlays.insert(
                absolute_path(path),
                updates
                    .get(&file_id)
                    .map_or(source, String::as_str)
                    .to_owned(),
            );
        }
        let staged = analyze_source_with_overlays(root_path, root_source, &overlays);
        if let Some(diagnostic) = first_new_error(analysis, &staged) {
            return Err(format!("{}: {}", diagnostic.code, diagnostic.message));
        }
        Ok(())
    })
}

fn first_new_error<'a>(baseline: &Analysis, staged: &'a Analysis) -> Option<&'a Diagnostic> {
    use crate::diagnostic::Level;

    let mut existing =
        HashMap::<(Option<PathBuf>, crate::diagnostic::DiagnosticCode, String), usize>::new();
    for diagnostic in baseline
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Error)
    {
        *existing.entry(error_key(baseline, diagnostic)).or_default() += 1;
    }
    for diagnostic in staged
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Error)
    {
        let count = existing.entry(error_key(staged, diagnostic)).or_default();
        if *count == 0 {
            return Some(diagnostic);
        }
        *count -= 1;
    }
    None
}

fn error_key(
    analysis: &Analysis,
    diagnostic: &Diagnostic,
) -> (Option<PathBuf>, crate::diagnostic::DiagnosticCode, String) {
    let path = diagnostic
        .file_id
        .and_then(|file_id| analysis.files.path(file_id))
        .map(Path::to_path_buf);
    (path, diagnostic.code, diagnostic.message.clone())
}

/// Make a path suitable for use as a stable source identity during analysis.
#[must_use]
pub fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze_source, analyze_source_with_overlays, apply_fixes_and_validate};
    use crate::diagnostic::{DiagnosticCode, Level, SourceRange, TextEdit};
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn returns_parser_diagnostics_for_incomplete_editor_buffers() {
        let source = "func main() returns void {\n    auto value = \"unfinished\n";
        let analysis = analyze_source(Path::new("/workspace/main.mux"), source);

        assert!(!analysis.diagnostics.is_empty());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.level == Level::Error)
        );
        assert_eq!(analysis.files.source(analysis.root_file), Some(source));
    }

    #[test]
    fn returns_semantic_diagnostics_without_codegen() {
        let source = "func main() returns void {\n    int value = \"wrong\"\n}\n";
        let analysis = analyze_source(Path::new("/workspace/main.mux"), source);

        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.level == Level::Error)
        );
    }

    #[test]
    fn resolved_import_references_keep_their_source_file() {
        let root = Path::new("/tmp/mux-lsp-analysis/main.mux");
        let imported = Path::new("/tmp/mux-lsp-analysis/tools.mux");
        let overlays = HashMap::from([(
            imported.to_path_buf(),
            "func helper() returns void { return }\n".to_owned(),
        )]);
        let analysis = analyze_source_with_overlays(
            root,
            "import tools.*\nfunc main() returns void {\n helper()\n return\n}\n",
            &overlays,
        );
        let reference = analysis
            .resolved_identifiers
            .iter()
            .find(|reference| reference.name == "helper")
            .unwrap_or_else(|| {
                panic!(
                    "the imported function reference should be resolved; diagnostics: {:?}; refs: {:?}",
                    analysis.diagnostics, analysis.resolved_identifiers
                )
            });

        assert_eq!(reference.source_path.as_deref(), Some(imported));
    }

    #[test]
    fn accepts_a_fix_when_unrelated_errors_remain_unchanged() {
        let path = Path::new("/tmp/mux-lsp-analysis/fix.mux");
        let source = "func main() returns void {\n    int bad = \"wrong\"\n    auto value = 1\n}\n";
        let analysis = analyze_source(path, source);
        let start = source.find("= 1").unwrap() + 2;
        let edit = TextEdit::machine_applicable(
            analysis.root_file,
            SourceRange::new(start, start + 1),
            "2",
            DiagnosticCode::TypeMismatch,
        );

        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.level == Level::Error)
        );
        assert!(apply_fixes_and_validate(&analysis, path, &[edit]).is_ok());
    }

    #[test]
    fn rejects_a_fix_that_introduces_an_error() {
        let path = Path::new("/tmp/mux-lsp-analysis/fix.mux");
        let source = "func main() returns void {\n    int value = 1\n}\n";
        let analysis = analyze_source(path, source);
        let start = source.find("= 1").unwrap() + 2;
        let edit = TextEdit::machine_applicable(
            analysis.root_file,
            SourceRange::new(start, start + 1),
            "\"wrong\"",
            DiagnosticCode::TypeMismatch,
        );

        assert!(apply_fixes_and_validate(&analysis, path, &[edit]).is_err());
    }
}
