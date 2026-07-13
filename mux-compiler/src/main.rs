mod ast;
mod build_config;
mod codegen;
mod diagnostic;
mod embedded_std {
    include!(concat!(env!("OUT_DIR"), "/embedded_std.rs"));
}
mod lexer;
mod module_resolver;
mod parser;
mod semantics;
mod source;
mod spinner;

use anstream::{eprintln, println, stdout};
use anstyle::AnsiColor;
use clap::{Parser as ClapParser, Subcommand};
use diagnostic::{ColorConfig, DiagnosticEmitter, FileId, Files, StandardEmitter, ToDiagnostic};
use module_resolver::ModuleResolver;
use source::Source;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::rc::Rc;

const REQUIRED_LLVM_MAJOR: u32 = 22;

/// Styling for clap's generated help output (mirrors cargo's palette).
const HELP_STYLES: clap::builder::styling::Styles = clap::builder::styling::Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Cyan.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default());

fn emit_diagnostics<E: ToDiagnostic>(files: &Files, file_id: FileId, errors: &[E]) {
    // Clear any in-progress spinner line before printing diagnostics.
    spinner::stop();
    let emitter = StandardEmitter::new(ColorConfig::Auto);
    let diagnostics: Vec<_> = errors.iter().map(|e| e.to_diagnostic(file_id)).collect();
    emitter.emit_batch(&diagnostics, files);
}

/// Mux compiler CLI
#[derive(ClapParser)]
#[command(name = "mux")]
#[command(about = "CLI tool for Mux Programming Language", long_about = None)]
#[command(styles = HELP_STYLES)]
struct Cli {
    /// Name of the output executable
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Emit intermediate LLVM IR (.ll)
    #[arg(short, long)]
    intermediate: bool,

    /// The command to run
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile a Mux file without running it
    Build {
        file: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long)]
        intermediate: bool,
    },
    /// Compile and run a Mux file
    Run {
        file: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long)]
        intermediate: bool,
    },
    /// Format a Mux file
    Format { file: PathBuf },
    /// Check system dependencies for the Mux compiler
    Doctor {
        /// Validate contributor toolchain requirements (LLVM 22)
        #[arg(long)]
        dev: bool,
    },
    /// Print the Mux version
    Version {},
}

fn find_clang_command() -> Option<String> {
    if let Ok(cc) = env::var("CC") {
        let output = Command::new(&cc).arg("--version").output();
        if output.is_ok_and(|o| o.status.success()) {
            return Some(cc);
        }
    }

    let linked_major = env!("MUX_LLVM_MAJOR");
    let versioned = format!("clang-{}", linked_major);
    let candidates: &[&str] = &[versioned.as_str(), "clang"];
    for candidate in candidates {
        let output = match Command::new(candidate).arg("--version").output() {
            Ok(output) => output,
            Err(_) => continue,
        };
        if output.status.success() {
            return Some(candidate.to_string());
        }
    }
    None
}

fn llvm_config_candidates() -> Vec<String> {
    let mut candidates = Vec::new();

    if let Ok(prefix) = env::var("LLVM_SYS_221_PREFIX") {
        let from_prefix = PathBuf::from(prefix).join("bin").join("llvm-config");
        if from_prefix.exists() {
            candidates.push(from_prefix.to_string_lossy().to_string());
        }
    }

    if let Ok(path) = env::var("LLVM_CONFIG") {
        candidates.push(path);
    }

    candidates.push(format!("llvm-config-{}", REQUIRED_LLVM_MAJOR));
    candidates.push("llvm-config".to_string());

    candidates
}

fn collect_llvm_versions() -> Vec<(String, String, u32)> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();

    for candidate in llvm_config_candidates() {
        if !seen.insert(candidate.clone()) {
            continue;
        }

        let output = match Command::new(&candidate).arg("--version").output() {
            Ok(output) => output,
            Err(_) => continue,
        };
        if !output.status.success() {
            continue;
        }

        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let major_str = raw.split('.').next().unwrap_or("");
        let major = match major_str.parse::<u32>() {
            Ok(major) => major,
            Err(_) => continue,
        };
        found.push((candidate, raw, major));
    }

    found
}

fn pick_llvm_for_dev(versions: &[(String, String, u32)]) -> Option<(String, String, u32)> {
    versions
        .iter()
        .find(|(_, _, major)| *major == REQUIRED_LLVM_MAJOR)
        .map(|(tool, version, major)| (tool.clone(), version.clone(), *major))
}

fn print_llvm_install_help() {
    if cfg!(target_os = "linux") {
        println!("Install LLVM {} and clang:", REQUIRED_LLVM_MAJOR);
        println!(
            "  Ubuntu/Debian: sudo apt-get install llvm-{0}-dev clang-{0} libpolly-{0}-dev",
            REQUIRED_LLVM_MAJOR
        );
        println!("  Arch Linux: sudo pacman -S llvm clang lld");
        println!(
            "Then set: LLVM_SYS_221_PREFIX=/usr/lib/llvm-{}",
            REQUIRED_LLVM_MAJOR
        );
    } else if cfg!(target_os = "macos") {
        println!(
            "Install LLVM {} and clang via Homebrew:",
            REQUIRED_LLVM_MAJOR
        );
        println!("  brew install llvm@{}", REQUIRED_LLVM_MAJOR);
        println!(
            "Then set: LLVM_SYS_221_PREFIX=$(brew --prefix llvm@{})",
            REQUIRED_LLVM_MAJOR
        );
    } else if cfg!(target_family = "windows") {
        println!(
            "Install LLVM {} and clang (for example via Chocolatey):",
            REQUIRED_LLVM_MAJOR
        );
        println!("  choco install llvm --version=22.1.6");
        println!("Then set LLVM_SYS_221_PREFIX to the LLVM install directory.");
    }
}

