//! Markdown API reference generation for local Mux modules.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;

use crate::analysis;
use crate::diagnostic::{Diagnostic, Level};
use crate::muxdoc::{DeclarationKind, DocComment, DocIndex, DocumentedDeclaration};
use crate::syntax;

const MARKER: &str = ".muxdoc-generated";

pub struct GenerateOutcome {
    pub paths: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

/// Generate API pages for an entry file and its resolved local imports.
pub fn generate(
    entry: &Path,
    output: &Path,
    deny_warnings: bool,
) -> Result<GenerateOutcome, String> {
    let entry = entry
        .canonicalize()
        .map_err(|error| format!("{}: {error}", entry.display()))?;
    if entry.extension().and_then(|ext| ext.to_str()) != Some("mux") {
        return Err("mux doc expects a .mux entry file".into());
    }
    let source =
        fs::read_to_string(&entry).map_err(|error| format!("{}: {error}", entry.display()))?;
    let analysis = analysis::analyze_source(&entry, &source);
    if let Some(diagnostic) = analysis
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.level == Level::Error)
    {
        return Err(display_diagnostic(&analysis, diagnostic));
    }
    let warnings = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Warning)
        .map(|diagnostic| display_diagnostic(&analysis, diagnostic))
        .collect::<Vec<_>>();
    if deny_warnings && let Some(warning) = warnings.first() {
        return Err(format!(
            "documentation generation stopped by --deny-warnings: {warning}"
        ));
    }
    let base = entry.parent().ok_or("entry file has no parent directory")?;
    let mut modules = Vec::new();
    let mut filenames = HashSet::new();
    for (_, path, source) in analysis.files.iter() {
        if path.to_string_lossy().starts_with("<embedded>") {
            continue;
        }
        let absolute = analysis::absolute_path(path);
        let relative = relative_path(base, &absolute)?;
        let slug = module_slug(&relative);
        let filename = format!("module-{slug}.md");
        if !filenames.insert(filename.clone()) {
            return Err(format!("two modules would generate {filename}"));
        }
        let parsed = syntax::parse_source(source);
        if !parsed.errors.is_empty() {
            return Err(format!("cannot parse {}", absolute.display()));
        }
        modules.push(Module {
            filename,
            title: relative
                .with_extension("")
                .to_string_lossy()
                .replace(['/', '\\'], "."),
            source: source.to_owned(),
            index: DocIndex::from_tree(&parsed.tree),
        });
    }
    modules.sort_by(|left, right| left.filename.cmp(&right.filename));
    let links = build_links(&modules);
    let mut pages = BTreeMap::new();
    let mut index = String::from("# API reference\n\n");
    for module in &modules {
        index.push_str(&format!("- [{}]({})\n", module.title, module.filename));
        pages.insert(module.filename.clone(), render_module(module, &links));
    }
    pages.insert("index.md".into(), index);
    write_pages(output, &pages)?;
    Ok(GenerateOutcome {
        paths: pages.keys().map(|name| output.join(name)).collect(),
        warnings,
    })
}

fn display_diagnostic(analysis: &analysis::Analysis, diagnostic: &Diagnostic) -> String {
    let Some(file_id) = diagnostic.file_id else {
        return format!("{}: {}", diagnostic.code, diagnostic.message);
    };
    let Some(path) = analysis.files.path(file_id) else {
        return format!("{}: {}", diagnostic.code, diagnostic.message);
    };
    let line = diagnostic
        .labels
        .first()
        .and_then(|label| label.span.byte_range)
        .and_then(|range| {
            analysis
                .files
                .source(file_id)
                .and_then(|source| source.get(..range.start))
                .map(|prefix| prefix.bytes().filter(|byte| *byte == b'\n').count() + 1)
        })
        .unwrap_or(1);
    format!(
        "{}:{line}: {}: {}",
        path.display(),
        diagnostic.code,
        diagnostic.message
    )
}

struct Module {
    filename: String,
    title: String,
    source: String,
    index: DocIndex,
}

fn module_slug(path: &Path) -> String {
    encode_name(&path.with_extension("").to_string_lossy())
}

fn encode_name(value: &str) -> String {
    let mut slug = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            slug.push(char::from(byte));
        } else {
            slug.push_str(&format!("~{byte:02X}"));
        }
    }
    slug
}

