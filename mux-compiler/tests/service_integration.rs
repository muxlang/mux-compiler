use std::collections::BTreeMap;
use std::fs;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn service_tests_enabled() -> bool {
    std::env::var("MUX_RUN_SERVICE_TESTS").is_ok_and(|value| value == "1")
}

fn required_env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("missing required environment variable: {key}"))
}

fn resolve_ipv4_addr(addr: &str) -> String {
    addr.to_socket_addrs()
        .unwrap_or_else(|e| panic!("failed to resolve {addr}: {e}"))
        .find(std::net::SocketAddr::is_ipv4)
        .unwrap_or_else(|| panic!("no IPv4 address found for {addr}"))
        .to_string()
}

fn integration_target_dir() -> PathBuf {
    PathBuf::from("target/service-integration")
}

/// Build the runtime staticlib into this suite's own target directory.
///
/// These tests compile through `cargo run` with a separate `CARGO_TARGET_DIR`,
/// so the compiler they build looks for `libmux_runtime.a` beside itself in
/// that directory. Cargo emits only a dependency's rlib, so without this the
/// archive is never produced there and every program fails to link with
/// undefined references to core runtime symbols.
fn ensure_runtime_built(repo_root: &Path) {
    static BUILT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| {
        let output = Command::new("cargo")
            .args(["build", "--quiet", "-p", "mux-runtime"])
            .current_dir(repo_root)
            .env("CARGO_TARGET_DIR", integration_target_dir())
            .output()
            .unwrap_or_else(|e| panic!("failed to build mux-runtime: {e}"));

        assert!(
            output.status.success(),
            "failed to build mux-runtime for the service suite:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    });
}

fn compile_and_execute_file(
    repo_root: &Path,
    test_file: &Path,
    envs: &BTreeMap<&str, String>,
) -> (String, String) {
    let abs_path = fs::canonicalize(test_file).unwrap_or_else(|e| {
        panic!(
            "Failed to get absolute path for {}: {}",
            test_file.display(),
            e
        )
    });
    let path_str = abs_path.to_string_lossy();
    let abs_dir = abs_path.parent().unwrap_or_else(|| Path::new("."));

    let exec_name = test_file
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("test_executable");
    let exec_path = abs_dir.join(exec_name);

    if exec_path.exists() {
        fs::remove_file(&exec_path).unwrap_or_else(|e| {
            panic!(
                "Failed to clean up old executable {}: {}",
                exec_path.display(),
                e
            )
        });
    }

    ensure_runtime_built(repo_root);

    let mut compile_cmd = Command::new("cargo");
    compile_cmd
        .args([
            "run",
            "--quiet",
            "--manifest-path",
            "mux-compiler/Cargo.toml",
            "--bin",
            "mux",
            "--",
            "build",
            &path_str,
        ])
        .current_dir(repo_root)
        .env("CARGO_TARGET_DIR", integration_target_dir())
        .env("RUST_BACKTRACE", "1");

    let compile_output = compile_cmd
        .output()
        .unwrap_or_else(|e| panic!("Failed to execute compile command for {path_str}: {e}"));

    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    if !exec_path.exists() {
        return (String::new(), compile_stderr);
    }

    let mut exec_cmd = Command::new(&exec_path);
    exec_cmd.current_dir(abs_dir);
    for (key, value) in envs {
        exec_cmd.env(key, value);
    }

    let exec_output = match exec_cmd.output() {
        Ok(output) => output,
        Err(e) => {
            let _ = fs::remove_file(&exec_path);
            return (String::new(), format!("Failed to execute binary: {e}"));
        }
    };

    let stdout = String::from_utf8_lossy(&exec_output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&exec_output.stderr).to_string();

    fs::remove_file(&exec_path).unwrap_or_else(|e| {
        panic!(
            "Failed to clean up executable {}: {}",
            exec_path.display(),
            e
        )
    });

    if exec_output.status.success() {
        (stdout, stderr)
    } else {
        (
            stdout,
            format!(
                "{}Program exited with status: {}",
                stderr, exec_output.status
            ),
        )
    }
}

fn run_service_script(script_name: &str) -> (String, String) {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root.parent().unwrap_or_else(|| {
        panic!(
            "failed to resolve repository root from crate dir: {}",
            crate_root.display()
        )
    });
    let script_path = repo_root
        .join("scripts")
        .join("integration_scripts")
        .join(script_name);
    let envs = BTreeMap::from([
        (
            "MUX_TEST_POSTGRES_URL",
            required_env("MUX_TEST_POSTGRES_URL"),
        ),
        ("MUX_TEST_HTTP_URL", required_env("MUX_TEST_HTTP_URL")),
        ("MUX_TEST_TCP_ADDR", required_env("MUX_TEST_TCP_ADDR")),
        (
            "MUX_TEST_UDP_ADDR",
            resolve_ipv4_addr(&required_env("MUX_TEST_UDP_ADDR")),
        ),
    ]);
    compile_and_execute_file(repo_root, &script_path, &envs)
}

#[test]
fn test_postgres_service_script() {
    if !service_tests_enabled() {
        eprintln!("Skipping service integration tests; set MUX_RUN_SERVICE_TESTS=1 to enable.");
        return;
    }

    let (stdout, stderr) = run_service_script("test_std_sql_postgres.mux");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert!(
        stdout.contains("postgres integration test passed"),
        "missing success marker in stdout: {stdout}"
    );
}

#[test]
fn test_http_service_script() {
    if !service_tests_enabled() {
        eprintln!("Skipping service integration tests; set MUX_RUN_SERVICE_TESTS=1 to enable.");
        return;
    }

    let (stdout, stderr) = run_service_script("test_std_http_external.mux");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert!(
        stdout.contains("external http integration test passed"),
        "missing success marker in stdout: {stdout}"
    );
}

#[test]
fn test_tcp_service_script() {
    if !service_tests_enabled() {
        eprintln!("Skipping service integration tests; set MUX_RUN_SERVICE_TESTS=1 to enable.");
        return;
    }

    let (stdout, stderr) = run_service_script("test_std_tcp_external.mux");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert!(
        stdout.contains("external tcp integration test passed"),
        "missing success marker in stdout: {stdout}"
    );
}

#[test]
fn test_udp_service_script() {
    if !service_tests_enabled() {
        eprintln!("Skipping service integration tests; set MUX_RUN_SERVICE_TESTS=1 to enable.");
        return;
    }

    let (stdout, stderr) = run_service_script("test_std_udp_external.mux");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert!(
        stdout.contains("external udp integration test passed"),
        "missing success marker in stdout: {stdout}"
    );
}
