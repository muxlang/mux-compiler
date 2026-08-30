use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const STYLES_PROBE: &str = r#"
#![deny(unused_must_use)]

use mux_lang::diagnostic::{ColorConfig, Styles};

fn main() {
    Styles::new(ColorConfig::Auto);

    let styles = Styles::new(ColorConfig::Auto);
    styles.error("error");
    styles.warning("warning");
    styles.help("help");
    styles.bold("bold");
    styles.location("location");
    styles.primary_label("primary");
    styles.secondary_label("secondary");
    styles.line_number("line");
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
fn styles_results_are_compile_time_must_use_contracts() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = package_root.join("examples");
    let directory = if examples_dir.exists() {
        None
    } else {
        fs::create_dir(&examples_dir).expect("create must-use probe examples directory");
        Some(examples_dir.clone())
    };
    let example_name = format!("styles_must_use_probe_{}", std::process::id());
    let probe = ProbeFile {
        path: examples_dir.join(format!("{example_name}.rs")),
        directory,
    };
    fs::write(&probe.path, STYLES_PROBE).expect("write must-use probe");

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
        .expect("run cargo for must-use probe");

    assert!(
        !result.status.success(),
        "must-use probe unexpectedly compiled; a #[must_use] annotation may be missing"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("unused_must_use"),
        "must-use probe failed for an unexpected reason:\n{stderr}"
    );
}
