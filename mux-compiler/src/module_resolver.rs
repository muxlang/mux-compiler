use crate::ast::AstNode;
use crate::diagnostic::fix::{self, RecoveryIntervals};
use crate::diagnostic::{
    ColorConfig, Diagnostic, DiagnosticCode, DiagnosticEmitter, FileId, Files, StandardEmitter,
    ToDiagnostic,
};
use crate::embedded_std::embedded_std_sources;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::source::Source;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ModuleResolutionError {
    pub code: DiagnosticCode,
    pub message: String,
}

impl ModuleResolutionError {
    fn new(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<String> for ModuleResolutionError {
    fn from(message: String) -> Self {
        Self::new(DiagnosticCode::ImportFailure, message)
    }
}

impl std::fmt::Display for ModuleResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModuleResolutionError {}

pub struct ModuleResolver {
    base_path: PathBuf,
    compiled_modules: HashMap<String, Vec<AstNode>>,
    import_stack: Vec<String>,
    being_imported: HashSet<String>,
    canonical_cache: HashMap<PathBuf, String>, // canonical path -> module_path
    source_overrides: HashMap<PathBuf, String>,
    module_file_ids: HashMap<String, FileId>,
    diagnostics: Vec<Diagnostic>,
    recovery_intervals: RecoveryIntervals,
    emit_diagnostics: bool,
    embedded_sources: &'static HashMap<String, String>,
}

impl ModuleResolver {
    pub fn new(base_path: PathBuf) -> Self {
        Self {
            base_path,
            compiled_modules: HashMap::new(),
            import_stack: Vec::new(),
            being_imported: HashSet::new(),
            canonical_cache: HashMap::new(),
            source_overrides: HashMap::new(),
            module_file_ids: HashMap::new(),
            diagnostics: Vec::new(),
            recovery_intervals: RecoveryIntervals::new(),
            emit_diagnostics: true,
            embedded_sources: embedded_std_sources(),
        }
    }

    fn resolve_embedded_key<'a>(&'a self, module_path: &'a str) -> Option<&'a str> {
        if self.embedded_sources.contains_key(module_path) {
            return Some(module_path);
        }

        module_path
            .strip_prefix("std.")
            .filter(|short| self.embedded_sources.contains_key(*short))
    }

    fn normalize_module_key<'a>(&'a self, module_path: &'a str) -> &'a str {
        self.resolve_embedded_key(module_path)
            .unwrap_or(module_path)
    }

    pub fn has_embedded_module(&self, module_path: &str) -> bool {
        self.resolve_embedded_key(module_path).is_some()
    }

    /// Use staged source for a local module during fix validation.
    pub fn set_source_overrides(&mut self, overrides: HashMap<PathBuf, String>) {
        self.source_overrides = overrides;
    }

    /// Control direct diagnostic emission while a caller is collecting a
    /// structured result such as `--format json`.
    pub fn set_emit_diagnostics(&mut self, emit: bool) {
        self.emit_diagnostics = emit;
    }

    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    pub fn take_recovery_intervals(&mut self) -> RecoveryIntervals {
        std::mem::take(&mut self.recovery_intervals)
    }

    /// Return the source file loaded for a resolved module.
    pub fn file_id_for_module(&self, module_path: &str) -> Option<FileId> {
        self.module_file_ids
            .get(self.normalize_module_key(module_path))
            .copied()
    }

    fn resolve_embedded_module_if_any(
        &mut self,
        module_path: &str,
        files: &mut Files,
    ) -> Result<Option<Vec<AstNode>>, ModuleResolutionError> {
        if let Some(embedded_key) = self.resolve_embedded_key(module_path) {
            let embedded_key = embedded_key.to_string();
            let cache_key = self.normalize_module_key(module_path).to_string();

            if let Some(nodes) = self.compiled_modules.get(&cache_key) {
                return Ok(Some(nodes.clone()));
            }

            if self.being_imported.contains(&cache_key) {
                return Err(ModuleResolutionError::new(
                    DiagnosticCode::ImportFailure,
                    format!(
                        "Circular import detected: {} -> {}",
                        self.import_stack.join(" -> "),
                        cache_key
                    ),
                ));
            }

            self.being_imported.insert(cache_key.clone());
            self.import_stack.push(cache_key.clone());

            let source = self.embedded_sources.get(&embedded_key).ok_or_else(|| {
                ModuleResolutionError::new(
                    DiagnosticCode::ModuleNotFound,
                    format!("Embedded module not found: {module_path}"),
                )
            })?;

            let virtual_path =
                PathBuf::from(format!("<embedded>/{}.mux", embedded_key.replace('.', "/")));
            let nodes = self.parse_module(&virtual_path, files, Some(source.as_str()))?;
            if let Some(file_id) = files.id_for_path(&virtual_path) {
                self.module_file_ids.insert(cache_key.clone(), file_id);
            }
            return Ok(Some(nodes));
        }
        Ok(None)
    }

    fn determine_file_path(
        &self,
        module_path: &str,
        current_file: Option<&Path>,
    ) -> Result<PathBuf, ModuleResolutionError> {
        if module_path.starts_with("./") || module_path.starts_with("../") {
            // Relative import - resolve relative to current file
            let current_dir = current_file.and_then(Path::parent).ok_or_else(|| {
                ModuleResolutionError::new(
                    DiagnosticCode::ImportFailure,
                    "Cannot resolve relative import: no current file",
                )
            })?;

            let relative_path = module_path.trim_start_matches("./");
            let mut path = current_dir.to_path_buf();

            // Handle ../ parts
            for part in relative_path.split('/') {
                if part == ".." {
                    path.pop();
                } else if !part.is_empty() {
                    path.push(part);
                }
            }
            path.set_extension("mux");
            Ok(path)
        } else if module_path.starts_with('/') {
            // Absolute import
            let mut path = PathBuf::from(module_path);
            path.set_extension("mux");
            Ok(path)
        } else {
            // Project-relative import (utils.logger)
            self.module_path_to_file(module_path)
        }
    }

    // Resolve import path relative to current file (for relative imports)
    pub fn resolve_import_path(
        &mut self,
        module_path: &str,
        current_file: Option<&Path>,
        files: &mut Files,
    ) -> Result<Vec<AstNode>, ModuleResolutionError> {
        if let Some(nodes) = self.resolve_embedded_module_if_any(module_path, files)? {
            return Ok(nodes);
        }

        let file_path = self.determine_file_path(module_path, current_file)?;

        // Canonicalize for cache key
        let canonical_path = file_path.canonicalize().map_err(|e| {
            ModuleResolutionError::new(
                DiagnosticCode::ImportFailure,
                format!("Cannot resolve module path {module_path}: {e}"),
            )
        })?;

        // Check cache by canonical path
        if let Some(cached_module_path) = self.canonical_cache.get(&canonical_path)
            && let Some(nodes) = self.compiled_modules.get(cached_module_path)
        {
            return Ok(nodes.clone());
        }

        // Check circular imports
        if self.being_imported.contains(module_path) {
            return Err(format!(
                "Circular import detected: {} -> {}",
                self.import_stack.join(" -> "),
                module_path
            )
            .into());
        }

        // Mark as being imported
        self.being_imported.insert(module_path.to_string());
        self.import_stack.push(module_path.to_string());

        // Parse module
        let source_override = self.source_overrides.get(&canonical_path).cloned();
        let nodes = self.parse_module(&canonical_path, files, source_override.as_deref())?;
        if let Some(file_id) = files.id_for_path(&canonical_path) {
            self.module_file_ids
                .insert(module_path.to_string(), file_id);
        }

        // Cache the canonical path
        self.canonical_cache
            .insert(canonical_path, module_path.to_string());

        Ok(nodes)
    }

    pub fn finish_import(&mut self, module_path: &str) {
        let cache_key = self.normalize_module_key(module_path).to_string();
        self.import_stack.pop();
        self.being_imported.remove(&cache_key);
    }

    pub fn cache_module(&mut self, module_path: &str, nodes: Vec<AstNode>) {
        let cache_key = self.normalize_module_key(module_path).to_string();
        self.compiled_modules.insert(cache_key, nodes);
    }

    /// Check if a module path resolves to a file, directory, both, or neither.
    /// Returns (has_file, has_directory) tuple.
    pub fn check_module_path(&self, module_path: &str) -> (bool, bool) {
        if self.has_embedded_module(module_path) {
            return (true, false);
        }

        let embedded_prefix = format!("{}.", module_path);
        let has_embedded_directory = self
            .embedded_sources
            .keys()
            .any(|key| key.starts_with(&embedded_prefix));

        let mut file_path = self.base_path.clone();
        for part in module_path.split('.') {
            file_path.push(part);
        }

        let mux_file = file_path.with_extension("mux");
        let dir_path = file_path;

        let has_file = mux_file.exists() && mux_file.is_file();
        let has_directory = dir_path.exists() && dir_path.is_dir();

        (has_file, has_directory || has_embedded_directory)
    }

    /// Get all .mux files in a directory module
    pub fn get_submodules(&self, module_path: &str) -> Result<Vec<String>, String> {
        let mut submodules = HashSet::new();
        let embedded_prefix = format!("{}.", module_path);
        for key in self.embedded_sources.keys() {
            if let Some(rest) = key.strip_prefix(&embedded_prefix)
                && !rest.is_empty()
                && let Some(child) = rest.split('.').next()
            {
                submodules.insert(child.to_string());
            }
        }

        let mut dir_path = self.base_path.clone();
        for part in module_path.split('.') {
            dir_path.push(part);
        }

        if dir_path.exists()
            && dir_path.is_dir()
            && let Ok(entries) = std::fs::read_dir(&dir_path)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && path.extension().is_some_and(|ext| ext == "mux")
                    && let Some(stem) = path.file_stem().and_then(std::ffi::OsStr::to_str)
                {
                    submodules.insert(stem.to_string());
                }
            }
        }

        if submodules.is_empty() {
            return Err(format!("Module directory not found: {}", module_path));
        }

        let mut result: Vec<String> = submodules.into_iter().collect();
        result.sort();
        Ok(result)
    }

