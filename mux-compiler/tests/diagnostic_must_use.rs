use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DIAGNOSTIC_PROBE: &str = r#"
#![deny(unused_must_use)]

use mux_lang::diagnostic::{
    diagnostic_from_parts_with_help, Diagnostic, DiagnosticCode, Files, Label, SpanEdit,
};
use mux_lang::lexer::Span;

fn main() {
    let span = Span::new(1, 1);
    let mut files = Files::new();
    let file_id = files.add("fixture.mux", "value".to_owned());

    Label::primary(span, "label");
    Diagnostic::new(DiagnosticCode::InternalCompiler);
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

struct ProbeFile {
    path: PathBuf,
    directory: Option<PathBuf>,
}

impl Drop for ProbeFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        if let Some(directory) = &self.directory {
            let _ = fs::remove_dir(directory);
        }
    }
}

#[test]
fn diagnostic_values_are_compile_time_must_use_contracts() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = package_root.join("examples");
    let directory = if examples_dir.exists() {
        None
    } else {
        fs::create_dir(&examples_dir).expect("create diagnostic probe examples directory");
        Some(examples_dir.clone())
    };
    let example_name = format!("diagnostic_must_use_probe_{}", std::process::id());
    let probe = ProbeFile {
        path: examples_dir.join(format!("{example_name}.rs")),
        directory,
    };
    fs::write(&probe.path, DIAGNOSTIC_PROBE).expect("write diagnostic must-use probe");

    let result = Command::new(cargo_executable())
        .arg("check")
        .arg("--manifest-path")
        .arg(package_root.join("Cargo.toml"))
        .arg("--example")
        .arg(example_name)
        .arg("--locked")
        .arg("--offline")
        .current_dir(package_root)
        .output()
        .expect("run cargo for diagnostic must-use probe");

    assert!(
        !result.status.success(),
        "diagnostic must-use probe unexpectedly compiled; an annotation may be missing"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("unused_must_use"),
        "diagnostic must-use probe failed for an unexpected reason:\n{stderr}"
    );
    for method in [
        "primary",
        "new",
        "diagnostic_from_parts_with_help",
        "with_message",
        "with_label",
        "with_help",
        "with_file_id",
        "with_span_edit",
        "with_span_edits",
    ] {
        assert!(
            stderr.contains(method),
            "diagnostic must-use probe did not report discarded {method} result:\n{stderr}"
        );
    }
}
