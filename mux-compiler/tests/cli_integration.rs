//! CLI-level tests for the `mux` binary's argument dispatch and error paths.
//!
//! `executable_integration` already covers the happy path (compile + run real
//! programs). This file exercises the parts of `main.rs` that the happy path
//! never reaches: subcommand dispatch (version/format), input validation, and
//! the lex/parse/semantic error branches that print diagnostics and exit
//! non-zero. The spawned binary is the llvm-cov-instrumented one, so these runs
//! count toward coverage.

use std::path::{Path, PathBuf};
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

fn runtime_library_for_child_process() -> PathBuf {
    if let Ok(path) = std::env::var("MUX_RUNTIME_LIB") {
        return PathBuf::from(path);
    }
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("compiler repository root");
    [
        repo_root.join("mux-runtime/target/debug"),
        repo_root.join("mux-runtime/target/release"),
    ]
    .into_iter()
    .find_map(|profile_dir| find_runtime_archive(&profile_dir))
    .expect("build mux-runtime or set MUX_RUNTIME_LIB before running CLI tests")
}

fn find_runtime_archive(profile_dir: &Path) -> Option<PathBuf> {
    let exact = profile_dir.join("libmux_runtime.a");
    if exact.is_file() {
        return Some(exact);
    }

    let deps_dir = profile_dir.join("deps");
    let mut candidates = std::fs::read_dir(deps_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("libmux_runtime-") && name.ends_with(".a"))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.into_iter().next()
}