fn runtime_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn normalize_runtime_features(features: &[String]) -> Vec<String> {
    let mut normalized: BTreeSet<String> = BTreeSet::new();
    for feature in features {
        if !feature.is_empty() {
            normalized.insert(feature.clone());
        }
    }
    normalized.into_iter().collect()
}

fn runtime_feature_key(features: &[String]) -> String {
    if features.is_empty() {
        return "core".to_string();
    }
    features.join("+")
}

fn parse_runtime_feature_override() -> Option<Vec<String>> {
    let raw = env::var("MUX_RUNTIME_FEATURES").ok()?;
    let parsed: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    Some(normalize_runtime_features(&parsed))
}

fn resolve_runtime_features(required: &[String]) -> Vec<String> {
    let required = normalize_runtime_features(required);
    if let Some(override_features) = parse_runtime_feature_override() {
        let missing: Vec<String> = required
            .iter()
            .filter(|feature| !override_features.contains(*feature))
            .cloned()
            .collect();
        if !missing.is_empty() {
            eprintln!(
                "MUX_RUNTIME_FEATURES is missing required feature(s): {}",
                missing.join(", ")
            );
            eprintln!(
                "Required by imports in this program: {}",
                required.join(", ")
            );
            process::exit(1);
        }
        return override_features;
    }
    required
}

/// Returns the full set of runtime features that indicate a pre-built library can be used.
/// This list is derived from the std_registry and must match the `full` feature in mux-runtime/Cargo.toml.
fn full_runtime_features() -> Vec<String> {
    semantics::std_registry::all_runtime_features()
        .into_iter()
        .map(|s| s.to_string())
        .collect()
}

fn find_runtime_lib_in_dir(dir: &Path) -> Option<PathBuf> {
    let static_lib = if cfg!(target_family = "windows") {
        dir.join("mux_runtime.lib")
    } else {
        dir.join("libmux_runtime.a")
    };
    if static_lib.exists() {
        return Some(static_lib);
    }

    let dynamic_lib = if cfg!(target_family = "windows") {
        dir.join("mux_runtime.dll")
    } else if cfg!(target_os = "macos") {
        dir.join("libmux_runtime.dylib")
    } else {
        dir.join("libmux_runtime.so")
    };
    if dynamic_lib.exists() {
        return Some(dynamic_lib);
    }

    None
}

fn runtime_lib_from_env() -> Option<PathBuf> {
    let path = env::var("MUX_RUNTIME_LIB").ok()?;
    let path = PathBuf::from(path);
    if path.exists() {
        return path.parent().map(|p| p.to_path_buf());
    }

    eprintln!(
        "MUX_RUNTIME_LIB is set but does not exist: {}",
        path.display()
    );
    None
}

fn runtime_lib_from_build_config() -> Option<PathBuf> {
    use crate::build_config::{MUX_RUNTIME_DYNAMIC, MUX_RUNTIME_STATIC};

    let static_path = PathBuf::from(MUX_RUNTIME_STATIC);
    if static_path.exists() {
        return static_path.parent().map(|p| p.to_path_buf());
    }

    let dynamic_path = PathBuf::from(MUX_RUNTIME_DYNAMIC);
    if dynamic_path.exists() {
        return dynamic_path.parent().map(|p| p.to_path_buf());
    }

    None
}

fn runtime_lib_near_executable() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    if let Some(parent) =
        find_runtime_lib_in_dir(exe_dir).and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        return Some(parent);
    }

    if let Some(parent_dir) = exe_dir.parent() {
        let bundled_dirs = [parent_dir.join("lib"), parent_dir.join("lib").join("mux")];
        for lib_dir in bundled_dirs {
            if !lib_dir.exists() {
                continue;
            }

            if let Some(parent) =
                find_runtime_lib_in_dir(&lib_dir).and_then(|p| p.parent().map(|d| d.to_path_buf()))
            {
                return Some(parent);
            }
        }
    }

    None
}

fn cargo_home_dir() -> Option<PathBuf> {
    if let Ok(val) = env::var("CARGO_HOME") {
        return Some(PathBuf::from(val));
    }

    if cfg!(target_family = "windows") {
        let user = env::var("USERPROFILE").ok()?;
        return Some(PathBuf::from(user).join(".cargo"));
    }

    let home = env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".cargo"))
}

fn default_cache_root() -> PathBuf {
    if let Ok(val) = env::var("MUX_RUNTIME_CACHE_DIR") {
        return PathBuf::from(val);
    }

    if cfg!(target_family = "windows") {
        if let Ok(base) = env::var("LOCALAPPDATA") {
            return PathBuf::from(base).join("mux-lang");
        }
        if let Ok(base) = env::var("USERPROFILE") {
            return PathBuf::from(base).join(".mux-lang");
        }
    } else if cfg!(target_os = "macos") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("mux-lang");
        }
    } else {
        if let Ok(base) = env::var("XDG_CACHE_HOME") {
            return PathBuf::from(base).join("mux-lang");
        }
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(".cache").join("mux-lang");
        }
    }

    env::temp_dir().join("mux-lang")
}

fn find_runtime_source_dir() -> Option<PathBuf> {
    let local_runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("mux-runtime");
    if local_runtime.join("Cargo.toml").exists() {
        return Some(local_runtime);
    }

    if let Ok(src) = env::var("MUX_RUNTIME_SRC") {
        let path = PathBuf::from(src);
        if path.join("Cargo.toml").exists() {
            return Some(path);
        }
        eprintln!(
            "MUX_RUNTIME_SRC is set but Cargo.toml was not found: {}",
            path.display()
        );
    }

    let cargo_home = cargo_home_dir()?;
    let registry_src = cargo_home.join("registry").join("src");
    let version = env!("MUX_RUNTIME_VERSION");
    let dir_name = format!("mux-runtime-{}", version);

    for entry in fs::read_dir(registry_src).ok()? {
        let entry = entry.ok()?;
        let candidate = entry.path().join(&dir_name);
        if candidate.join("Cargo.toml").exists() {
            return Some(candidate);
        }
    }

    None
}

