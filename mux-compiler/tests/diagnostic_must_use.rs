use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL_PROBE: &str = r#"
#![deny(unused_must_use)]

use mux_lang::diagnostic::Label;
use mux_lang::lexer::Span;

fn main() {
    Label::primary(Span::new(1, 1), "label");
}
"#;

const NEW_PROBE: &str = r#"
#![deny(unused_must_use)]

use mux_lang::diagnostic::{Diagnostic, DiagnosticCode};

fn main() {
    Diagnostic::new(DiagnosticCode::InternalCompiler);
}
"#;

const BUILDER_PROBE: &str = r#"
#![deny(unused_must_use)]

use mux_lang::diagnostic::{
    diagnostic_from_parts_with_help, Diagnostic, DiagnosticCode, Files, Label, SpanEdit,
};
use mux_lang::lexer::Span;

fn main() {
    let span = Span::new(1, 1);
    let mut files = Files::new();
    let file_id = files.add("fixture.mux", "value".to_owned());

    diagnostic_from_parts_with_help(
        DiagnosticCode::InternalCompiler,
        "message",
        None,
        span,
        file_id,
    );
    Diagnostic::new(DiagnosticCode::InternalCompiler).with_message("message");
    Diagnostic::new(DiagnosticCode::InternalCompiler)
        .with_label(Label::primary(span, "label"));
    Diagnostic::new(DiagnosticCode::InternalCompiler).with_help(None::<String>);
    Diagnostic::new(DiagnosticCode::InternalCompiler).with_file_id(file_id);
    Diagnostic::new(DiagnosticCode::InternalCompiler).with_span_edit(SpanEdit::machine_applicable_text(
        span,
        "replacement",
        DiagnosticCode::InternalCompiler,
    ));
    Diagnostic::new(DiagnosticCode::InternalCompiler).with_span_edits(std::iter::empty());
}
"#;

fn cargo_executable() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

struct ProbeDirectory(PathBuf);

impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

struct ProbeFile(PathBuf);

impl Drop for ProbeFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn write_probe(examples_dir: &Path, name: &str, source: &str) -> Result<ProbeFile, String> {
    let path = examples_dir.join(format!("{name}.rs"));
    fs::write(&path, source).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(ProbeFile(path))
}

fn run_probe(
    package_root: &Path,
    examples_dir: &Path,
    name: &str,
    source: &str,
) -> Result<String, String> {
    let _probe = write_probe(examples_dir, name, source)?;
    let result = Command::new(cargo_executable())
        .arg("check")
        .arg("--manifest-path")
        .arg(package_root.join("Cargo.toml"))
        .arg("--example")
        .arg(name)
        .arg("--locked")
        .arg("--offline")
        .current_dir(package_root)
        .output()
        .map_err(|error| format!("run cargo for {name} must-use probe: {error}"))?;

    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    if result.status.success() {
        return Err(format!(
            "{name} must-use probe unexpectedly compiled; its annotation may be missing"
        ));
    }
    if !stderr.contains("unused_must_use") {
        return Err(format!(
            "{name} must-use probe failed for an unexpected reason:\n{stderr}"
        ));
    }
    Ok(stderr)
}

fn require_diagnostic(stderr: &str, method: &str) -> Result<(), String> {
    if stderr.contains(method) {
        Ok(())
    } else {
        Err(format!(
            "must-use probe did not report discarded {method} result:\n{stderr}"
        ))
    }
}

#[test]
fn diagnostic_values_are_compile_time_must_use_contracts() -> Result<(), String> {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = package_root.join("examples");
    let directory = if examples_dir.exists() {
        None
    } else {
        fs::create_dir(&examples_dir)
            .map_err(|error| format!("create {}: {error}", examples_dir.display()))?;
        Some(ProbeDirectory(examples_dir.clone()))
    };
    let pid = std::process::id();
    let label_stderr = run_probe(
        package_root,
        &examples_dir,
        &format!("diagnostic_label_must_use_{pid}"),
        LABEL_PROBE,
    )?;
    require_diagnostic(&label_stderr, "Label::primary")?;

    let new_stderr = run_probe(
        package_root,
        &examples_dir,
        &format!("diagnostic_new_must_use_{pid}"),
        NEW_PROBE,
    )?;
    require_diagnostic(&new_stderr, "Diagnostic::new")?;

    let builder_stderr = run_probe(
        package_root,
        &examples_dir,
        &format!("diagnostic_builder_must_use_{pid}"),
        BUILDER_PROBE,
    )?;
    for method in [
        "diagnostic_from_parts_with_help",
        "with_message",
        "with_label",
        "with_help",
        "with_file_id",
        "with_span_edit",
        "with_span_edits",
    ] {
        require_diagnostic(&builder_stderr, method)?;
    }
    drop(directory);
    Ok(())
}