#[test]
fn coverage_runner_maps_generated_test_lines_and_branches_to_source() {
    let dir = unique_tmp_dir("coverage");
    let file = write_file(
        &dir,
        "coverage.mux",
        r#"test "source mapping" {
    auto value = 1
    helper()
    if value == 1 {
        print("taken")
    } else {
        print("uncovered")
    }
}

func helper() returns void {
    print("helper")
    return
}
"#,
    );
    let output = mux()
        .args(["test", file.to_str().unwrap(), "--coverage", "--jobs", "1"])
        .current_dir(&dir)
        .env("MUX_RUNTIME_LIB", runtime_library_for_child_process())
        .output()
        .expect("spawn mux test --coverage");
    assert!(
        output.status.success(),
        "coverage runner failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report = std::fs::read_to_string(dir.join("lcov.info")).unwrap();
    let source_path = file.to_string_lossy();
    assert!(
        report.contains(&format!("SF:{source_path}\n")),
        "report: {report}"
    );
    assert!(report.contains("LF:7\nLH:6\n"), "report: {report}");
    assert!(report.contains("DA:12,1\n"), "report: {report}");
    assert!(report.contains("DA:13,1\n"), "report: {report}");
    assert!(report.contains("BRF:2\nBRH:1\n"), "report: {report}");
    assert!(report.contains("BRDA:4,0,0-1,1\n"), "report: {report}");
    assert!(report.contains("BRDA:4,0,0-0,-\n"), "report: {report}");
    assert!(
        !report.contains(".mux-test-"),
        "temporary path leaked: {report}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dynamic_interface_values_deep_copy_the_wrapped_object() {
    let dir = unique_tmp_dir("dynamic_copy");
    let file = write_file(
        &dir,
        "dynamic_copy.mux",
        r#"interface Greeter {
    func greet() returns string
}

class Alpha is Greeter {
    int count

    func greet() returns string {
        return "alpha-" + self.count.to_string()
    }
}

func erase(dyn<Greeter> value) returns dyn<Greeter> {
    return value
}

func main() returns void {
    auto alpha = Alpha.new()
    alpha.count = 1
    auto erased = erase(alpha)
    auto copied = erased
    alpha.count = 2
    print(copied.greet())
    return
}
"#,
    );
    let output_path = dir.join("dynamic_copy_output");
    let output = mux()
        .args([
            "run",
            file.to_str().unwrap(),
            "--output",
            output_path.to_str().unwrap(),
        ])
        .current_dir(&dir)
        .env("MUX_RUNTIME_LIB", runtime_library_for_child_process())
        .output()
        .expect("spawn mux run for dynamic interface copy");
    assert!(
        output.status.success(),
        "dynamic interface copy failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "alpha-1");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn version_subcommand_prints_versions() {
    let out = mux().arg("version").output().expect("spawn mux version");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("compiler v"),
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
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("E0101"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("E0202"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("E0300"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn build_reports_missing_module_with_module_code() {
    let dir = unique_tmp_dir("missing_module");
    let file = write_file(
        &dir,
        "bad.mux",
        "import missing\nfunc main() returns void { return }\n",
    );
    let out = mux()
        .args(["build", file.to_str().unwrap()])
        .output()
        .expect("spawn mux build");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("E0400"), "stderr: {stderr}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn explain_prints_registry_metadata() {
    let out = mux()
        .args(["explain", "e0302"])
        .output()
        .expect("spawn mux explain");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("E0302: type mismatch"), "stdout: {stdout}");
    assert!(stdout.contains("Fix:"), "stdout: {stdout}");
}

#[test]
fn explain_rejects_unknown_codes() {
    let out = mux()
        .args(["explain", "E9999"])
        .output()
        .expect("spawn mux explain");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Unknown diagnostic code"));
}

#[test]
fn fix_valid_source_is_a_noop_with_machine_readable_output() {
    let dir = unique_tmp_dir("fix_noop");
    let file = write_file(
        &dir,
        "program.mux",
        "func main() returns void {\n    return\n}\n",
    );
    let original = std::fs::read_to_string(&file).unwrap();
    let out = mux()
        .args([
            "fix",
            file.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn mux fix");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"status\":\"no_changes\""));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_rejects_syntax_errors_without_writing() {
    let dir = unique_tmp_dir("fix_parse");
    let file = write_file(&dir, "broken.mux", "func main( {\n    return\n}\n");
    let original = std::fs::read_to_string(&file).unwrap();
    let out = mux()
        .args(["fix", file.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("spawn mux fix");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"status\":\"error\""), "stdout: {stdout}");
    assert!(stdout.contains("E0202"), "stdout: {stdout}");
    assert!(stdout.contains("\"level\":\"error\""), "stdout: {stdout}");
    assert!(!stdout.contains("\"level\":\"Error\""), "stdout: {stdout}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_json_preserves_imported_diagnostic_file() {
    let dir = unique_tmp_dir("fix_import_file");
    write_file(
        &dir,
        "broken.mux",
        "func broken() returns int {\n    return \"wrong\"\n}\n",
    );
    let file = write_file(
        &dir,
        "program.mux",
        "import broken\n\nfunc main() returns void {\n    return\n}\n",
    );
    let out = mux()
        .args([
            "fix",
            file.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn mux fix");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("E0302"), "stdout: {stdout}");
    assert!(stdout.contains("broken.mux"), "stdout: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_reports_imported_syntax_diagnostic_with_its_file() {
    let dir = unique_tmp_dir("fix_import_parse");
    write_file(&dir, "broken.mux", "func broken( {\n    return\n}\n");
    let file = write_file(
        &dir,
        "program.mux",
        "import broken\n\nfunc main() returns void {\n    return\n}\n",
    );
    let out = mux()
        .args([
            "fix",
            file.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn mux fix");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("E0202"), "stdout: {stdout}");
    assert!(stdout.contains("broken.mux"), "stdout: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_continues_after_recoverable_syntax_error() {
    let dir = unique_tmp_dir("fix_recovery");
    let file = write_file(
        &dir,
        "program.mux",
        "func broken( {\n    return\n}\n\nfunc independent() returns void {\n    print(missing_name)\n    return\n}\n",
    );
    let out = mux()
        .args([
            "fix",
            file.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn mux fix");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("E0202"), "stdout: {stdout}");
    assert!(stdout.contains("E0300"), "stdout: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_reports_provable_warnings_without_failing_valid_source() {
    let dir = unique_tmp_dir("fix_warning");
    let file = write_file(
        &dir,
        "program.mux",
        "func main() returns void {\n    if true {\n        return\n    } else {\n        return\n    }\n}\n",
    );
    let out = mux()
        .args([
            "fix",
            file.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn mux fix");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"code\":\"W0305\""), "stdout: {stdout}");
    assert!(stdout.contains("\"level\":\"warning\""), "stdout: {stdout}");
    assert!(
        stdout.contains("\"status\":\"no_changes\""),
        "stdout: {stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_applies_a_proven_safe_boolean_simplification() {
    let dir = unique_tmp_dir("fix_boolean");
    let file = write_file(
        &dir,
        "program.mux",
        "func main() returns void {\n    auto value = true\n    if 1 == 1 {\n        print(\"constant\")\n    } else {\n        print(\"never\")\n    }\n    if value && true {\n        return\n    } else {\n        return\n    }\n}\n",
    );
    let out = mux()
        .args(["fix", file.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("spawn mux fix");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"status\":\"applied\""),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"applicability\":\"machine-applicable\""),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("W0305"), "stdout: {stdout}");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("if value {")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_does_not_call_a_breaking_loop_unreachable() {
    let dir = unique_tmp_dir("fix_break");
    let file = write_file(
        &dir,
        "program.mux",
        "func main() returns void {\n    while true {\n        break\n    }\n    return\n}\n",
    );
    let out = mux()
        .args([
            "fix",
            file.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn mux fix");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("W0305"), "stdout: {stdout}");
    assert!(!stdout.contains("W0302"), "stdout: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_deny_warnings_does_not_write_safe_edits() {
    let dir = unique_tmp_dir("fix_deny_warning");
    let file = write_file(
        &dir,
        "program.mux",
        "func main() returns void {\n    auto value = true\n    if value && true {\n        return\n    } else {\n        return\n    }\n}\n",
    );
    let original = std::fs::read_to_string(&file).unwrap();
    let out = mux()
        .args([
            "--deny-warnings",
            "fix",
            file.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("spawn mux fix");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("W0306"), "stdout: {stdout}");
    assert!(stdout.contains("\"status\":\"error\""), "stdout: {stdout}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    std::fs::remove_dir_all(&dir).ok();
}

/// Building the same file twice must produce byte-identical LLVM IR.
///
/// Codegen walks several maps to decide what to emit and in what order -
/// function nodes for monomorphization, module ASTs for the compilation unit,
/// a lambda's captured variables for its environment layout. When those were
/// `HashMap`s, Rust's per-process hash seed randomization made the emitted IR
/// differ between back-to-back builds of the same file (issue #344). Nothing
/// asserted on IR text, so it went unnoticed.
///
/// The program below exercises each path at once: a generic class with methods
/// to monomorphize, a closure with several captures, a class whose invariant
/// binds several fields (their GEPs were emitted in hash order), and imports
/// from two modules - without those the module maps are empty and their
/// ordering is never tested, so a revert there would pass unnoticed.
///
/// Eight builds rather than two, because the field-order case flipped a coin
/// per build - four builds missed it often enough to be useless as a gate.
#[test]
fn repeated_builds_emit_identical_ir() {
    let dir = unique_tmp_dir("determinism");
    write_file(
        &dir,
        "det_alpha.mux",
        r"
func alpha_one(int n) returns int {
    return n + 1
}

func alpha_two(int n) returns int {
    return n + 2
}
",
    );
    write_file(
        &dir,
        "det_beta.mux",
        r"
func beta_one(int n) returns int {
    return n * 2
}

func beta_two(int n) returns int {
    return n * 3
}
",
    );
    let src = write_file(
        &dir,
        "determinism.mux",
        r#"
import det_alpha
import det_beta
class Box<T> {
    T value

    common func from(T val) returns Box<T> {
        auto b = Box<T>.new()
        b.value = val
        return b
    }

    func get() returns T {
        return self.value
    }
}

class Server {
    string host = "localhost" where { host.length() < 64 }
    int port = 8080 where { port > 0, port < 65535 }
    int backlog = 16 where { backlog > 0 }

    func describe() returns string {
        return self.host + ":" + self.port.to_string()
    }
} where {
    port != 22
}

func main() returns void {
    print(det_alpha.alpha_one(1).to_string())
    print(det_alpha.alpha_two(1).to_string())
    print(det_beta.beta_one(2).to_string())
    print(det_beta.beta_two(2).to_string())

    auto s = Server.new()
    print(s.describe())

    auto a = Box<int>.from(1)
    auto b = Box<string>.from("two")
    auto c = Box<bool>.from(true)
    print(a.get().to_string())
    print(b.get())
    print(c.get().to_string())

    auto first = 1
    auto second = 2
    auto third = 3
    auto sum = func() returns int {
        return first + second + third
    }
    print(sum().to_string())
    return
}
"#,
    );
    let ir = dir.join("determinism.ll");

    let build_once = || {
        let out = mux()
            .arg("build")
            .arg("--intermediate")
            .arg(&src)
            .arg("-o")
            .arg(dir.join("determinism_bin"))
            .env("MUX_RUNTIME_LIB", runtime_library_for_child_process())
            .output()
            .expect("spawn mux build");
        assert!(
            out.status.success(),
            "build failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read(&ir).expect("intermediate .ll should exist")
    };

    let first = build_once();
    for run in 2..=8 {
        assert!(
            build_once() == first,
            "run {run} emitted different IR than run 1; codegen order is not deterministic (issue #344)"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