fn build_runtime_in_cache(profile: &str, features: &[String]) -> Option<PathBuf> {
    let target_root = default_cache_root()
        .join("runtime")
        .join(env!("MUX_RUNTIME_VERSION"))
        .join(runtime_feature_key(features));
    let profile_dir = target_root.join(profile);

    if let Some(lib) = find_runtime_lib_in_dir(&profile_dir) {
        return lib.parent().map(|p| p.to_path_buf());
    }

    let runtime_src = match find_runtime_source_dir() {
        Some(path) => path,
        None => {
            eprintln!("Could not locate mux-runtime source in cargo registry.");
            return None;
        }
    };

    if fs::create_dir_all(&target_root).is_err() {
        eprintln!(
            "Failed to create runtime cache directory: {}",
            target_root.display()
        );
        return None;
    }

    eprintln!("Building mux-runtime (first run)...");
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(runtime_src.join("Cargo.toml"))
        .arg("--no-default-features")
        .arg("--features")
        .arg(features.join(","))
        .env("CARGO_TARGET_DIR", &target_root);

    if profile == "release" {
        cmd.arg("--release");
    }

    let output = match cmd.output() {
        Ok(output) => output,
        Err(err) => {
            eprintln!("Failed to run cargo: {}", err);
            return None;
        }
    };

    if !output.status.success() {
        eprintln!("Failed to build mux-runtime.");
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        return None;
    }

    if let Some(lib) = find_runtime_lib_in_dir(&profile_dir) {
        return lib.parent().map(|p| p.to_path_buf());
    }

    eprintln!("mux-runtime build completed but library was not found.");
    None
}

fn resolve_runtime_lib_dir(profile: &str, features: &[String]) -> Option<PathBuf> {
    if let Some(dir) = runtime_lib_from_env() {
        return Some(dir);
    }

    if features == full_runtime_features() {
        return runtime_lib_near_executable()
            .or_else(runtime_lib_from_build_config)
            .or_else(|| build_runtime_in_cache(profile, features));
    }

    build_runtime_in_cache(profile, features)
}

/// Colored ASCII status marker for doctor checks: green "[ok]" or red "[x]".
fn status_marker(ok: bool) -> String {
    let (style, marker) = if ok {
        (AnsiColor::Green.on_default().bold(), "[ok]")
    } else {
        (AnsiColor::Red.on_default().bold(), "[x]")
    };
    format!("{style}{marker}{style:#}")
}

fn print_detected_llvm_versions(llvm_versions: &[(String, String, u32)]) {
    if llvm_versions.is_empty() {
        println!("No llvm-config command found on PATH.");
        return;
    }

    println!("Detected llvm-config versions:");
    for (tool, version, major) in llvm_versions {
        println!("  - {} => {} (major {})", tool, version, major);
    }
}

fn validate_llvm_for_doctor(
    dev_mode: bool,
    selected_dev_llvm: Option<(String, String, u32)>,
) -> bool {
    if dev_mode {
        return match selected_dev_llvm {
            Some((tool, version, _)) => {
                println!(
                    "{} LLVM development requirement satisfied with {} ({}).",
                    status_marker(true),
                    version,
                    tool
                );
                true
            }
            None => {
                println!(
                    "{} Contributor mode requires LLVM {}.x (llvm-config-{}).",
                    status_marker(false),
                    REQUIRED_LLVM_MAJOR,
                    REQUIRED_LLVM_MAJOR
                );
                print_llvm_install_help();
                false
            }
        };
    }

    if let Some((tool, version, _)) = selected_dev_llvm {
        println!(
            "{} Using LLVM {} from {}.",
            status_marker(true),
            version,
            tool
        );
    } else {
        println!(
            "{} LLVM {} was not detected. This is okay for prebuilt installs, but source builds need LLVM {}.",
            status_marker(true),
            REQUIRED_LLVM_MAJOR,
            REQUIRED_LLVM_MAJOR
        );
    }

    true
}

fn report_clang_for_doctor(clang: Option<&str>) -> bool {
    if let Some(clang_cmd) = clang {
        let linked_major: u32 = env!("MUX_LLVM_MAJOR")
            .parse()
            .unwrap_or(REQUIRED_LLVM_MAJOR);
        let clang_ok = match extract_clang_major(clang_cmd) {
            Some(clang_major) if clang_major == linked_major => {
                println!(
                    "{} Clang is installed: {} (matches linked LLVM {}).",
                    status_marker(true),
                    clang_cmd,
                    linked_major
                );
                true
            }
            Some(clang_major) => {
                println!(
                    "{} {} (clang {}) does not match linked LLVM {}.",
                    status_marker(false),
                    clang_cmd,
                    clang_major,
                    linked_major
                );
                println!(
                    "  This will cause IR parse errors. Install clang-{} or set CC=clang-{}.",
                    linked_major, linked_major
                );
                false
            }
            None => {
                println!("{} Clang is installed: {}.", status_marker(true), clang_cmd);
                true
            }
        };
        return clang_ok;
    }

    println!("{} Clang is not installed.", status_marker(false));
    print_llvm_install_help();
    false
}