    fn module_path_to_file(&self, module_path: &str) -> Result<PathBuf, ModuleResolutionError> {
        let mut path = self.base_path.clone();
        for part in module_path.split('.') {
            path.push(part);
        }
        path.set_extension("mux");

        if !path.exists() {
            return Err(ModuleResolutionError::new(
                DiagnosticCode::ModuleNotFound,
                format!("Module not found: {module_path} (looked for {path:?})"),
            ));
        }

        Ok(path)
    }

    fn parse_module(
        &mut self,
        file_path: &Path,
        files: &mut Files,
        source_override: Option<&str>,
    ) -> Result<Vec<AstNode>, String> {
        let source_str = self.read_module_source(file_path, source_override)?;
        let file_id = files.add(file_path, source_str.clone());
        let mut src = Source::from_string(source_str);
        let tokens = self.lex_module(file_path, file_id, files, &mut src)?;
        self.parse_module_tokens(file_path, file_id, files, &tokens)
    }

    fn read_module_source(
        &self,
        file_path: &Path,
        source_override: Option<&str>,
    ) -> Result<String, String> {
        source_override.map_or_else(
            || {
                std::fs::read_to_string(file_path)
                    .map_err(|e| format!("Failed to open module: {e}"))
            },
            |source| Ok(source.to_string()),
        )
    }