fn relative_path(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let base_parts = base.components().collect::<Vec<_>>();
    let path_parts = path.components().collect::<Vec<_>>();
    if base_parts.first() != path_parts.first() {
        return Err(format!(
            "cannot make a relative path for {}",
            path.display()
        ));
    }
    let shared = base_parts
        .iter()
        .zip(&path_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in shared..base_parts.len() {
        relative.push("..");
    }
    for component in &path_parts[shared..] {
        relative.push(component.as_os_str());
    }
    Ok(relative)
}

fn anchor(name: &str) -> String {
    encode_name(name)
}

fn build_links(modules: &[Module]) -> HashMap<String, (String, String)> {
    let mut links = HashMap::new();
    let mut ambiguous = HashSet::new();
    for module in modules {
        let module_name = module.title.rsplit('.').next().unwrap_or(&module.title);
        for declaration in module
            .index
            .declarations
            .iter()
            .filter(|declaration| declaration.api && declaration.kind != DeclarationKind::Test)
        {
            let target = (module.filename.clone(), anchor(&declaration.qualified_name));
            let keys = [
                declaration.qualified_name.clone(),
                declaration.name.clone(),
                format!("{module_name}.{}", declaration.qualified_name),
                format!("{}.{}", module.title, declaration.qualified_name),
            ]
            .into_iter()
            .collect::<HashSet<_>>();
            for key in keys {
                if links
                    .insert(key.clone(), target.clone())
                    .is_some_and(|previous| previous != target)
                {
                    ambiguous.insert(key);
                }
            }
        }
    }
    for name in ambiguous {
        links.remove(&name);
    }
    links
}

fn render_module(module: &Module, links: &HashMap<String, (String, String)>) -> String {
    let mut output = format!("# {}\n\n", module.title);
    for declaration in module
        .index
        .declarations
        .iter()
        .filter(|declaration| declaration.api && declaration.kind != DeclarationKind::Test)
    {
        output.push_str(&format!(
            "<a id=\"{}\"></a>\n\n## {}\n\n",
            anchor(&declaration.qualified_name),
            declaration.qualified_name
        ));
        output.push_str("```mux\n");
        output.push_str(&signature(&module.source, declaration));
        output.push_str("\n```\n\n");
        if let Some(doc) = &declaration.doc {
            output.push_str(&render_comment(doc, module, links));
            output.push_str("\n\n");
        }
    }
    output
}

fn signature(source: &str, declaration: &DocumentedDeclaration) -> String {
    let text = source
        .get(declaration.range.start..declaration.signature_end)
        .unwrap_or(&declaration.name);
    let mut signature = text.trim().trim_end_matches(',').trim().to_owned();
    if let Some(suffix) = declaration.signature_suffix
        && let Some(text) = source.get(suffix.start..suffix.end)
    {
        signature.push(' ');
        signature.push_str(text.trim());
    }
    signature
}

fn render_comment(
    doc: &DocComment,
    module: &Module,
    links: &HashMap<String, (String, String)>,
) -> String {
    doc.markdown_with_links(|target| {
        if let Some(declaration) = module.index.declarations.iter().find(|declaration| {
            declaration.api && (declaration.name == target || declaration.qualified_name == target)
        }) {
            return Some(format!("#{}", anchor(&declaration.qualified_name)));
        }
        links.get(target).map(|(file, anchor)| {
            if file == &module.filename {
                format!("#{anchor}")
            } else {
                format!("{file}#{anchor}")
            }
        })
    })
}

fn write_pages(output: &Path, pages: &BTreeMap<String, String>) -> Result<(), String> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("output path needs a directory name")?;
    let lock_path = parent.join(format!(".{name}.muxdoc.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("{}: {error}", lock_path.display()))?;
    lock.try_lock_exclusive()
        .map_err(|error| format!("cannot lock {}: {error}", output.display()))?;
    let old_exists = validate_output_directory(output)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let stage = parent.join(format!(
        ".{name}.muxdoc-stage-{}-{nonce}",
        std::process::id()
    ));
    let backup = parent.join(format!(
        ".{name}.muxdoc-backup-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&stage).map_err(|error| format!("{}: {error}", stage.display()))?;
    let staged = (|| -> Result<(), String> {
        for (filename, content) in pages {
            fs::write(stage.join(filename), content)
                .map_err(|error| format!("{filename}: {error}"))?;
        }
        let manifest = pages.keys().cloned().collect::<Vec<_>>().join("\n");
        fs::write(stage.join(MARKER), format!("{manifest}\n")).map_err(|error| error.to_string())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    if old_exists && let Err(error) = fs::rename(output, &backup) {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!("{}: {error}", output.display()));
    }
    if let Err(error) = fs::rename(&stage, output) {
        if old_exists {
            let _ = fs::rename(&backup, output);
        }
        let _ = fs::remove_dir_all(&stage);
        return Err(format!("{}: {error}", output.display()));
    }
    if old_exists {
        fs::remove_dir_all(&backup).map_err(|error| format!("{}: {error}", backup.display()))?;
    }
    Ok(())
}

fn validate_output_directory(output: &Path) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(output) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("{}: {error}", output.display())),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not a regular directory", output.display()));
    }
    let contents = fs::read_dir(output)
        .map_err(|error| format!("{}: {error}", output.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<HashSet<_>, _>>()?;
    if contents.is_empty() {
        return Ok(true);
    }
    let marker = fs::read_to_string(output.join(MARKER))
        .map_err(|_| "output directory is not managed by mux doc".to_owned())?;
    let expected = marker
        .lines()
        .map(str::to_owned)
        .chain(std::iter::once(MARKER.to_owned()))
        .collect::<HashSet<_>>();
    if contents != expected {
        return Err("output directory contains files not managed by mux doc".into());
    }
    for name in &expected {
        let path = output.join(name);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(format!("managed output {} is not a regular file", path.display()));
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_module_paths_have_distinct_page_names() {
        assert_ne!(
            module_slug(Path::new("foo/bar.mux")),
            module_slug(Path::new("foo-bar.mux"))
        );
        assert_ne!(anchor("Foo"), anchor("foo"));
    }

    #[test]
    fn signatures_keep_where_constraints_without_bodies() {
        let source = "class Server { int port } where { port > 0 }\nfunc positive(int value) where { value > 0 } returns int { return value }\n";
        let parsed = syntax::parse_source(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let index = DocIndex::from_tree(&parsed.tree);
        let server = index
            .declarations
            .iter()
            .find(|declaration| declaration.name == "Server")
            .unwrap();
        assert_eq!(signature(source, server), "class Server where { port > 0 }");
        let positive = index
            .declarations
            .iter()
            .find(|declaration| declaration.name == "positive")
            .unwrap();
        assert_eq!(
            signature(source, positive),
            "func positive(int value) where { value > 0 } returns int"
        );
    }

    #[test]
    fn generates_linked_pages_and_preserves_unmanaged_files() {
        let root = std::env::temp_dir().join(format!(
            "muxdoc-generator-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let entry = root.join("main.mux");
        fs::write(&entry, "import math\nfunc main() returns void { return }\n").unwrap();
        fs::write(root.join("math.mux"), "/**\n * Larger value.\n * see: min\n */\nfunc max(int a, int b) returns int { if a > b { return a } return b }\n/// Smaller value.\nfunc min(int a, int b) returns int { if a < b { return a } return b }\n/// A point.\nclass Point {\n    /// Horizontal position.\n    int x\n}\n").unwrap();
        let output = root.join("api");
        generate(&entry, &output, false).unwrap();
        let page = fs::read_to_string(output.join("module-math.md")).unwrap();
        assert!(page.contains("**See:** [min](#min)"));
        assert!(page.contains("```mux\nfunc max(int a, int b) returns int"));
        assert!(page.contains("```mux\nclass Point\n```"));
        assert!(page.contains("## Point.x"));
        assert!(
            fs::read_to_string(output.join("module-main.md"))
                .unwrap()
                .contains("func main() returns void")
        );
        generate(&entry, &output, false).unwrap();
        assert_eq!(
            page,
            fs::read_to_string(output.join("module-math.md")).unwrap()
        );
        fs::write(output.join("notes.txt"), "keep").unwrap();
        assert!(generate(&entry, &output, false).is_err());
        assert_eq!(
            fs::read_to_string(output.join("notes.txt")).unwrap(),
            "keep"
        );
        let invalid = root.join("invalid.mux");
        fs::write(
            &invalid,
            "/// absent: Wrong name\nfunc f() returns void { return }\n",
        )
        .unwrap();
        let invalid_output = root.join("invalid-api");
        assert!(generate(&invalid, &invalid_output, true).is_err());
        assert!(!invalid_output.exists());
        let outcome = generate(&invalid, &invalid_output, false).unwrap();
        assert!(
            outcome
                .warnings
                .iter()
                .any(|warning| warning.contains("W0500"))
        );
        fs::remove_dir_all(root).unwrap();
    }
}