// Running a clang binary that was just written and chmod'd can transiently fail
// to exec (e.g. ETXTBSY on Linux, before the writer's file handle is fully
// released). Retry a few times so version detection - and the doctor test that
// writes a fake clang then execs it - is not flaky under load. A genuinely
// missing clang still fails every attempt and returns None.
fn clang_version_output(clang_cmd: &str) -> Option<std::process::Output> {
    for attempt in 0..5 {
        match Command::new(clang_cmd).arg("--version").output() {
            Ok(output) => return Some(output),
            Err(_) if attempt + 1 < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
    None
}

fn extract_clang_major(clang_cmd: &str) -> Option<u32> {
    let output = clang_version_output(clang_cmd)?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Output is like "clang version 17.0.6" or "Ubuntu clang version 22.1.6"
    let version_part = stdout.lines().next()?;
    let version_str = version_part.split_whitespace().find(|s| {
        s.split('.')
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .is_some()
    })?;
    let major_str = version_str.split('.').next()?;
    major_str.parse::<u32>().ok()
}

fn ensure_runtime_for_doctor() -> bool {
    let profile = runtime_profile();
    let features = full_runtime_features();
    let runtime_ok = if resolve_runtime_lib_dir(profile, &features).is_some() {
        true
    } else {
        println!("Mux runtime not found. Building it now...");
        build_runtime_in_cache(profile, &features).is_some()
    };

    report_runtime_for_doctor(runtime_ok)
}

fn report_runtime_for_doctor(runtime_ok: bool) -> bool {
    if runtime_ok {
        println!("{} Mux runtime is available.", status_marker(true));
    } else {
        println!("{} Mux runtime is not available.", status_marker(false));
    }

    runtime_ok
}

/// Print the final doctor verdict line and return whether all checks passed.
fn print_doctor_verdict(ok: bool) -> bool {
    if ok {
        let style = AnsiColor::Green.on_default().bold();
        println!("{style}Your system is ready to use the Mux compiler!{style:#}");
    } else {
        let style = AnsiColor::Red.on_default().bold();
        println!("{style}Please install the missing dependencies and try again.{style:#}");
    }

    ok
}

fn run_doctor(dev_mode: bool) {
    let llvm_versions = collect_llvm_versions();
    let selected_dev_llvm = pick_llvm_for_dev(&llvm_versions);

    print_detected_llvm_versions(&llvm_versions);
    let llvm_ok = validate_llvm_for_doctor(dev_mode, selected_dev_llvm);
    let clang = find_clang_command();
    let clang_ok = report_clang_for_doctor(clang.as_deref());
    let runtime_ok = ensure_runtime_for_doctor();

    if !print_doctor_verdict(llvm_ok && clang_ok && runtime_ok) {
        process::exit(1);
    }
}

fn get_clang_version() -> Option<String> {
    let clang = find_clang_command()?;
    let output = Command::new(&clang).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next()?;
    first_line
        .split_whitespace()
        .find(|s| s.starts_with(|c: char| c.is_ascii_digit()))
        .map(|s| s.to_string())
}

fn get_llvm_version() -> String {
    for candidate in llvm_config_candidates() {
        if let Ok(output) = Command::new(&candidate).arg("--version").output()
            && output.status.success()
        {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !raw.is_empty() {
                return raw;
            }
        }
    }
    format!("{}.x", env!("MUX_LLVM_MAJOR"))
}

fn print_version_banner() {
    use std::thread::sleep;
    use std::time::Duration;

    let palette = BannerPalette::new();
    let green = AnsiColor::Green.on_default().bold();
    let combined = banner_logo_rows();
    let version_lines = banner_version_lines(green);
    let num_versions = version_lines.len();

    // Allocate blank space for the animated section
    for _ in 0..BANNER_ANIM_ROWS {
        println!();
    }
    // Version info below the animated area, visible from the start
    for v in &version_lines {
        println!("{v}");
    }

    let mut out = stdout();
    out.flush().ok();

    // Neon gradient column wipe (1 col per frame)
    for cols in 1..=BANNER_TOTAL_COLS {
        let offset = if cols == 1 {
            BANNER_ANIM_ROWS + num_versions
        } else {
            BANNER_ANIM_ROWS
        };
        let buf = banner_sweep_frame(&combined, cols, offset, &palette);
        write!(out, "{buf}").ok();
        out.flush().ok();
        sleep(Duration::from_millis(10));
    }

    // Settle frame: render the full logo in settled blue
    sleep(Duration::from_millis(10));
    let buf = banner_settled_frame(&combined, &palette.settled);
    write!(out, "{buf}").ok();
    out.flush().ok();

    // Pause to show the completed logo before moving on
    sleep(Duration::from_millis(200));

    // Move cursor past the version lines so the shell prompt doesn't overlap
    write!(out, "\x1b[{}B", num_versions).ok();
    out.flush().ok();
}

const BANNER_LOGO_ROWS: usize = 6;
const BANNER_ANIM_ROWS: usize = BANNER_LOGO_ROWS + 3;
const BANNER_TOTAL_COLS: usize = 30;
const BANNER_MSG: &str = "The Programming Language For Everyone";

struct BannerPalette {
    settled: anstyle::Style,
    warm: anstyle::Style,
    glow: anstyle::Style,
    hot: anstyle::Style,
}

impl BannerPalette {
    fn new() -> Self {
        let rgb = |r, g, b| anstyle::Style::new().fg_color(Some(anstyle::RgbColor(r, g, b).into()));
        Self {
            settled: rgb(0x60, 0xa5, 0xfa),
            warm: rgb(0x93, 0xc5, 0xfd),
            glow: rgb(0xbf, 0xdb, 0xfe),
            hot: rgb(0xff, 0xff, 0xff),
        }
    }

    /// Style for a column that trails the sweep head by `dist` columns.
    fn for_distance(&self, dist: usize) -> &anstyle::Style {
        match dist {
            0..=1 => &self.hot,
            2..=4 => &self.warm,
            5..=8 => &self.glow,
            _ => &self.settled,
        }
    }
}

/// Left-pad or truncate `content` to exactly `w` characters.
fn banner_pad(content: &str, w: usize) -> String {
    let mut chars: Vec<char> = content.chars().collect();
    chars.truncate(w);
    let mut s: String = chars.into_iter().collect();
    while s.chars().count() < w {
        s.push(' ');
    }
    s
}

/// Combined M-U-X logo bitmap: 6 rows, 30 chars wide (11+1+9+1+8).
fn banner_logo_rows() -> Vec<Vec<char>> {
    let m: [&str; BANNER_LOGO_ROWS] = [
        "███╗   ███╗",
        "████╗ ████║",
        "██╔████╔██║",
        "██║╚██╔╝██║",
        "██║ ╚═╝ ██║",
        "╚═╝     ╚═╝",
    ];
    let u: [&str; BANNER_LOGO_ROWS] = [
        "██╗   ██╗",
        "██║   ██║",
        "██║   ██║",
        "██║   ██║",
        "╚██████╔╝",
        " ╚═════╝",
    ];
    let x: [&str; BANNER_LOGO_ROWS] = [
        "██╗  ██╗",
        "╚██╗██╔╝",
        " ╚███╔╝",
        " ██╔██╗",
        "██╔╝ ██╗",
        "╚═╝  ╚═╝",
    ];
    (0..BANNER_LOGO_ROWS)
        .map(|row| {
            let mut s = String::new();
            s.push_str(&banner_pad(m[row], 11));
            s.push(' ');
            s.push_str(&banner_pad(u[row], 9));
            s.push(' ');
            s.push_str(&banner_pad(x[row], 8));
            s.chars().collect()
        })
        .collect()
}

fn banner_version_lines(green: anstyle::Style) -> Vec<String> {
    let mut version_lines = vec![
        format!("{green}compiler{green:#} v{}", env!("CARGO_PKG_VERSION")),
        format!("{green}runtime{green:#} v{}", env!("MUX_RUNTIME_VERSION")),
    ];
    if let Some(ref c) = get_clang_version() {
        version_lines.push(format!("{green}clang{green:#} v{c}"));
    }
    version_lines.push(format!("{green}llvm{green:#} v{}", get_llvm_version()));
    version_lines
}

/// One animation frame: the logo revealed up to `cols` columns, with a
/// neon gradient trailing the sweep head. Starts by moving the cursor up
/// `offset` rows so the frame redraws in place.
fn banner_sweep_frame(
    combined: &[Vec<char>],
    cols: usize,
    offset: usize,
    palette: &BannerPalette,
) -> String {
    let mut buf = format!("\x1b[{}A", offset);
    // Blank line before logo
    buf.push('\n');
    for char_row in combined {
        for (col, ch) in char_row.iter().enumerate().take(cols) {
            let s = palette.for_distance(cols - 1 - col);
            buf.push_str(&format!("{s}{ch}{s:#}"));
        }
        buf.push('\n');
    }
    buf.push('\n');
    let settled = &palette.settled;
    buf.push_str(&format!("{settled}{BANNER_MSG}{settled:#}\n"));
    buf
}

fn banner_settled_frame(combined: &[Vec<char>], settled: &anstyle::Style) -> String {
    let mut buf = format!("\x1b[{}A", BANNER_ANIM_ROWS);
    buf.push('\n');
    for char_row in combined {
        let s: String = char_row.iter().collect();
        buf.push_str(&format!("{settled}{s}{settled:#}\n"));
    }
    buf.push('\n');
    buf.push_str(&format!("{settled}{BANNER_MSG}{settled:#}\n"));
    buf
}

fn parse_args_or_exit() -> (PathBuf, bool, Option<PathBuf>, bool) {
    let cli = Cli::parse();
    match &cli.command {
        Commands::Version {} => {
            print_version_banner();
            process::exit(0);
        }
        Commands::Doctor { dev } => {
            run_doctor(*dev);
            process::exit(0);
        }
        Commands::Build {
            file,
            output,
            intermediate,
        } => (file.clone(), false, output.clone(), *intermediate),
        Commands::Run {
            file,
            output,
            intermediate,
        } => (file.clone(), true, output.clone(), *intermediate),
        Commands::Format { file } => {
            eprintln!("formatting is not yet implemented for {}", file.display());
            process::exit(1);
        }
    }
}

fn ensure_mux_extension_or_exit(file_path: &Path) {
    if !file_path.to_string_lossy().ends_with(".mux") {
        eprintln!("Error: Input file must have a .mux extension.");
        process::exit(1);
    }
}

fn load_source_or_exit(file_path: &Path) -> String {
    match fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening file: {}", e);
            process::exit(1);
        }
    }
}

