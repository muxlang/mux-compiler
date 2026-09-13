use insta::assert_snapshot;
use regex::Regex;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Matches the compiler-source location embedded in a panic's internal-error
/// report, e.g. `(at mux-compiler/src/codegen/memory.rs:42:9)`. The line and
/// column shift whenever the compiler is edited, so they are masked to keep
/// panic snapshots stable.
static PANIC_LOC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(at (?P<path>[^:)]+):\d+:\d+\)").unwrap());

/// The distinctive first line of an internal-compiler-error report.
const INTERNAL_ERROR_MARKER: &str = "error: internal compiler error";

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

/// Make filesystem paths printed by executable fixtures stable across hosts.
///
/// Some fixtures intentionally exercise `fs.cwd()` and `fs.absolute()`. Their
/// values are necessarily different on every checkout, and Windows also uses
/// a native backslash separator. Normalize only the executable output used by
/// snapshots; compiler diagnostics retain their original text.
fn normalize_executable_paths(output: &str, fixture_dir: &Path) -> String {
    let fixture_dir = fixture_dir.to_string_lossy().replace('\\', "/");
    let path_prefixes = ["dirname: ", "join: ", "cwd: ", "absolute: "];
    let mut normalized = String::with_capacity(output.len());

    for chunk in output.split_inclusive('\n') {
        let (line, ending) = chunk
            .strip_suffix('\n')
            .map_or((chunk, ""), |line| (line, "\n"));
        let (line, carriage_return) = line
            .strip_suffix('\r')
            .map_or((line, ""), |line| (line, "\r"));

        if let Some(prefix) = path_prefixes
            .iter()
            .find(|prefix| line.starts_with(**prefix))
        {
            let path = line[prefix.len()..].replace('\\', "/");
            let path = if fixture_dir.is_empty() {
                path
            } else {
                path.replace(&fixture_dir, "<fixture-dir>")
            };
            normalized.push_str(prefix);
            normalized.push_str(&path);
        } else {
            normalized.push_str(line);
        }
        normalized.push_str(carriage_return);
        normalized.push_str(ending);
    }

    normalized
}