    fn lex_module(
        &mut self,
        file_path: &Path,
        file_id: FileId,
        files: &Files,
        source: &mut Source,
    ) -> Result<Vec<crate::lexer::Token>, String> {
        let mut lex = Lexer::new(source);
        let tokens = match lex.lex_all() {
            Ok(t) => t,
            Err(e) => {
                let diagnostic = e.to_diagnostic(file_id);
                if self.emit_diagnostics {
                    crate::spinner::stop();
                    let emitter = StandardEmitter::new(ColorConfig::Auto);
                    emitter.emit(&diagnostic, files);
                }
                if let Ok(range) =
                    fix::source_range_for_span(files.source(file_id).unwrap_or_default(), e.span)
                {
                    self.recovery_intervals.add(file_id, range);
                }
                self.diagnostics.push(diagnostic);
                return Err(format!("Lexer error in module {}", file_path.display()));
            }
        };
        Ok(tokens)
    }

    fn parse_module_tokens(
        &mut self,
        file_path: &Path,
        file_id: FileId,
        files: &Files,
        tokens: &[crate::lexer::Token],
    ) -> Result<Vec<AstNode>, String> {
        let mut parser = Parser::new(tokens);
        match parser.parse() {
            Ok(nodes) => Ok(nodes),
            Err((_, errors)) => {
                let source = files.source(file_id).unwrap_or_default();
                let diagnostics: Vec<_> = errors
                    .iter()
                    .map(|error| {
                        if let Ok(range) = fix::source_range_for_span(source, error.span) {
                            self.recovery_intervals.add(file_id, range);
                        }
                        error.to_diagnostic(file_id)
                    })
                    .collect();
                for span in parser.recovery_spans() {
                    if let Ok(range) = fix::source_range_for_span(source, *span) {
                        self.recovery_intervals.add(file_id, range);
                    }
                }
                if self.emit_diagnostics {
                    crate::spinner::stop();
                    let emitter = StandardEmitter::new(ColorConfig::Auto);
                    emitter.emit_batch(&diagnostics, files);
                }
                self.diagnostics.extend(diagnostics);
                Err(format!("Parse error in module {}", file_path.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("mux_mod_{}_{}_{}", tag, std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_import_path_reports_lex_and_parse_errors() {
        let dir = unique_tmp_dir("broken");
        std::fs::write(dir.join("badlex.mux"), "auto s = \"unterminated\n").unwrap();
        std::fs::write(dir.join("badparse.mux"), "func main( {\n    return\n}\n").unwrap();

        let mut resolver = ModuleResolver::new(dir.clone());
        let mut files = Files::new();

        let err = resolver
            .resolve_import_path("badlex", None, &mut files)
            .expect_err("lex error module must fail");
        assert!(err.message.contains("Lexer error in module"), "got: {err}");

        let err = resolver
            .resolve_import_path("badparse", None, &mut files)
            .expect_err("parse error module must fail");
        assert!(err.message.contains("Parse error in module"), "got: {err}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