fn lex_source_or_exit(file_id: FileId, files: &Files, source: String) -> Vec<lexer::Token> {
    let mut src = Source::from_string(source);
    let mut lex = lexer::Lexer::new(&mut src);
    match lex.lex_all() {
        Ok(tokens) => tokens,
        Err(err) => {
            emit_diagnostics(files, file_id, &[err]);
            process::exit(1);
        }
    }
}

fn parse_tokens_or_exit(
    file_id: FileId,
    files: &Files,
    tokens: &[lexer::Token],
) -> Vec<ast::AstNode> {
    let mut parser = parser::Parser::new(tokens);
    match parser.parse() {
        Ok(nodes) => nodes,
        Err((_, errors)) => {
            emit_diagnostics(files, file_id, &errors);
            process::exit(1);
        }
    }
}

fn analyze_semantics_or_exit(
    analyzer: &mut semantics::SemanticAnalyzer,
    nodes: &[ast::AstNode],
    file_id: FileId,
    files: &mut Files,
) {
    let errors = analyzer.analyze(nodes, Some(files));
    if !errors.is_empty() {
        emit_diagnostics(files, file_id, &errors);
        process::exit(1);
    }
}

fn generate_ir_or_exit(
    codegen: &mut codegen::CodeGenerator,
    nodes: &[ast::AstNode],
    ir_file: &str,
) {
    if let Err(e) = codegen.generate(nodes) {
        spinner::stop();
        eprintln!("Codegen error: {}", e);
        process::exit(1);
    }
    if let Err(e) = codegen.emit_ir_to_file(ir_file) {
        spinner::stop();
        eprintln!("Failed to emit IR: {}", e);
        process::exit(1);
    }
}

