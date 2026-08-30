use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const CATALOG_PROBE: &str = r#"
#![deny(unused_must_use)]

use mux_lang::diagnostic::DiagnosticCode;

fn main() {
    DiagnosticCode::all();
    DiagnosticCode::reserved();
    DiagnosticCode::LexUnexpectedCharacter.as_str();
    DiagnosticCode::LexUnexpectedCharacter.level();
    DiagnosticCode::LexUnexpectedCharacter.info();
    DiagnosticCode::parse("E0100");
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
fn diagnostic_catalog_results_are_compile_time_must_use_contracts() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = package_root.join("examples");
    let directory = if examples_dir.exists() {
        None
    } else {
        fs::create_dir(&examples_dir).expect("create catalog probe examples directory");
        Some(examples_dir.clone())
    };
    let example_name = format!("catalog_must_use_probe_{}", std::process::id());
    let probe = ProbeFile {
        path: examples_dir.join(format!("{example_name}.rs")),
        directory,
    };
    fs::write(&probe.path, CATALOG_PROBE).expect("write catalog must-use probe");

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
        .expect("run cargo for catalog must-use probe");

    assert!(
        !result.status.success(),
        "catalog must-use probe unexpectedly compiled; an annotation may be missing"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("unused_must_use"),
        "catalog must-use probe failed for an unexpected reason:\n{stderr}"
    );
    for method in ["all", "reserved", "as_str", "level", "info", "parse"] {
        assert!(
            stderr.contains(method),
            "catalog must-use probe did not report discarded {method} result:\n{stderr}"
        );
    }
}
