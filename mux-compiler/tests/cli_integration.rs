//! CLI-level tests for the `mux` binary's argument dispatch and error paths.
//!
//! `executable_integration` already covers the happy path (compile + run real
//! programs). This file exercises the parts of `main.rs` that the happy path
//! never reaches: subcommand dispatch (version/format), input validation, and
//! the lex/parse/semantic error branches that print diagnostics and exit
//! non-zero. The spawned binary is the llvm-cov-instrumented one, so these runs
//! count toward coverage.

use std::path::PathBuf;
use std::process::Command;

fn mux() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mux"))
}

fn unique_tmp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("mux_cli_{}_{}_{}", tag, std::process::id(), nanos));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &std::path::Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn version_subcommand_prints_versions() {
    let out = mux().arg("version").output().expect("spawn mux version");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("mux version"),
        "unexpected version output: {stdout}"
    );
}

#[test]
fn help_flag_succeeds() {
    let out = mux().arg("--help").output().expect("spawn mux --help");
    assert!(out.status.success());
}

#[test]
fn no_arguments_is_a_usage_error() {
    // clap exits non-zero (usage error) when no subcommand is given.
    let out = mux().output().expect("spawn mux");
    assert!(!out.status.success());
}

#[test]
fn format_subcommand_reports_not_implemented() {
    let out = mux()
        .args(["format", "whatever.mux"])
        .output()
        .expect("spawn mux format");
    assert!(!out.status.success(), "format stub must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("formatting is not yet implemented"));
}

#[test]
fn build_rejects_non_mux_extension() {
    let dir = unique_tmp_dir("ext");
    let file = write_file(&dir, "program.txt", "func main() returns void { return }\n");
    let out = mux()
        .args(["build", file.to_str().unwrap()])
        .output()
        .expect("spawn mux build");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(".mux extension"), "stderr: {stderr}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn build_reports_missing_file() {
    let dir = unique_tmp_dir("missing");
    let missing = dir.join("nope.mux");
    let out = mux()
        .args(["build", missing.to_str().unwrap()])
        .output()
        .expect("spawn mux build");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Error opening file"), "stderr: {stderr}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn build_reports_lex_error() {
    let dir = unique_tmp_dir("lex");
    // Unterminated string literal -> lexer error.
    let file = write_file(
        &dir,
        "bad.mux",
        "func main() returns void {\n    print(\"unterminated)\n}\n",
    );
    let out = mux()
        .args(["build", file.to_str().unwrap()])
        .output()
        .expect("spawn mux build");
    assert!(!out.status.success());
    assert!(!out.stderr.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn build_reports_parse_error() {
    let dir = unique_tmp_dir("parse");
    // Malformed function header -> parser error.
    let file = write_file(&dir, "bad.mux", "func main( {\n    return\n}\n");
    let out = mux()
        .args(["build", file.to_str().unwrap()])
        .output()
        .expect("spawn mux build");
    assert!(!out.status.success());
    assert!(!out.stderr.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn build_reports_semantic_error() {
    let dir = unique_tmp_dir("sem");
    // Reference to an undefined variable -> semantic error.
    let file = write_file(
        &dir,
        "bad.mux",
        "func main() returns void {\n    print(undefined_variable.to_string())\n    return\n}\n",
    );
    let out = mux()
        .args(["build", file.to_str().unwrap()])
        .output()
        .expect("spawn mux build");
    assert!(!out.status.success());
    assert!(!out.stderr.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}
