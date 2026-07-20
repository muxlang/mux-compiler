use insta::assert_snapshot;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

/// Matches the compiler-source location embedded in a panic's internal-error
/// report, e.g. `(at mux-compiler/src/codegen/memory.rs:42:9)`. The line and
/// column shift whenever the compiler is edited, so they are masked to keep
/// panic snapshots stable.
static PANIC_LOC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(at (?P<path>[^:)]+):\d+:\d+\)").unwrap());

/// The distinctive first line of an internal-compiler-error report.
const INTERNAL_ERROR_MARKER: &str = "error: internal compiler error - this is a bug in mux";

/// Make an internal-compiler-error report deterministic. A compiler panic runs
/// under `RUST_BACKTRACE=1` (set by `compile_and_execute_file`), so the default
/// panic hook prints a non-deterministic backtrace before the friendly report;
/// keep only the report (from its marker onward). Then mask the compiler-source
/// location so a panic snapshot does not churn on every recompile. Output with no
/// such report is returned unchanged.
fn normalize_internal_error(output: &str) -> String {
    let report = match output.find(INTERNAL_ERROR_MARKER) {
        Some(idx) => &output[idx..],
        None => output,
    };
    PANIC_LOC_RE
        .replace_all(report, "(at $path:LINE:COL)")
        .into_owned()
}

fn compile_and_execute_file(test_file: &Path) -> (String, String) {
    let abs_path = fs::canonicalize(test_file).unwrap_or_else(|e| {
        panic!(
            "Failed to get absolute path for {}: {}",
            test_file.display(),
            e
        )
    });
    let path_str = abs_path.to_string_lossy();

    // Use absolute path's directory for exec_path (where binary is created)
    let abs_dir = abs_path.parent().unwrap_or_else(|| Path::new("."));

    let exec_name = test_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("test_executable");

    let exec_path = abs_dir.join(exec_name);

    // Clean up any existing binary before compiling
    if exec_path.exists() {
        fs::remove_file(&exec_path).unwrap_or_else(|e| {
            eprintln!(
                "Warning: Failed to clean up old executable {}: {}",
                exec_path.display(),
                e
            )
        });
    }

    // Compile the file using the already-built mux binary (build, not run).
    // Using CARGO_BIN_EXE_mux instead of `cargo run` avoids paying cargo's
    // fingerprint/dependency-check overhead on every single test file.
    let mux_bin = env!("CARGO_BIN_EXE_mux");
    let mut compile_cmd = Command::new(mux_bin);
    compile_cmd
        .args(["build", &path_str])
        .current_dir("../")
        .env("RUST_BACKTRACE", "1");

    println!("Compiling: {}", path_str);
    let compile_output = compile_cmd
        .output()
        .unwrap_or_else(|e| panic!("Failed to execute compile command for {}: {}", path_str, e));

    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    // Print compile stderr for debugging (including DEBUG lines from the compiler)
    if !compile_stderr.is_empty() {
        print!("COMPILE_STDERR: {}", compile_stderr);
    }

    // Check if binary was created (indicates successful compilation)
    if !exec_path.exists() {
        // Compilation failed - return the error output
        return (String::new(), compile_stderr);
    }

    // Execute the compiled binary
    println!("Executing: {}", exec_path.display());

    // Debug: print ldd output and check rpath
    let ldd_out = Command::new("ldd")
        .arg(&exec_path)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    println!("LDD for {}:\n{}", exec_path.display(), ldd_out);
    let readelf_out = Command::new("readelf")
        .args(["-d", &exec_path.to_string_lossy()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    for line in readelf_out
        .lines()
        .filter(|l| l.contains("PATH") || l.contains("NEEDED"))
    {
        println!("READELF: {}", line);
    }

    let mut exec_cmd = Command::new(&exec_path);
    exec_cmd.current_dir(abs_dir);

    let exec_output = match exec_cmd.output() {
        Ok(output) => output,
        Err(e) => {
            // Clean up before returning
            let _ = fs::remove_file(&exec_path);
            return (String::new(), format!("Failed to execute binary: {}", e));
        }
    };

    let mut exec_stdout = String::from_utf8_lossy(&exec_output.stdout).to_string();
    let exec_stderr = String::from_utf8_lossy(&exec_output.stderr).to_string();

    // If binary exited with non-zero status, append exit status to output
    if !exec_output.status.success() {
        exec_stdout.push_str(&format!(
            "Program exited with status: {}\n",
            exec_output.status
        ));
    }

    // Clean up the executable
    if exec_path.exists() {
        fs::remove_file(&exec_path).unwrap_or_else(|e| {
            eprintln!(
                "Warning: Failed to clean up executable {}: {}",
                exec_path.display(),
                e
            )
        });
    }

    (exec_stdout, exec_stderr)
}

fn collect_mux_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_mux_files(&path));
            } else if path.extension().and_then(|s| s.to_str()) == Some("mux") {
                files.push(path);
            }
        }
    }
    files
}

fn run_snapshot_test(path: &Path, ipv4_re: &Regex, ipv6_re: &Regex) {
    println!("Compiling and executing file: {}", path.display());
    let (stdout, stderr) = compile_and_execute_file(path);

    let snapshot_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown_file");

    let output_to_snapshot = if stderr.is_empty() {
        stdout.clone()
    } else {
        stderr.clone()
    };

    println!("Creating executable snapshot for: {}", snapshot_name);

    let normalized = ipv4_re.replace_all(&output_to_snapshot, "$host:PORT");
    let normalized = ipv6_re.replace_all(&normalized, "[$host]:PORT");
    let normalized = normalize_internal_error(&normalized);
    assert_snapshot!(
        format!("executable_integration__{}", snapshot_name),
        normalized
    );
}

fn process_test_file(path: &Path, ipv4_re: &Regex, ipv6_re: &Regex) {
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    println!("\n=== Testing executable for file: {} ===", file_name);

    match std::panic::catch_unwind(|| {
        run_snapshot_test(path, ipv4_re, ipv6_re);
        println!("✓ Successfully processed executable for: {}", file_name);
    }) {
        Ok(_) => {}
        Err(e) => {
            println!(
                "❌ Error processing executable for file {}: {:?}",
                file_name, e
            );
            panic!("Executable test failed while processing: {}", file_name);
        }
    }
}

#[test]
fn test_executable_all_mux_files_in_dir() {
    let test_dir = "../test_scripts";
    let dir_path = PathBuf::from(&test_dir);

    if !dir_path.exists() {
        panic!("Test scripts directory not found: {}", dir_path.display());
    }

    println!(
        "Scanning directory for executable tests: {}",
        dir_path.display()
    );

    let mut test_files = collect_mux_files(&dir_path);
    test_files.sort();

    let ipv4_re = Regex::new(r"(?P<host>\b(?:\d{1,3}\.){3}\d{1,3}):\d+\b").unwrap();
    let ipv6_re = Regex::new(r"\[(?P<host>[0-9a-fA-F:]+)\]:\d+\b").unwrap();

    for path in test_files {
        process_test_file(&path, &ipv4_re, &ipv6_re);
    }
}