fn resolve_runtime_lib_dir_or_exit(profile: &str, features: &[String]) -> PathBuf {
    match resolve_runtime_lib_dir(profile, features) {
        Some(dir) => dir,
        None => {
            spinner::stop();
            eprintln!();
            eprintln!("Could not locate mux-runtime.");
            eprintln!("You can set MUX_RUNTIME_LIB to a built library path.");
            eprintln!("You can set MUX_RUNTIME_SRC to a local mux-runtime source.");
            eprintln!("Requested runtime features: {}", features.join(","));
            eprintln!("Example:");
            eprintln!("  MUX_RUNTIME_LIB=/path/to/libmux_runtime.a mux run file.mux");
            process::exit(1);
        }
    }
}

fn find_clang_or_exit() -> String {
    match find_clang_command() {
        Some(cmd) => cmd,
        None => {
            spinner::stop();
            eprintln!("clang is required to link Mux programs but was not found on PATH.");
            print_llvm_install_help();
            process::exit(1);
        }
    }
}

fn report_clang_output_or_exit(
    clang_output: std::io::Result<std::process::Output>,
    _do_run: bool,
    _file_path: &Path,
    ir_file: &str,
) {
    match clang_output {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            eprintln!("clang failed: {}", String::from_utf8_lossy(&output.stderr));
            process::exit(1);
        }
        Err(e) => {
            eprintln!(
                "Failed to run clang: {}. IR file generated at: {}",
                e, ir_file
            );
            process::exit(1);
        }
    }
}

fn remove_ir_if_requested(intermediate: bool, ir_file: &str) {
    if intermediate {
        return;
    }

    Command::new("rm")
        .arg(ir_file)
        .status()
        .expect("Failed to remove intermediate IR file");
}

fn run_executable_or_exit(exe_file: &Path) {
    let run_path = if exe_file.is_absolute() {
        exe_file.to_path_buf()
    } else {
        PathBuf::from("./").join(exe_file)
    };
    let status = Command::new(&run_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("Program exited with status: {}", status);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to execute program: {}", e);
            process::exit(1);
        }
    }
}

fn main() {
    let (file_path, do_run, output, intermediate) = parse_args_or_exit();

    ensure_mux_extension_or_exit(&file_path);

    let mut files = Files::new();
    let source_str = load_source_or_exit(&file_path);
    let file_id = files.add(&file_path, source_str.clone());

    // Progress spinner on stderr for slow compiles; error paths and the
    // post-link stop below clear the line before anything else prints.
    spinner::start(format!("compiling {}", file_path.display()));

    let tokens = { lex_source_or_exit(file_id, &files, source_str) };
    let nodes = { parse_tokens_or_exit(file_id, &files, &tokens) };

    let base_path = file_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let resolver = Rc::new(RefCell::new(ModuleResolver::new(base_path)));

    let mut analyzer = semantics::SemanticAnalyzer::new_with_resolver(resolver);
    analyze_semantics_or_exit(&mut analyzer, &nodes, file_id, &mut files);
    let runtime_features = resolve_runtime_features(&analyzer.required_runtime_features());

    let context = inkwell::context::Context::create();
    let source_name = file_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.to_string_lossy().into_owned());
    let mut codegen = codegen::CodeGenerator::new(&context, &mut analyzer, &source_name);

    let ir_file = format!(
        "{}.ll",
        file_path.to_string_lossy().trim_end_matches(".mux")
    );
    generate_ir_or_exit(&mut codegen, &nodes, &ir_file);

    // build executable
    // Use ./ prefix to ensure we run the local executable, not a system command
    // (e.g., "test" would find the shell built-in instead of ./test)
    // Executable goes next to the source file unless -o is specified
    let exe_file = if let Some(out_path) = &output {
        if out_path.parent().is_some_and(|p| !p.as_os_str().is_empty()) {
            out_path.clone()
        } else {
            PathBuf::from("./").join(out_path)
        }
    } else {
        let source_path = PathBuf::from(file_path.to_string_lossy().trim_end_matches(".mux"));
        let parent = source_path.parent().unwrap_or(Path::new("."));
        let file_stem = source_path
            .file_stem()
            .expect("executable name should be valid Unicode");
        parent.join(file_stem)
    };

    let profile = runtime_profile();
    let lib_dir = resolve_runtime_lib_dir_or_exit(profile, &runtime_features);

    let lib_path_str = lib_dir
        .to_str()
        .expect("library path should be valid Unicode");

    let clang_cmd = find_clang_or_exit();
    let mut clang_args = vec![
        ir_file.clone(),
        "-L".to_string(),
        lib_path_str.to_string(),
        format!("-Wl,-rpath,{}", lib_path_str),
        "-ffunction-sections".to_string(),
        "-fdata-sections".to_string(),
    ];

    #[cfg(not(target_os = "macos"))]
    clang_args.push("-Wl,--disable-new-dtags".to_string());

    let gc_sections_flag = if cfg!(target_os = "macos") {
        "-Wl,-dead_strip".to_string()
    } else {
        "-Wl,--gc-sections".to_string()
    };
    clang_args.push(gc_sections_flag);
    clang_args.push("-lmux_runtime".to_string());
    clang_args.push("-o".to_string());
    clang_args.push(
        exe_file
            .to_str()
            .expect("executable path should be valid Unicode")
            .to_string(),
    );

    let clang_output = Command::new(&clang_cmd).args(&clang_args).output();

    spinner::stop();
    report_clang_output_or_exit(clang_output, do_run, &file_path, &ir_file);

    remove_ir_if_requested(intermediate, &ir_file);

    if do_run {
        run_executable_or_exit(&exe_file);
    }
}