#[test]
fn executable_snapshot_normalizes_native_paths() {
    let output = "cwd: C:\\build\\mux\\test_scripts\nabsolute: C:\\build\\mux\\test_scripts\\functions.mux\n";
    let fixture_dir = Path::new(r"C:\build\mux\test_scripts");

    assert_eq!(
        normalize_executable_paths(output, fixture_dir),
        "cwd: <fixture-dir>\nabsolute: <fixture-dir>/functions.mux\n"
    );
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
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("test_executable");

    let mut exec_path = abs_dir.join(exec_name);
    if cfg!(windows) {
        exec_path.set_extension("exe");
    }

    // Clean up any existing binary before compiling
    if exec_path.exists() {
        fs::remove_file(&exec_path).unwrap_or_else(|e| {
            eprintln!(
                "Warning: Failed to clean up old executable {}: {}",
                exec_path.display(),
                e
            );
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

    println!("Compiling: {path_str}");
    let compile_output = compile_cmd
        .output()
        .unwrap_or_else(|e| panic!("Failed to execute compile command for {path_str}: {e}"));

    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    // Print compile stderr for debugging (including DEBUG lines from the compiler)
    if !compile_stderr.is_empty() {
        print!("COMPILE_STDERR: {compile_stderr}");
    }

    // Check if binary was created (indicates successful compilation)
    if !exec_path.exists() {
        // Compilation failed - return the error output
        return (String::new(), compile_stderr);
    }

    // Execute the compiled binary
    println!("Executing: {}", exec_path.display());

    // Debug: print ELF dependency/rpath details where the tools are defined.
    // macOS and Windows use different binary formats and should not probe
    // unavailable Unix commands during their acceptance runs.
    #[cfg(target_os = "linux")]
    {
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
            println!("READELF: {line}");
        }
    }

    let mut exec_cmd = Command::new(&exec_path);
    exec_cmd.current_dir(abs_dir);

    let exec_output = match exec_cmd.output() {
        Ok(output) => output,
        Err(e) => {
            // Clean up before returning
            let _ = fs::remove_file(&exec_path);
            return (String::new(), format!("Failed to execute binary: {e}"));
        }
    };

    let mut exec_stdout = String::from_utf8_lossy(&exec_output.stdout).to_string();
    let exec_stderr = String::from_utf8_lossy(&exec_output.stderr).to_string();

    // If binary exited with non-zero status, append exit status to output
    if !exec_output.status.success() {
        let _ = writeln!(
            &mut exec_stdout,
            "Program exited with status: {}",
            exec_output.status
        );
    }

    // Clean up the executable
    if exec_path.exists() {
        fs::remove_file(&exec_path).unwrap_or_else(|e| {
            eprintln!(
                "Warning: Failed to clean up executable {}: {}",
                exec_path.display(),
                e
            );
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
            } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("mux") {
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
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("unknown_file");

    let output_to_snapshot = if stderr.is_empty() {
        stdout.clone()
    } else {
        stderr.clone()
    };

    println!("Creating executable snapshot for: {snapshot_name}");

    let fixture_dir = fs::canonicalize(path)
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let normalized = normalize_executable_paths(&output_to_snapshot, &fixture_dir);
    let normalized = ipv4_re.replace_all(&normalized, "$host:PORT");
    let normalized = ipv6_re.replace_all(&normalized, "[$host]:PORT");
    let normalized = normalize_internal_error(&normalized);
    assert_snapshot!(
        format!("executable_integration__{}", snapshot_name),
        normalized
    );
}

fn run_network_test(path: &Path) {
    let (stdout, stderr) = compile_and_execute_file(path);
    assert!(
        stderr.is_empty(),
        "network fixture wrote to stderr: {stderr}"
    );
    assert!(
        !stdout.contains("Program exited with status:"),
        "network fixture failed: {stdout}"
    );
}

fn process_test_file(path: &Path, ipv4_re: &Regex, ipv6_re: &Regex) {
    let file_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("unknown");
    if is_network_fixture(file_name) && std::env::var_os("MUX_RUN_NETWORK_TESTS").is_none() {
        println!("Skipping network fixture {file_name}; set MUX_RUN_NETWORK_TESTS=1 to run it");
        return;
    }
    println!("\n=== Testing executable for file: {file_name} ===");

    match std::panic::catch_unwind(|| {
        if is_network_fixture(file_name) {
            run_network_test(path);
        } else {
            run_snapshot_test(path, ipv4_re, ipv6_re);
        }
        println!("✓ Successfully processed executable for: {file_name}");
    }) {
        Ok(()) => {}
        Err(e) => {
            println!("❌ Error processing executable for file {file_name}: {e:?}");
            panic!("Executable test failed while processing: {file_name}");
        }
    }
}

fn is_network_fixture(file_name: &str) -> bool {
    matches!(
        file_name,
        "test_std_http.mux"
            | "test_std_http_server.mux"
            | "test_std_local_net.mux"
            | "test_std_tcp.mux"
            | "test_std_udp.mux"
    )
}

#[test]
fn base32_decoder_rejects_noncanonical_padding_and_trailing_bits() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let source_path = std::env::temp_dir().join(format!(
        "mux_base32_canonical_{}_{}.mux",
        std::process::id(),
        nonce
    ));
    fs::write(
        &source_path,
        r#"import std.encoding

func main() returns void {
    match encoding.base32_decode("MY======") {
        ok(_) { print("canonical") }
        err(_) { print("rejected canonical") }
    }
    match encoding.base32_decode("MZ======") {
        ok(_) { print("accepted trailing bits") }
        err(_) { print("rejected trailing bits") }
    }
    match encoding.base32_decode("A=======") {
        ok(_) { print("accepted invalid padding") }
        err(_) { print("rejected invalid padding") }
    }
    match encoding.base64url_decode("TQ==") {
        ok(_) { print("accepted base64url padding") }
        err(_) { print("rejected base64url padding") }
    }
    match encoding.base64url_decode("+w") {
        ok(_) { print("accepted base64url standard alphabet") }
        err(_) { print("rejected base64url standard alphabet") }
    }
    return
}
"#,
    )
    .expect("temporary Mux source should be writable");

    let (stdout, stderr) = compile_and_execute_file(&source_path);
    fs::remove_file(&source_path).expect("temporary Mux source should be removable");
    assert!(stderr.is_empty(), "program wrote to stderr: {stderr}");
    assert_eq!(
        stdout,
        "canonical\nrejected trailing bits\nrejected invalid padding\nrejected base64url padding\nrejected base64url standard alphabet\n"
    );
}

#[test]
fn reference_to_temporary_in_loop_releases_each_previous_value() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let source_path =
        std::env::temp_dir().join(format!("mux_ref_loop_{}_{}.mux", std::process::id(), nonce));
    fs::write(
        &source_path,
        r"func main() returns void {
    list<int> xs = [2, 1, 5]
    for int x in xs {
        auto ref = &xs[0]
        print(ref.to_string())
    }
    return
}
",
    )
    .expect("temporary Mux source should be writable");

    let (stdout, stderr) = compile_and_execute_file(&source_path);
    fs::remove_file(&source_path).expect("temporary Mux source should be removable");

    // CI runs this test again with MUX_RUNTIME_LIB set to the rc-leak-check
    // runtime. Any box left alive then exits 101 and makes these assertions fail.
    assert!(stderr.is_empty(), "program wrote to stderr: {stderr}");
    assert_eq!(stdout, "2\n2\n2\n");
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

    let fixture_stems: Vec<_> = test_files
        .iter()
        .filter_map(|path| path.file_stem().map(std::ffi::OsStr::to_os_string))
        .collect();
    let unique_fixture_stems: std::collections::BTreeSet<_> =
        fixture_stems.iter().cloned().collect();
    assert_eq!(
        fixture_stems.len(),
        unique_fixture_stems.len(),
        "executable fixtures must have unique stems so each fixture maps to one snapshot"
    );

    let ipv4_re = Regex::new(r"(?P<host>\b(?:\d{1,3}\.){3}\d{1,3}):\d+\b").unwrap();
    let ipv6_re = Regex::new(r"\[(?P<host>[0-9a-fA-F:]+)\]:\d+\b").unwrap();

    for path in test_files {
        process_test_file(&path, &ipv4_re, &ipv6_re);
    }
}
