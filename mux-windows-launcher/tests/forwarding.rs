use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Installation(PathBuf);

impl Installation {
    fn new(driver: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "mux-launcher-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_mux-windows-launcher"),
            directory.join("mux.exe"),
        )
        .unwrap();
        let driver_path = directory.join("mux-driver.exe");
        let mut rustc = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args([
                "--edition",
                "2024",
                "--crate-name",
                "launcher_test_driver",
                "-o",
            ])
            .arg(&driver_path)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        rustc
            .stdin
            .take()
            .unwrap()
            .write_all(driver.as_bytes())
            .unwrap();
        let result = rustc.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        Self(directory)
    }

    fn command(&self) -> Command {
        Command::new(self.0.join("mux.exe"))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Installation {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn preserves_arguments_environment_working_directory_and_exit_status() {
    let installation = Installation::new(
        r#"fn main() {
    println!("{}", std::env::current_dir().unwrap().display());
    println!("{}", std::env::var("MUX_LAUNCHER_TEST").unwrap());
    for argument in std::env::args().skip(1) { println!("{argument}"); }
    eprintln!("driver error");
    std::process::exit(37);
}
"#,
    );
    let output = installation
        .command()
        .current_dir(&installation.0)
        .env("MUX_LAUNCHER_TEST", "inherited")
        .args(["run", "file with spaces.mux", "a\"b", "", "unicode-λ"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(37));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{}\ninherited\nrun\nfile with spaces.mux\na\"b\n\nunicode-λ\n",
            installation.0.display()
        )
    );
    assert_eq!(output.stderr, b"driver error\n");
}

#[test]
fn preserves_lsp_standard_streams_without_launcher_output() {
    let installation = Installation::new(
        "fn main() { std::io::copy(&mut std::io::stdin(), &mut std::io::stdout()).unwrap(); }",
    );
    let mut child = installation
        .command()
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = b"Content-Length: 2\r\n\r\n{}";
    child.stdin.take().unwrap().write_all(request).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, request);
    assert!(output.stderr.is_empty());
}

#[test]
fn waits_until_the_compiler_finishes() {
    let installation = Installation::new(
        r#"fn main() {
    std::fs::write(std::env::var_os("MUX_LAUNCHER_STARTED").unwrap(), []).unwrap();
    let finish = std::env::var_os("MUX_LAUNCHER_FINISH").unwrap();
    let finish = std::path::Path::new(&finish);
    for _ in 0..1000 {
        if finish.exists() { std::process::exit(37); }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::process::exit(99);
}
"#,
    );
    let started = installation.path("started");
    let finish = installation.path("finish");
    let mut child = installation
        .command()
        .arg("version")
        .env("MUX_LAUNCHER_STARTED", &started)
        .env("MUX_LAUNCHER_FINISH", &finish)
        .spawn()
        .unwrap();
    wait_for_file(&started);
    assert!(child.try_wait().unwrap().is_none());
    fs::write(finish, []).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(37));
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "driver did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn update_help_does_not_start_the_driver() {
    let installation = Installation::new("fn main() { std::process::exit(97); }");
    let output = installation
        .command()
        .args(["-di", "update", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("--check")
    );
}

#[test]
fn launcher_healthcheck_does_not_start_the_driver() {
    let installation = Installation::new("fn main() { std::process::exit(97); }");
    fs::write(installation.path(".mux-install.json"), "invalid receipt").unwrap();
    let output = installation.command().arg("__version").output().unwrap();
    assert!(output.status.success());
    let version = String::from_utf8(output.stdout).unwrap();
    assert_eq!(version.trim(), env!("MUX_COMPILER_VERSION"));
    assert!(output.stderr.is_empty());
}

#[test]
fn missing_driver_is_an_actionable_failure() {
    let installation = Installation::new("fn main() {}");
    fs::remove_file(installation.path("mux-driver.exe")).unwrap();
    let output = installation.command().arg("version").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("mux-driver.exe"), "{error}");
    assert!(error.contains("Reinstall Mux"), "{error}");
}