#[cfg(test)]
mod tests {
    use super::full_runtime_features;
    use super::{
        REQUIRED_LLVM_MAJOR, default_cache_root, extract_clang_major, find_runtime_lib_in_dir,
        llvm_config_candidates, normalize_runtime_features, pick_llvm_for_dev,
        print_doctor_verdict, print_version_banner, report_clang_for_doctor,
        report_runtime_for_doctor, runtime_feature_key, runtime_profile, status_marker,
        validate_llvm_for_doctor,
    };
    use std::path::PathBuf;

    fn sv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn unique_tmp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("mux_{}_{}_{}", tag, std::process::id(), nanos))
    }

    // Mirror the platform-specific library names used by find_runtime_lib_in_dir.
    fn static_lib_name() -> &'static str {
        if cfg!(target_family = "windows") {
            "mux_runtime.lib"
        } else {
            "libmux_runtime.a"
        }
    }

    fn dynamic_lib_name() -> &'static str {
        if cfg!(target_family = "windows") {
            "mux_runtime.dll"
        } else if cfg!(target_os = "macos") {
            "libmux_runtime.dylib"
        } else {
            "libmux_runtime.so"
        }
    }

    #[test]
    fn normalize_runtime_features_dedups_sorts_and_drops_empty() {
        assert_eq!(
            normalize_runtime_features(&sv(&["math", "json", "math", "", "csv"])),
            sv(&["csv", "json", "math"])
        );
        assert!(normalize_runtime_features(&[]).is_empty());
    }

    #[test]
    fn runtime_feature_key_uses_core_for_empty_and_joins_otherwise() {
        assert_eq!(runtime_feature_key(&[]), "core");
        assert_eq!(runtime_feature_key(&sv(&["json", "math"])), "json+math");
    }

    #[test]
    fn runtime_profile_is_a_known_cargo_profile() {
        assert!(matches!(runtime_profile(), "debug" | "release"));
    }

    #[test]
    fn pick_llvm_for_dev_selects_required_major() {
        let empty: Vec<(String, String, u32)> = Vec::new();
        assert!(pick_llvm_for_dev(&empty).is_none());

        let versions = vec![
            ("llvm-config-17".to_string(), "17.0.0".to_string(), 17),
            (
                "llvm-config-22".to_string(),
                "22.1.6".to_string(),
                REQUIRED_LLVM_MAJOR,
            ),
        ];
        let picked = pick_llvm_for_dev(&versions).expect("required major present");
        assert_eq!(picked.0, "llvm-config-22");
        assert_eq!(picked.2, REQUIRED_LLVM_MAJOR);

        let mismatch = vec![("other".to_string(), "17.0.0".to_string(), 17)];
        assert!(pick_llvm_for_dev(&mismatch).is_none());
    }

    #[test]
    fn llvm_config_candidates_always_includes_defaults() {
        let candidates = llvm_config_candidates();
        assert!(candidates.iter().any(|c| c == "llvm-config"));
        assert!(
            candidates
                .iter()
                .any(|c| c == &format!("llvm-config-{}", REQUIRED_LLVM_MAJOR))
        );
    }

    #[test]
    fn find_runtime_lib_in_dir_detects_static_dynamic_and_missing() {
        // Empty directory: nothing found.
        let empty_dir = unique_tmp("rtlib_empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        assert!(find_runtime_lib_in_dir(&empty_dir).is_none());
        std::fs::remove_dir_all(&empty_dir).ok();

        // Static library takes precedence and is returned.
        let static_dir = unique_tmp("rtlib_static");
        std::fs::create_dir_all(&static_dir).unwrap();
        let static_path = static_dir.join(static_lib_name());
        std::fs::write(&static_path, b"x").unwrap();
        assert_eq!(find_runtime_lib_in_dir(&static_dir), Some(static_path));
        std::fs::remove_dir_all(&static_dir).ok();

        // Dynamic library is found when only it is present.
        let dyn_dir = unique_tmp("rtlib_dyn");
        std::fs::create_dir_all(&dyn_dir).unwrap();
        let dyn_path = dyn_dir.join(dynamic_lib_name());
        std::fs::write(&dyn_path, b"x").unwrap();
        assert_eq!(find_runtime_lib_in_dir(&dyn_dir), Some(dyn_path));
        std::fs::remove_dir_all(&dyn_dir).ok();
    }

    #[test]
    fn validate_llvm_for_doctor_covers_dev_and_user_modes() {
        let found = || Some(("llvm-config-22".to_string(), "22.1.6".to_string(), 22));
        // Dev mode requires the toolchain: present -> ok, absent -> fail.
        assert!(validate_llvm_for_doctor(true, found()));
        assert!(!validate_llvm_for_doctor(true, None));
        // User mode is lenient either way.
        assert!(validate_llvm_for_doctor(false, found()));
        assert!(validate_llvm_for_doctor(false, None));
    }

    #[test]
    fn clang_doctor_helpers_handle_missing_clang() {
        // No clang at all is a failure.
        assert!(!report_clang_for_doctor(None));
        // A command that cannot be executed yields no major version...
        assert!(extract_clang_major("mux-nonexistent-clang-binary-xyz").is_none());
        // ...and report treats an unparseable/missing version as "installed".
        assert!(report_clang_for_doctor(Some(
            "mux-nonexistent-clang-binary-xyz"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn clang_doctor_reports_matching_and_mismatching_majors() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_tmp("fake_clang");
        std::fs::create_dir_all(&dir).unwrap();
        let write_fake_clang = |name: &str, major: u32| -> PathBuf {
            let path = dir.join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho \"clang version {}.0.0\"\n", major),
            )
            .unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path
        };

        let linked_major: u32 = env!("MUX_LLVM_MAJOR")
            .parse()
            .unwrap_or(REQUIRED_LLVM_MAJOR);

        let matching = write_fake_clang("clang-match", linked_major);
        let matching = matching.to_str().unwrap();
        assert_eq!(extract_clang_major(matching), Some(linked_major));
        assert!(report_clang_for_doctor(Some(matching)));

        let mismatching = write_fake_clang("clang-mismatch", linked_major + 1);
        assert!(!report_clang_for_doctor(Some(
            mismatching.to_str().unwrap()
        )));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn status_marker_uses_colored_ascii_glyphs() {
        let ok = status_marker(true);
        assert!(ok.contains("[ok]"), "ok marker: {ok:?}");
        assert!(
            ok.contains("\u{1b}[32m"),
            "ok marker should be green: {ok:?}"
        );

        let fail = status_marker(false);
        assert!(fail.contains("[x]"), "fail marker: {fail:?}");
        assert!(
            fail.contains("\u{1b}[31m"),
            "fail marker should be red: {fail:?}"
        );

        // ASCII only once the color codes are stripped.
        assert!(ok.is_ascii());
        assert!(fail.is_ascii());
    }

    #[test]
    fn doctor_report_helpers_pass_through_status() {
        assert!(report_runtime_for_doctor(true));
        assert!(!report_runtime_for_doctor(false));
        assert!(print_doctor_verdict(true));
        assert!(!print_doctor_verdict(false));
    }

    #[test]
    fn version_banner_prints_without_error() {
        // Exercise the printing path used by `mux version`.
        print_version_banner();
    }

    #[test]
    fn default_cache_root_is_namespaced() {
        assert!(default_cache_root().to_string_lossy().contains("mux-lang"));
    }

    /// Locate the `mux-runtime` `Cargo.toml`, mirroring the binary's runtime
    /// source resolution: in-workspace/sibling checkout, then `MUX_RUNTIME_SRC`,
    /// then the fetched crate in the cargo registry. Returns `None` when no
    /// source is available (e.g. CI that only links the published crate's lib).
    fn locate_runtime_cargo_toml() -> Option<std::path::PathBuf> {
        use std::path::{Path, PathBuf};

        // 1. In-workspace / sibling checkout (`../mux-runtime`).
        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR")
            && let Some(parent) = Path::new(&manifest_dir).parent()
        {
            let sibling = parent.join("mux-runtime").join("Cargo.toml");
            if sibling.exists() {
                return Some(sibling);
            }
        }

        // 2. Explicit override for coupled local dev.
        if let Ok(src) = std::env::var("MUX_RUNTIME_SRC") {
            let p = Path::new(&src).join("Cargo.toml");
            if p.exists() {
                return Some(p);
            }
        }

        // 3. Fetched crate in the cargo registry (production path).
        let cargo_home = std::env::var("CARGO_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".cargo"))
            })?;
        let registry_src = cargo_home.join("registry").join("src");
        let dir_name = format!("mux-runtime-{}", env!("MUX_RUNTIME_VERSION"));
        for entry in std::fs::read_dir(registry_src).ok()?.flatten() {
            let candidate = entry.path().join(&dir_name).join("Cargo.toml");
            if candidate.exists() {
                return Some(candidate);
            }
        }

        None
    }

    #[test]
    fn full_runtime_features_matches_cargo_toml() {
        let Some(cargo_toml_path) = locate_runtime_cargo_toml() else {
            eprintln!(
                "skipping full_runtime_features parity check: mux-runtime source not \
                 found (set MUX_RUNTIME_SRC or check out mux-runtime as a sibling)"
            );
            return;
        };
        let cargo_toml_content = std::fs::read_to_string(&cargo_toml_path)
            .expect("Failed to read mux-runtime Cargo.toml");

        // Parse the [features].full array. Use a real TOML parser: cargo
        // normalizes the published manifest (the array may span multiple lines),
        // so naive line parsing is not reliable.
        let manifest: toml::Value =
            toml::from_str(&cargo_toml_content).expect("Failed to parse mux-runtime Cargo.toml");
        let full = manifest
            .get("features")
            .and_then(|features| features.get("full"))
            .and_then(|value| value.as_array())
            .expect("mux-runtime Cargo.toml has no [features].full array");
        // Remove "core": it is a meta-feature, not a stdlib module that needs checking.
        let mut toml_features: Vec<String> = full
            .iter()
            .filter_map(|value| value.as_str())
            .filter(|name| *name != "core")
            .map(|name| name.to_string())
            .collect();
        toml_features.sort();

        // Get the runtime features from our function
        let mut runtime_features = full_runtime_features();
        runtime_features.sort();

        assert_eq!(
            toml_features, runtime_features,
            "full_runtime_features() does not match mux-runtime/Cargo.toml full feature list.\n\
             Expected (from Cargo.toml): {:?}\n\
             Actual (from function):   {:?}\n\
             Hint: Update the hardcoded list in full_runtime_features() to match the full feature in Cargo.toml",
            toml_features, runtime_features
        );
    }
}
