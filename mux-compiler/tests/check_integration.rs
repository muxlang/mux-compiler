//! Frontend-only CLI behavior and parity with native compilation diagnostics.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mux_check_{}_{}", std::process::id(), id));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn write(&self, name: &str, source: &str) -> std::io::Result<PathBuf> {
        let path = self.0.join(name);
        std::fs::write(&path, source)?;
        Ok(path)
    }

    fn run(&self, args: &[&str]) -> std::io::Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_mux"))
            .args(args)
            .current_dir(&self.0)
            .env("NO_COLOR", "1")
            .output()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Cleanup must not trigger a second panic if a test is already failing.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_status(output: &Output, status: i32) {
    assert_eq!(
        output.status.code(),
        Some(status),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_accepts_libraries_and_existing_generic_class_programs() -> TestResult {
    let workspace = Workspace::new()?;
    workspace.write("library.mux", "func value() returns int { return 42 }\n")?;
    let output = workspace.run(&["check", "library.mux"])?;
    assert_status(&output, 0);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("compiler manifest directory has no parent")?;
    for fixture in [
        "generic_bounds.mux",
        "generic_class_capabilities.mux",
        "class_scalar_fields.mux",
    ] {
        let file = root.join("test_scripts").join(fixture);
        let file = file.to_str().ok_or("fixture path is not valid UTF-8")?;
        assert_status(&workspace.run(&["check", file])?, 0);
    }
    Ok(())
}

#[test]
fn check_errors_match_build_diagnostics() -> TestResult {
    let workspace = Workspace::new()?;
    for source in [
        "func main( {\n",
        "func main() returns void { int value = \"wrong\" }\n",
        "import missing_module\nfunc main() returns void { return }\n",
    ] {
        workspace.write("bad.mux", source)?;
        let checked = workspace.run(&["check", "bad.mux"])?;
        let built = workspace.run(&["build", "bad.mux"])?;
        assert_status(&checked, 1);
        assert_status(&built, 1);
        assert_eq!(checked.stderr, built.stderr);
        assert_eq!(checked.stdout, built.stdout);
    }
    Ok(())
}

#[test]
fn check_reports_errors_in_imported_modules() -> TestResult {
    let workspace = Workspace::new()?;
    workspace.write(
        "main.mux",
        "import library\nfunc main() returns void { return }\n",
    )?;
    for source in [
        "func value( {\n",
        "func value() returns int { return \"wrong\" }\n",
        "import missing_module\nfunc value() returns int { return 42 }\n",
    ] {
        workspace.write("library.mux", source)?;
        let checked = workspace.run(&["check", "main.mux"])?;
        let built = workspace.run(&["build", "main.mux"])?;
        assert_status(&checked, 1);
        assert_status(&built, 1);
        assert_eq!(checked.stderr, built.stderr);
        assert!(String::from_utf8_lossy(&checked.stderr).contains("library.mux"));
    }
    Ok(())
}

#[test]
fn check_preserves_warnings_and_denies_them_on_request() -> TestResult {
    let workspace = Workspace::new()?;
    workspace.write(
        "warning.mux",
        "func main() returns void { if true { return } else { return } }\n",
    )?;
    let warning = workspace.run(&["check", "warning.mux"])?;
    assert_status(&warning, 0);
    assert!(String::from_utf8_lossy(&warning.stderr).contains("W0305"));
    for args in [
        vec!["--deny-warnings", "check", "warning.mux"],
        vec!["check", "warning.mux", "--deny-warnings"],
    ] {
        let denied = workspace.run(&args)?;
        assert_status(&denied, 1);
        assert_eq!(warning.stderr, denied.stderr);
    }
    let built = workspace.run(&["build", "warning.mux", "--deny-warnings"])?;
    assert_status(&built, 1);
    assert_eq!(warning.stderr, built.stderr);
    Ok(())
}

#[test]
fn check_rejects_invalid_arguments_and_input() -> TestResult {
    let workspace = Workspace::new()?;
    workspace.write("valid.mux", "func main() returns void { return }\n")?;
    for args in [
        vec!["check"],
        vec!["check", "valid.mux", "valid.mux"],
        vec!["check", "valid.mux", "-o", "out"],
        vec!["check", "valid.mux", "--output", "out"],
        vec!["check", "valid.mux", "-i"],
        vec!["check", "valid.mux", "--intermediate"],
        vec!["-o", "out", "check", "valid.mux"],
        vec!["--output", "out", "check", "valid.mux"],
        vec!["-i", "check", "valid.mux"],
        vec!["--intermediate", "check", "valid.mux"],
        vec!["check", "valid.mux", "--json"],
    ] {
        assert_status(&workspace.run(&args)?, 2);
    }
    for file in ["missing.mux", ".", "-", "file.txt"] {
        assert_status(&workspace.run(&["check", file])?, 1);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn check_reads_imports_without_runtime_linker_or_filesystem_writes() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let workspace = Workspace::new()?;
    workspace.write("library.mux", "func value() returns int { return 42 }\n")?;
    workspace.write(
        "main.mux",
        "import library\nfunc main() returns void {\n    print(library.value().to_string())\n    return\n}\n",
    )?;
    let trap = workspace.write(
        "cc",
        "#!/bin/sh\necho invoked > linker-was-called\nexit 99\n",
    )?;
    std::fs::set_permissions(&trap, std::fs::Permissions::from_mode(0o755))?;
    for name in ["clang", "clang-22", "gcc"] {
        std::fs::copy(&trap, workspace.0.join(name))?;
    }
    let before = std::fs::read_dir(&workspace.0)?.count();
    for entry in std::fs::read_dir(&workspace.0)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "mux") {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))?;
        }
    }
    std::fs::set_permissions(&workspace.0, std::fs::Permissions::from_mode(0o555))?;
    let output = Command::new(env!("CARGO_BIN_EXE_mux"))
        .args(["check", "main.mux"])
        .current_dir(&workspace.0)
        .env("MUX_RUNTIME_LIB", workspace.0.join("nonexistent-runtime"))
        .env("CC", &trap)
        .env("PATH", &workspace.0)
        .env("TMPDIR", &workspace.0)
        .output();
    // Restore permissions even when launching the compiler fails.
    std::fs::set_permissions(&workspace.0, std::fs::Permissions::from_mode(0o755))?;
    let output = output?;
    assert_status(&output, 0);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(before, std::fs::read_dir(&workspace.0)?.count());
    assert_eq!(
        std::fs::read_to_string(workspace.0.join("library.mux"))?,
        "func value() returns int { return 42 }\n"
    );
    Ok(())
}
