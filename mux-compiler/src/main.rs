mod ast;
mod build_config;
mod codegen;
pub mod diagnostic;
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
use clap::{Parser as ClapParser, Subcommand, ValueEnum};
use diagnostic::{
    ColorConfig, Diagnostic, DiagnosticCode, DiagnosticEmitter, FileId, Files, MAX_DIAGNOSTICS,
    StandardEmitter, ToDiagnostic,
    fix::{self, RecoveryIntervals},
};
use module_resolver::ModuleResolver;
use source::Source;
use std::cell::RefCell;
use std::collections::HashSet;
use std::env;
use std::fmt::Write as _;
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

fn emit_diagnostics<E: ToDiagnostic>(
    files: &Files,
    file_id: FileId,
    errors: &[E],
    deny_warnings: bool,
) -> bool {
    let diagnostics: Vec<_> = errors.iter().map(|e| e.to_diagnostic(file_id)).collect();
    emit_diagnostic_batch(files, &diagnostics, deny_warnings)
}

fn emit_diagnostic_batch(files: &Files, diagnostics: &[Diagnostic], deny_warnings: bool) -> bool {
    // Clear any in-progress spinner line before printing diagnostics.
    spinner::stop();
    let emitter = StandardEmitter::new(ColorConfig::Auto);
    emitter.emit_batch(diagnostics, files);
    diagnostics.iter().any(|diagnostic| {
        diagnostic.level == diagnostic::Level::Error
            || (deny_warnings && diagnostic.level == diagnostic::Level::Warning)
    })
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

    /// Treat warnings as compilation failures while preserving their codes.
    #[arg(long, global = true)]
    deny_warnings: bool,

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
    /// Explain a compiler diagnostic code
    Explain {
        /// A code such as E0302 or W0300
        code: String,
    },
    /// Apply proven machine-applicable compiler fixes
    Fix {
        file: PathBuf,
        /// Show the proposed diff without changing files.
        #[arg(long)]
        dry_run: bool,
        /// Select human-readable or machine-readable output.
        #[arg(long, value_enum, default_value_t = FixOutputFormat::Text)]
        format: FixOutputFormat,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FixOutputFormat {
    Text,
    Json,
}

fn find_linker_command() -> Option<String> {
    if let Ok(cc) = env::var("CC") {
        let output = Command::new(&cc).arg("--version").output();
        if output.is_ok_and(|o| o.status.success()) {
            return Some(cc);
        }
    }

    // Any C driver can link an object file. The version used to matter because
    // the compiler handed clang textual IR to parse; it emits an object now, so
    // `cc` and `gcc` are just as valid and the search no longer needs a
    // version-matched clang to exist.
    let linked_major = env!("MUX_LLVM_MAJOR");
    let versioned = format!("clang-{linked_major}");
    let candidates: &[&str] = &[versioned.as_str(), "clang", "cc", "gcc"];
    for candidate in candidates {
        let Ok(output) = Command::new(candidate).arg("--version").output() else {
            continue;
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

    candidates.push(format!("llvm-config-{REQUIRED_LLVM_MAJOR}"));
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

        let Ok(output) = Command::new(&candidate).arg("--version").output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }

        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let major_str = raw.split('.').next().unwrap_or("");
        let Ok(major) = major_str.parse::<u32>() else {
            continue;
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

/// Install help for the C driver used to link programs. Distinct from
/// `print_llvm_install_help`, which is about the LLVM *development* libraries a
/// source build of the compiler needs - linking a compiled program only needs a
/// C toolchain, of any version.
fn print_linker_install_help() {
    if cfg!(target_os = "linux") {
        println!("Install a C toolchain:");
        println!("  Debian/Ubuntu: sudo apt-get install clang");
        println!("  Arch Linux:    sudo pacman -S clang");
        println!("Any recent clang or gcc works; the version does not need to match.");
    } else if cfg!(target_os = "macos") {
        println!("Install the Xcode command line tools:");
        println!("  xcode-select --install");
    } else if cfg!(target_family = "windows") {
        println!("Install LLVM (which provides clang), for example via Chocolatey:");
        println!("  choco install llvm");
    }
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

/// Path to the static runtime archive in `dir` for the target platform. The
/// single source of the archive filename, so runtime discovery and static-only
/// classification cannot disagree.
fn runtime_static_lib_path(dir: &Path) -> PathBuf {
    let expected = if cfg!(target_family = "windows") {
        dir.join("mux_runtime.lib")
    } else {
        dir.join("libmux_runtime.a")
    };
    if expected.exists() {
        return expected;
    }

    // A git dependency selected with `cargo build -p mux-runtime` is built as
    // a dependency artifact, so Cargo puts its hash-suffixed archive under
    // `target/<profile>/deps/` instead of beside the profile directory.
    // Resolve that form too; otherwise source-checkout tests cannot link even
    // immediately after the documented runtime build command.
    let deps = dir.join("deps");
    let prefix = if cfg!(target_family = "windows") {
        "mux_runtime-"
    } else {
        "libmux_runtime-"
    };
    let candidates: Vec<PathBuf> = std::fs::read_dir(deps)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| {
                    path.is_file()
                        && name.starts_with(prefix)
                        && (name.ends_with(".a") || name.ends_with(".lib"))
                })
        })
        .collect();
    // Cargo can leave archives from previous builds in `deps/`, and directory
    // iteration order is unspecified. If more than one hash is present, fail
    // closed instead of guessing and potentially linking a stale runtime.
    // `cargo clean` (or a fresh target directory) makes the intended artifact
    // unambiguous; a uniquely matching archive is safe to use.
    match candidates.as_slice() {
        [candidate] => candidate.clone(),
        _ => expected,
    }
}

/// Path to the dynamic runtime library in `dir` for the target platform. Shared
/// with `runtime_static_lib_path` as the one place the runtime lib names live.
fn runtime_dynamic_lib_path(dir: &Path) -> PathBuf {
    if cfg!(target_family = "windows") {
        dir.join("mux_runtime.dll")
    } else if cfg!(target_os = "macos") {
        dir.join("libmux_runtime.dylib")
    } else {
        dir.join("libmux_runtime.so")
    }
}

fn find_runtime_lib_in_dir(dir: &Path) -> Option<PathBuf> {
    let static_lib = runtime_static_lib_path(dir);
    if static_lib.exists() {
        return Some(static_lib);
    }

    // A bare DLL is not a linker input on Windows: you link against the import
    // library and the DLL is only loaded at run time. Accepting one here picks a
    // directory the linker cannot use, and since a real install must put the DLL
    // beside the executable for the loader to find it, `bin/` would always win
    // over the `lib/` holding the import library - failing every compile with
    // "LNK1181: cannot open input file 'mux_runtime.lib'".
    //
    // Elsewhere a shared object is a perfectly good link input, so it still
    // counts.
    if cfg!(target_family = "windows") {
        return None;
    }

    let dynamic_lib = runtime_dynamic_lib_path(dir);
    if dynamic_lib.exists() {
        return Some(dynamic_lib);
    }

    None
}

/// Whether `dir` provides only the static `libmux_runtime.a` with no dynamic
/// library beside it. A static archive carries no `NEEDED` entries, so when it
/// is linked alone its undefined native symbols (libm's `pow`, zlib, ...) are
/// never resolved; the cdylib records those dependencies itself. Used to decide
/// whether to link the runtime's native deps explicitly (issue #291).
fn runtime_lib_dir_is_static_only(dir: &Path) -> bool {
    if !runtime_static_lib_path(dir).exists() {
        return false;
    }

    // On Windows this is a question about which .lib the linker picks, not about
    // whether a DLL is present. mux-runtime builds both a staticlib and a
    // cdylib, and cargo names them `mux_runtime.lib` and `mux_runtime.dll.lib`
    // respectively - so `-lmux_runtime` always resolves the STATIC archive. A
    // DLL sitting beside it does not make the link dynamic; it is there because
    // the loader needs it, and a packaged install ships it for that reason.
    //
    // Reading the DLL as "dynamic" here suppressed the native system libraries
    // below and failed every Windows compile with LNK2019 on symbols they own.
    if cfg!(target_family = "windows") {
        return true;
    }

    let dynamic_lib = runtime_dynamic_lib_path(dir);
    !dynamic_lib.exists()
}

fn runtime_lib_from_env() -> Option<PathBuf> {
    let path = env::var("MUX_RUNTIME_LIB").ok()?;
    let path = PathBuf::from(path);
    if path.exists() {
        return path.parent().map(Path::to_path_buf);
    }

    eprintln!(
        "MUX_RUNTIME_LIB is set but does not exist: {}",
        path.display()
    );
    None
}

/// The runtime library cargo built into `target/`, at the path `build.rs`
/// recorded. Both recorded paths live in the same profile directory, so this
/// asks `find_runtime_lib_in_dir` about that directory rather than repeating
/// the static-then-dynamic decision - which also keeps the platform library
/// names in one place.
fn runtime_lib_from_build_config() -> Option<PathBuf> {
    use crate::build_config::MUX_RUNTIME_STATIC;

    let profile_dir = PathBuf::from(MUX_RUNTIME_STATIC).parent()?.to_path_buf();
    dir_holding_runtime_lib(&profile_dir)
}

/// `dir` itself, when it holds a runtime library. `find_runtime_lib_in_dir`
/// builds the candidate paths from `dir`, so the containing directory is `dir`
/// by construction - no need to walk back up from the file it found.
fn dir_holding_runtime_lib(dir: &Path) -> Option<PathBuf> {
    find_runtime_lib_in_dir(dir).map(|_| dir.to_path_buf())
}

/// Search a release install layout: the library beside the binary, or under a
/// sibling `lib/` (optionally `lib/mux/`) as `scripts/install.sh` lays it out.
fn runtime_lib_near_executable() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    let mut candidates = vec![exe_dir.to_path_buf()];
    if let Some(prefix) = exe_dir.parent() {
        candidates.push(prefix.join("lib"));
        candidates.push(prefix.join("lib").join("mux"));
    }

    candidates
        .iter()
        .find_map(|dir| dir_holding_runtime_lib(dir))
}

/// Directory holding the runtime library to link.
///
/// Always a prebuilt library carrying mux-runtime's `full` feature set. The
/// compiler never builds a runtime at compile time: static linking pulls in
/// only the archive members a program actually references, so a feature-trimmed
/// runtime produces a byte-identical binary to the full one. Trimming bought
/// nothing and cost a source tree, a build cache, and a resolution order deep
/// enough to hide a broken install.
///
/// `MUX_RUNTIME_LIB` comes first so a specially built runtime can be forced -
/// `scripts/leak-check.sh` relies on this to link the `rc-leak-check` runtime,
/// which sits outside `full` and is the one build the compiler cannot produce.
fn resolve_runtime_lib_dir() -> Option<PathBuf> {
    runtime_lib_from_env()
        .or_else(runtime_lib_near_executable)
        .or_else(runtime_lib_from_build_config)
}

/// Colored ASCII status marker for doctor checks: green `[ok]` or red `[x]`.
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

/// Report the C driver used to link compiled programs.
///
/// Its version is informational. The compiler emits an object file, so any
/// driver that can link one works - a mismatch against the linked LLVM used to
/// mean IR parse errors and no longer means anything.
fn report_clang_for_doctor(linker: Option<&str>) -> bool {
    let Some(linker_cmd) = linker else {
        println!(
            "{} No C compiler found to link programs with.",
            status_marker(false)
        );
        print_linker_install_help();
        return false;
    };

    match extract_clang_major(linker_cmd) {
        Some(major) => println!(
            "{} Linker driver: {} (version {}).",
            status_marker(true),
            linker_cmd,
            major
        ),
        None => println!("{} Linker driver: {}.", status_marker(true), linker_cmd),
    }
    true
}

// Running a clang binary that was just written and chmod'd can transiently fail
// to exec (e.g. ETXTBSY on Linux, before the writer's file handle is fully
// released). Retry a few times so version detection - and the doctor test that
// writes a fake clang then execs it - is not flaky under load. A genuinely
// missing clang still fails every attempt and returns None.
fn clang_version_output(clang_cmd: &str) -> Option<std::process::Output> {
    let mut attempt = 0;
    loop {
        match Command::new(clang_cmd).arg("--version").output() {
            Ok(output) => return Some(output),
            Err(_) => {
                attempt += 1;
                if attempt >= 5 {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
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
    report_runtime_for_doctor(resolve_runtime_lib_dir().is_some())
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
    let linker = find_linker_command();
    let clang_ok = report_clang_for_doctor(linker.as_deref());
    let runtime_ok = ensure_runtime_for_doctor();

    if !print_doctor_verdict(llvm_ok && clang_ok && runtime_ok) {
        process::exit(1);
    }
}

/// Version of the C driver used to link programs, for the version banner.
fn get_linker_version() -> Option<String> {
    let linker = find_linker_command()?;
    let output = Command::new(&linker).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next()?;
    first_line
        .split_whitespace()
        .find(|s| s.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
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
    write!(out, "\x1b[{num_versions}B").ok();
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
    if let Some(ref c) = get_linker_version() {
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
    let mut buf = format!("\x1b[{offset}A");
    // Blank line before logo
    buf.push('\n');
    for char_row in combined {
        for (col, ch) in char_row.iter().enumerate().take(cols) {
            let s = palette.for_distance(cols - 1 - col);
            let _ = write!(&mut buf, "{s}{ch}{s:#}");
        }
        buf.push('\n');
    }
    buf.push('\n');
    let settled = &palette.settled;
    let _ = writeln!(&mut buf, "{settled}{BANNER_MSG}{settled:#}");
    buf
}

fn banner_settled_frame(combined: &[Vec<char>], settled: &anstyle::Style) -> String {
    let mut buf = format!("\x1b[{BANNER_ANIM_ROWS}A");
    buf.push('\n');
    for char_row in combined {
        let s: String = char_row.iter().collect();
        let _ = writeln!(&mut buf, "{settled}{s}{settled:#}");
    }
    buf.push('\n');
    let _ = writeln!(&mut buf, "{settled}{BANNER_MSG}{settled:#}");
    buf
}

fn parse_args_or_exit() -> (PathBuf, bool, Option<PathBuf>, bool, bool) {
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
        Commands::Explain { code } => {
            explain_diagnostic(code);
            process::exit(0);
        }
        Commands::Fix {
            file,
            dry_run,
            format,
        } => {
            let status = run_fix_command(file, *dry_run, *format, cli.deny_warnings);
            process::exit(status);
        }
        Commands::Build {
            file,
            output,
            intermediate,
        } => (
            file.clone(),
            false,
            output.clone(),
            *intermediate,
            cli.deny_warnings,
        ),
        Commands::Run {
            file,
            output,
            intermediate,
        } => (
            file.clone(),
            true,
            output.clone(),
            *intermediate,
            cli.deny_warnings,
        ),
        Commands::Format { file } => {
            eprintln!("formatting is not yet implemented for {}", file.display());
            process::exit(1);
        }
    }
}

fn fix_json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(&mut escaped, "\\u{:04x}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn print_fix_json(
    status: &str,
    dry_run: bool,
    diagnostics: &[Diagnostic],
    edits: &[diagnostic::TextEdit],
    changed_files: &[FileId],
    files: &Files,
    error: Option<&str>,
) {
    print_fix_json_with_truncation(
        status,
        dry_run,
        diagnostics,
        edits,
        changed_files,
        files,
        FixJsonMeta {
            error,
            truncated_count: 0,
        },
    );
}

struct FixJsonMeta<'a> {
    error: Option<&'a str>,
    truncated_count: usize,
}

fn print_fix_json_with_truncation(
    status: &str,
    dry_run: bool,
    diagnostics: &[Diagnostic],
    edits: &[diagnostic::TextEdit],
    changed_files: &[FileId],
    files: &Files,
    meta: FixJsonMeta<'_>,
) {
    let diagnostics = diagnostics
        .iter()
        .map(|diagnostic| {
            let level = match diagnostic.level {
                diagnostic::Level::Error => "error",
                diagnostic::Level::Warning => "warning",
            };
            let file = diagnostic
                .file_id
                .and_then(|file_id| files.path(file_id))
                .map_or_else(String::new, |path| path.display().to_string());
            format!(
                "{{\"code\":\"{}\",\"level\":\"{}\",\"file\":\"{}\",\"message\":\"{}\"}}",
                diagnostic.code,
                level,
                fix_json_escape(&file),
                fix_json_escape(&diagnostic.message)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let edits = edits
        .iter()
        .map(|edit| {
            let path = files
                .path(edit.file_id)
                .map_or_else(String::new, |path| path.display().to_string());
            format!(
                "{{\"code\":\"{}\",\"file\":\"{}\",\"start_byte\":{},\"end_byte\":{},\"replacement\":\"{}\",\"applicability\":\"{}\",\"solution_id\":{}}}",
                edit.diagnostic_code,
                fix_json_escape(&path),
                edit.range.start_byte,
                edit.range.end_byte,
                fix_json_escape(&edit.replacement),
                edit.applicability.as_str(),
                edit.solution_id
                    .map_or_else(|| "null".to_string(), |id| id.to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let changed_files = changed_files
        .iter()
        .filter_map(|file_id| files.path(*file_id))
        .map(|path| format!("\"{}\"", fix_json_escape(&path.display().to_string())))
        .collect::<Vec<_>>()
        .join(",");
    let error = meta.error.map_or_else(
        || "null".to_string(),
        |error| format!("\"{}\"", fix_json_escape(error)),
    );
    println!(
        "{{\"status\":\"{}\",\"dry_run\":{},\"diagnostics\":[{}],\"edits\":[{}],\"changed_files\":[{}],\"truncated_count\":{},\"error\":{}}}",
        status, dry_run, diagnostics, edits, changed_files, meta.truncated_count, error
    );
}

fn sort_fix_diagnostics(diagnostics: &mut [Diagnostic], files: &Files) {
    diagnostics.sort_by_key(|diagnostic| diagnostic::sort_key(diagnostic, files));
}

fn diagnostic_touches_recovery(
    diagnostic: &Diagnostic,
    files: &Files,
    recovery: &RecoveryIntervals,
) -> bool {
    let Some(file_id) = diagnostic.file_id else {
        return false;
    };
    let Some(source) = files.source(file_id) else {
        return false;
    };
    diagnostic.labels.iter().any(|label| {
        fix::source_range_for_span(source, label.span)
            .is_ok_and(|range| recovery.touches(file_id, range))
    })
}

fn materialize_span_edits(
    diagnostics: &[Diagnostic],
    files: &Files,
) -> Result<Vec<diagnostic::TextEdit>, String> {
    let mut edits = Vec::new();
    for diagnostic in diagnostics {
        for edit in &diagnostic.span_edits {
            if !matches!(
                edit.applicability,
                diagnostic::Applicability::MachineApplicable
            ) {
                continue;
            }
            let target_file = edit.target_file.or(diagnostic.file_id).ok_or_else(|| {
                format!(
                    "{} supplied an edit without an originating file",
                    diagnostic.code
                )
            })?;
            let target_source = files.source(target_file).ok_or_else(|| {
                format!(
                    "{} supplied an edit for an unknown target file",
                    diagnostic.code
                )
            })?;
            let target = fix::source_range_for_span(target_source, edit.target)
                .map_err(|error| format!("{}: {error}", diagnostic.code))?;
            let replacement = match &edit.replacement {
                diagnostic::EditReplacement::Text(text) => text.clone(),
                diagnostic::EditReplacement::Source(source_span) => {
                    let replacement_file = edit.replacement_file.unwrap_or(target_file);
                    let replacement_source = files.source(replacement_file).ok_or_else(|| {
                        format!(
                            "{} supplied an unknown source replacement file",
                            diagnostic.code
                        )
                    })?;
                    let source_range = fix::source_range_for_span(replacement_source, *source_span)
                        .map_err(|error| format!("{}: {error}", diagnostic.code))?;
                    replacement_source
                        .get(source_range.start_byte..source_range.end_byte)
                        .ok_or_else(|| {
                            format!(
                                "{} supplied a source replacement outside the file",
                                diagnostic.code
                            )
                        })?
                        .to_string()
                }
            };
            edits.push(
                diagnostic::TextEdit::machine_applicable(
                    target_file,
                    target,
                    replacement,
                    edit.diagnostic_code,
                )
                .with_solution(edit.solution_id),
            );
        }
    }
    Ok(edits)
}

fn validate_staged_sources(
    root_path: &Path,
    root_file_id: FileId,
    files: &Files,
    updates: &std::collections::BTreeMap<FileId, String>,
) -> Result<(), String> {
    let mut staged_files = Files::new();
    let mut source_by_path = std::collections::HashMap::new();
    for (file_id, path, source) in files.iter() {
        let staged_source = updates.get(&file_id).map_or(source, String::as_str);
        staged_files.add(path, staged_source.to_string());
        if !path.to_string_lossy().starts_with("<embedded>/")
            && let Ok(canonical) = fs::canonicalize(path)
        {
            source_by_path.insert(canonical, staged_source.to_string());
        }
    }

    let root_source = updates
        .get(&root_file_id)
        .map(String::as_str)
        .or_else(|| files.source(root_file_id))
        .ok_or_else(|| format!("root file {} is not registered", root_path.display()))?;

    let mut source = Source::from_string(root_source.to_string());
    let mut lexer = lexer::Lexer::new(&mut source);
    let tokens = lexer.lex_all().map_err(|error| error.message.to_string())?;
    let mut parser = parser::Parser::new(&tokens);
    let nodes = parser.parse().map_err(|(_, errors)| {
        errors
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>()
            .join("; ")
    })?;

    let base_path = root_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let resolver = Rc::new(RefCell::new(ModuleResolver::new(base_path)));
    let mut resolver_state = resolver.borrow_mut();
    resolver_state.set_source_overrides(source_by_path);
    resolver_state.set_emit_diagnostics(false);
    drop(resolver_state);
    let mut analyzer = semantics::SemanticAnalyzer::new_with_resolver(resolver);
    if let Some(staged_root_id) = staged_files.id_for_path(root_path) {
        analyzer.set_current_file_id(staged_root_id);
    }
    let errors = analyzer.analyze(&nodes, Some(&mut staged_files));
    let imported_errors = analyzer.take_imported_errors();
    let blocking = errors
        .iter()
        .chain(imported_errors.iter())
        .filter(|error| error.code.level() == diagnostic::Level::Error)
        .collect::<Vec<_>>();
    if blocking.is_empty() {
        Ok(())
    } else {
        let messages = blocking
            .iter()
            .map(|error| error.message.clone())
            .collect::<Vec<_>>();
        Err(messages.join("; "))
    }
}

fn parse_fix_source(
    source: &str,
    file_id: FileId,
) -> Result<(Vec<ast::AstNode>, RecoveryIntervals, Vec<Diagnostic>), Vec<Diagnostic>> {
    let mut source_cursor = Source::from_string(source.to_string());
    let mut lexer = lexer::Lexer::new(&mut source_cursor);
    let tokens = lexer
        .lex_all()
        .map_err(|error| vec![error.to_diagnostic(file_id)])?;

    let mut parser = parser::Parser::new(&tokens);
    match parser.parse() {
        Ok(nodes) => Ok((nodes, RecoveryIntervals::new(), Vec::new())),
        Err((partial_nodes, errors)) => {
            let mut recovery = RecoveryIntervals::new();
            let diagnostics = errors
                .into_iter()
                .map(|error| {
                    if let Ok(range) = fix::source_range_for_span(source, error.span) {
                        recovery.add(file_id, range);
                    }
                    error.to_diagnostic(file_id)
                })
                .collect();
            for span in parser.recovery_spans() {
                if let Ok(range) = fix::source_range_for_span(source, *span) {
                    recovery.add(file_id, range);
                }
            }
            Ok((partial_nodes, recovery, diagnostics))
        }
    }
}

fn analyze_fix_source(
    file_path: &Path,
    source: &str,
    file_id: FileId,
    files: &mut Files,
) -> Result<(RecoveryIntervals, Vec<Diagnostic>), Vec<Diagnostic>> {
    let (nodes, mut recovery, mut diagnostics) = parse_fix_source(source, file_id)?;
    let base_path = file_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let resolver = Rc::new(RefCell::new(ModuleResolver::new(base_path)));
    resolver.borrow_mut().set_emit_diagnostics(false);
    let mut analyzer = semantics::SemanticAnalyzer::new_with_resolver(resolver.clone());
    analyzer.set_current_file_id(file_id);
    let errors = analyzer.analyze(&nodes, Some(files));
    let imported_errors = analyzer.take_imported_errors();
    let (module_diagnostics, module_recovery) = {
        let mut resolver = resolver.borrow_mut();
        (
            resolver.take_diagnostics(),
            resolver.take_recovery_intervals(),
        )
    };
    recovery.extend(module_recovery);
    diagnostics.extend(module_diagnostics);
    diagnostics.extend(
        errors
            .iter()
            .map(|error| error.to_diagnostic(file_id))
            .chain(
                imported_errors
                    .iter()
                    .map(|error| error.to_diagnostic(file_id)),
            )
            .filter(|diagnostic| !diagnostic_touches_recovery(diagnostic, files, &recovery)),
    );
    Ok((recovery, diagnostics))
}

fn run_fix_command(
    file_path: &Path,
    dry_run: bool,
    format: FixOutputFormat,
    deny_warnings: bool,
) -> i32 {
    if !file_path.to_string_lossy().ends_with(".mux") {
        report_fix_error(
            format,
            dry_run,
            &[],
            &[],
            &Files::new(),
            "Input file must have a .mux extension.",
        );
        return 1;
    }

    let source = match fs::read_to_string(file_path) {
        Ok(source) => source,
        Err(error) => {
            let message = format!("Error opening file: {error}");
            report_fix_error(format, dry_run, &[], &[], &Files::new(), &message);
            return 1;
        }
    };

    let mut files = Files::new();
    let file_id = files.add(file_path, source.clone());
    let (recovery, mut diagnostics) =
        match analyze_fix_source(file_path, &source, file_id, &mut files) {
            Ok(result) => result,
            Err(diagnostics) => {
                emit_fix_diagnostics(format, &diagnostics, &files, 0);
                return 1;
            }
        };
    sort_fix_diagnostics(&mut diagnostics, &files);

    let has_blocking_diagnostics = diagnostics.iter().any(|diagnostic| {
        diagnostic.level == diagnostic::Level::Error
            || (deny_warnings && diagnostic.level == diagnostic::Level::Warning)
    });
    let truncated_count = diagnostics.len().saturating_sub(MAX_DIAGNOSTICS);
    diagnostics.truncate(MAX_DIAGNOSTICS);
    let edits = match collect_fix_edits(&diagnostics, &files) {
        Ok(edits) => edits,
        Err(error) => {
            report_fix_error(format, dry_run, &diagnostics, &[], &files, &error);
            return 1;
        }
    };
    emit_fix_diagnostics(format, &diagnostics, &files, truncated_count);

    if has_blocking_diagnostics {
        if matches!(format, FixOutputFormat::Json) {
            print_fix_json("error", dry_run, &diagnostics, &edits, &[], &files, None);
        }
        return 1;
    }

    let (applied, diff) =
        match apply_fix_transaction(file_path, file_id, &files, &edits, &recovery, dry_run) {
            Ok(result) => result,
            Err(error) => {
                report_fix_error(format, dry_run, &diagnostics, &edits, &files, &error);
                return 1;
            }
        };

    emit_fix_result(
        format,
        dry_run,
        FixResult {
            diagnostics: &diagnostics,
            edits: &edits,
            files: &files,
            applied: &applied,
            diff: &diff,
            truncated_count,
        },
    );
    0
}

fn report_fix_error(
    format: FixOutputFormat,
    dry_run: bool,
    diagnostics: &[Diagnostic],
    edits: &[diagnostic::TextEdit],
    files: &Files,
    error: &str,
) {
    if matches!(format, FixOutputFormat::Json) {
        print_fix_json(
            "error",
            dry_run,
            diagnostics,
            edits,
            &[],
            files,
            Some(error),
        );
    } else {
        eprintln!("mux fix: {error}");
    }
}

fn emit_fix_diagnostics(
    format: FixOutputFormat,
    diagnostics: &[Diagnostic],
    files: &Files,
    truncated_count: usize,
) {
    if matches!(format, FixOutputFormat::Text) && !diagnostics.is_empty() {
        StandardEmitter::new(ColorConfig::Auto).emit_batch(diagnostics, files);
        if truncated_count > 0 {
            eprintln!(
                "warning: output truncated; {truncated_count} additional diagnostics omitted (maximum is {MAX_DIAGNOSTICS})"
            );
        }
    }
}

fn collect_fix_edits(
    diagnostics: &[Diagnostic],
    files: &Files,
) -> Result<Vec<diagnostic::TextEdit>, String> {
    let mut edits = diagnostics
        .iter()
        .flat_map(|diagnostic| diagnostic.edits.iter().cloned())
        .collect::<Vec<_>>();
    edits.extend(materialize_span_edits(diagnostics, files)?);
    edits.sort_by_key(|edit| {
        (
            edit.file_id,
            edit.range.start_byte,
            edit.range.end_byte,
            edit.diagnostic_code,
        )
    });
    Ok(edits)
}

fn apply_fix_transaction(
    file_path: &Path,
    file_id: FileId,
    files: &Files,
    edits: &[diagnostic::TextEdit],
    recovery: &RecoveryIntervals,
    dry_run: bool,
) -> Result<(fix::AppliedEdits, String), String> {
    let applied = fix::apply_and_validate(files, edits, recovery, |updates| {
        validate_staged_sources(file_path, file_id, files, updates)
    })
    .map_err(|error| error.to_string())?;
    let diff = fix::unified_diff(files, &applied);
    if !dry_run && !applied.changed_files.is_empty() {
        fix::write_transaction(files, &applied.files).map_err(|error| error.to_string())?;
    }
    Ok((applied, diff))
}

struct FixResult<'a> {
    diagnostics: &'a [Diagnostic],
    edits: &'a [diagnostic::TextEdit],
    files: &'a Files,
    applied: &'a fix::AppliedEdits,
    diff: &'a str,
    truncated_count: usize,
}

fn emit_fix_result(format: FixOutputFormat, dry_run: bool, result: FixResult<'_>) {
    if matches!(format, FixOutputFormat::Json) {
        let status = if result.applied.changed_files.is_empty() {
            "no_changes"
        } else if dry_run {
            "dry_run"
        } else {
            "applied"
        };
        print_fix_json_with_truncation(
            status,
            dry_run,
            result.diagnostics,
            result.edits,
            &result.applied.changed_files,
            result.files,
            FixJsonMeta {
                error: None,
                truncated_count: result.truncated_count,
            },
        );
    } else if result.applied.changed_files.is_empty() {
        println!("No machine-applicable fixes available.");
    } else {
        print!("{}", result.diff);
        let action = if dry_run { "Would apply" } else { "Applied" };
        println!(
            "{action} {} edit(s) to {} file(s).",
            result.edits.len(),
            result.applied.changed_files.len()
        );
    }
}

fn explain_diagnostic(value: &str) {
    let Some(code) = DiagnosticCode::parse(&value.to_ascii_uppercase()) else {
        eprintln!("Unknown diagnostic code '{value}'. Use a code such as E0302 or W0300.");
        process::exit(2);
    };
    let info = code.info();
    println!("{}: {}", info.code, info.title);
    println!();
    println!("When: {}", info.trigger);
    if !info.example.is_empty() {
        println!("Example:\n{}", info.example);
    }
    println!("Why: {}", info.explanation);
    println!("Fix: {}", info.fix);
    println!("Docs: https://mux-lang.dev/docs/reference/diagnostics/");
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
            emit_diagnostics(files, file_id, &[err], false);
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
            emit_diagnostics(files, file_id, &errors, false);
            process::exit(1);
        }
    }
}

fn analyze_semantics_or_exit(
    analyzer: &mut semantics::SemanticAnalyzer,
    nodes: &[ast::AstNode],
    file_id: FileId,
    files: &mut Files,
    deny_warnings: bool,
) {
    let errors = analyzer.analyze(nodes, Some(files));
    let imported_errors = analyzer.take_imported_errors();
    let mut diagnostics = errors
        .iter()
        .chain(imported_errors.iter())
        .map(|error| error.to_diagnostic(file_id))
        .collect::<Vec<_>>();
    if let Some(resolver) = analyzer.module_resolver.as_ref() {
        diagnostics.extend(resolver.borrow_mut().take_diagnostics());
    }
    if !diagnostics.is_empty() && emit_diagnostic_batch(files, &diagnostics, deny_warnings) {
        process::exit(1);
    }
}

/// Run codegen, then write the object file the linker consumes - and, when
/// `-i` asked for it, the readable `.ll` alongside it.
///
/// Only the object is on the critical path. The textual IR used to be, which is
/// what tied an install to a clang matching this compiler's LLVM major.
/// Returns rather than exiting, so the caller can release the object handle
/// first. `process::exit` skips destructors, and on Windows that handle is what
/// deletes the file - exiting while holding it would leave the object in the
/// temp directory for good.
fn generate_object(
    codegen: &mut codegen::CodeGenerator,
    nodes: &[ast::AstNode],
    object: &mut fs::File,
    ir_file: Option<&str>,
) -> Result<(), String> {
    // A codegen failure is an internal compiler bug, not a language-level error
    // in the user's program, so callers give it the internal-error framing
    // rather than a bare message.
    codegen
        .generate(nodes)
        .map_err(|e| format!("codegen error: {e}"))?;

    // IR first: when the module is invalid, the emitted `.ll` is exactly what is
    // needed to debug it, and object emission verifies and would bail first.
    if let Some(ir_file) = ir_file {
        codegen
            .emit_ir_to_file(ir_file)
            .map_err(|e| format!("failed to emit IR: {e}"))?;
    }

    codegen
        .emit_object(object)
        .map_err(|e| format!("failed to emit object file: {e}"))?;

    // The bytes must be on disk, and read from the start: on unix the handle
    // becomes the linker's stdin, on Windows it is reopened by path.
    object
        .sync_all()
        .and_then(|()| {
            use std::io::Seek;
            object.rewind()
        })
        .map_err(|e| format!("failed to finalize the object file: {e}"))
}

fn resolve_runtime_lib_dir_or_exit() -> PathBuf {
    match resolve_runtime_lib_dir() {
        Some(dir) => dir,
        None => {
            spinner::stop();
            eprintln!();
            eprintln!("Could not locate the mux-runtime library.");
            eprintln!("A release install ships it beside the compiler, in ../lib.");
            eprintln!("In a source checkout, build it with:");
            eprintln!("  cargo build -p mux-runtime");
            eprintln!("(cargo builds only a dependency's rlib, not this staticlib.)");
            eprintln!("Otherwise point MUX_RUNTIME_LIB at a built library:");
            eprintln!("  MUX_RUNTIME_LIB=/path/to/libmux_runtime.a mux run file.mux");
            process::exit(1);
        }
    }
}

fn find_linker_or_exit() -> String {
    match find_linker_command() {
        Some(cmd) => cmd,
        None => {
            spinner::stop();
            eprintln!("A C compiler is required to link Mux programs, and none was found.");
            print_linker_install_help();
            process::exit(1);
        }
    }
}

/// Build the internal-error detail line for a failed link from clang's output.
///
/// Both streams are reported because the linker's own diagnostics do not land on
/// the same one everywhere: `ld` and `lld` write to stderr, while MSVC's
/// `link.exe` writes to stdout. Reading stderr alone on Windows reduces the whole
/// report to "linker command failed with exit code 1104" and drops the `LNK1104:
/// cannot open file '...'` line that says what is actually wrong.
///
/// Trailing whitespace is trimmed so the report does not end with a blank line;
/// pure so the wording is unit-tested without spawning clang.
fn clang_failure_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut parts = Vec::new();
    for stream in [stderr, stdout] {
        let text = String::from_utf8_lossy(stream);
        let trimmed = text.trim_end();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    format!("linking failed: {}", parts.join("\n"))
}

/// What the driver should do about clang's result. Decided without side effects
/// so the branching is unit-testable; the exit/report wiring stays in the thin
/// `report_clang_output_or_exit` wrapper.
enum ClangOutcome {
    /// The executable linked successfully.
    Linked,
    /// A non-zero clang exit: the IR we emitted did not link, which is a compiler
    /// bug rather than a user mistake, so it becomes an internal-error report.
    LinkFailed(String),
    /// clang was found but could not be executed - an environment problem. The
    /// message keeps the IR path so the user can retry.
    SpawnFailed(String),
}

fn classify_clang_output(
    clang_output: std::io::Result<std::process::Output>,
    ir_file: &str,
) -> ClangOutcome {
    match clang_output {
        Ok(output) if output.status.success() => ClangOutcome::Linked,
        Ok(output) => {
            ClangOutcome::LinkFailed(clang_failure_detail(&output.stdout, &output.stderr))
        }
        Err(e) => ClangOutcome::SpawnFailed(format!(
            "Failed to run clang: {e}. IR file generated at: {ir_file}"
        )),
    }
}

fn report_clang_output_or_exit(
    clang_output: std::io::Result<std::process::Output>,
    _do_run: bool,
    _file_path: &Path,
    ir_file: &str,
) {
    match classify_clang_output(clang_output, ir_file) {
        ClangOutcome::Linked => {}
        ClangOutcome::LinkFailed(detail) => {
            spinner::stop();
            report_internal_compiler_error(&detail);
            process::exit(1);
        }
        ClangOutcome::SpawnFailed(message) => {
            spinner::stop();
            eprintln!("{}", message);
            process::exit(1);
        }
    }
}

/// Remove the named scratch object, warning rather than failing if it will not
/// go.
///
/// Only the platforms that keep a name need this; unix released it at creation.
/// By the time this runs the link has already happened, so a stuck temp file is
/// untidy rather than fatal and must not change the exit status - but it should
/// not be silent either. On Windows an antivirus scanner can hold a transient
/// lock on a freshly written object, and a discarded error there means these
/// accumulate in the temp directory with nothing ever saying so.
#[cfg(not(unix))]
fn remove_scratch_object(path: &Path) {
    if let Err(e) = fs::remove_file(path) {
        eprintln!(
            "warning: could not remove the temporary object file {}: {e}",
            path.display()
        );
    }
}

/// How the linker refers to an object it reads from inherited stdin. `/dev/fd` is
/// standard on Linux and the BSDs, macOS included, and `/dev/stdin` is the
/// descriptor-0 entry within it.
#[cfg(unix)]
const LINKER_STDIN_OBJECT: &str = "/dev/stdin";

/// An open handle to the object file, and the argument naming it to the linker.
///
/// A file directly in the temp directory, with no directory of our own in
/// between. Two properties make that safe, and an intermediate component would
/// lose both:
///
/// - `create_new` is `O_CREAT|O_EXCL`, so an existing file or symlink here is an
///   error, never a write redirected through it. Nothing is opened that this
///   process did not create, and the handle is returned so no path is resolved
///   again afterwards.
/// - Symlink traversal applies only to *non-final* path components, and `unlink`
///   never follows the final one. So cleanup cannot reach outside this path even
///   if the entry is swapped later: it would remove the replacement link itself,
///   not whatever it points at.
///
/// Not `<source>.o` - that would clobber a `foo.o` the user already had beside
/// `foo.mux`. The pid and a monotonic counter keep concurrent compiles from
/// colliding (the test suite runs many at once), and exclusive creation turns
/// any collision that does occur into a retry rather than an overwrite.
///
/// The two platforms then diverge, because only one of them can hand the linker
/// a descriptor. Unix unlinks immediately and passes the open descriptor as the
/// child's stdin, so no name exists to race against. Windows has no equivalent:
/// the linker must open the object by path, so the file keeps its name, the
/// handle is closed before the link, and the caller removes it afterwards.
fn create_scratch_object(stem: &str) -> Result<(PathBuf, fs::File), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let name = Path::new(stem).file_name().map_or_else(
        || "mux_module".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );

    for _ in 0..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let candidate = env::temp_dir().join(format!(
            "mux-{}-{}-{}-{}.o",
            name,
            process::id(),
            nanos,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        // Deliberately NOT FILE_FLAG_DELETE_ON_CLOSE on Windows. That flag looks
        // ideal - the OS reclaims the file with no delete-by-path at all - but it
        // changes how OTHER processes may open the file: while such a handle is
        // open, any opener that does not itself pass FILE_SHARE_DELETE gets a
        // sharing violation. link.exe does not, so it failed with
        // "LNK1104: cannot open file" on an object that existed and was readable.
        // The share mode on THIS handle cannot grant that; only the opener's can.
        match options.open(&candidate) {
            Ok(file) => {
                #[cfg(unix)]
                {
                    // Drop the name straight away. From here the object exists
                    // only as this descriptor, so there is no path for anything
                    // to replace and nothing to unlink afterwards - the file is
                    // released when the descriptor closes. The linker reads it as
                    // `/dev/stdin`, which is why the handle becomes the child's
                    // stdin rather than being closed here.
                    if let Err(e) = fs::remove_file(&candidate) {
                        return Err(format!(
                            "failed to unlink the temporary object file {}: {}",
                            candidate.display(),
                            e
                        ));
                    }
                    return Ok((PathBuf::from(LINKER_STDIN_OBJECT), file));
                }
                #[cfg(not(unix))]
                return Ok((candidate, file));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "failed to create a temporary object file at {}: {}",
                    candidate.display(),
                    e
                ));
            }
        }
    }

    Err("could not create a temporary object file".to_string())
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

/// The `.mux` file currently being compiled, so an internal failure (a codegen
/// error or a panic) can point the user at the input that triggered it. Set once
/// after argument parsing; read from the panic hook, which has no other handle
/// on it.
static COMPILING_FILE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Render `path` relative to the current directory when it is absolute, matching
/// how `diagnostic::Files::add` renders diagnostic paths, so a reported path is
/// identical across machines and CI rather than an absolute developer path. A
/// path outside the current directory (or already relative) is returned as-is.
fn relativize_to_cwd(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(env::current_dir().unwrap_or_else(|_| path.to_path_buf()))
            .unwrap_or(path)
            .to_path_buf()
    } else {
        path.to_path_buf()
    }
}

fn set_compiling_file(path: &Path) {
    if let Ok(mut slot) = COMPILING_FILE.lock() {
        *slot = Some(relativize_to_cwd(path).display().to_string());
    }
}

fn compiling_file() -> Option<String> {
    COMPILING_FILE.lock().ok().and_then(|slot| slot.clone())
}

/// Report an internal compiler failure - a codegen error or a panic - in a way
/// that makes clear it is a bug in mux, not a mistake in the user's program, and
/// points them at where to report it. Language-level problems (lex, parse,
/// semantics) do NOT use this: those are the user's code and keep their normal
/// diagnostics. `detail` is the underlying error string or panic message.
/// Build the internal-compiler-error report text. Split from the printing
/// wrapper so its wording - which file line it shows, and whether it points at
/// "the file above" or the `RUST_BACKTRACE` hint - is unit-testable without
/// capturing stderr or a real panic.
fn internal_compiler_error_report(
    detail: &str,
    file: Option<&str>,
    show_backtrace_hint: bool,
) -> String {
    let mut out = format!(
        "\nerror[{}]: internal compiler error\n",
        DiagnosticCode::InternalCompiler
    );
    if !detail.is_empty() {
        // Indent every line: a linking failure feeds multi-line clang/linker
        // stderr in as `detail`, and indenting only the first line would leave
        // the rest starting at column 0 (a panic message is always single-line).
        for line in detail.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    if let Some(file) = file {
        let _ = writeln!(&mut out, "  while compiling: {file}");
    }
    // The "tip:" wording requested in mux-context#24: point the user at where to
    // report and what to include, without implying their code is at fault. Only
    // say "the file above" when a file line was actually emitted; a panic before
    // argument parsing has no file to show.
    let subject = if file.is_some() {
        "the file above"
    } else {
        "the file you ran"
    };
    let _ = writeln!(
        &mut out,
        "tip: please report this error, along with {subject} and your system details"
    );
    out.push_str(
        "     (the output of `mux --version`), to the Mux maintainers at https://github.com/muxlang/mux-compiler\n",
    );
    if show_backtrace_hint {
        out.push_str("     re-run with RUST_BACKTRACE=1 to include the internal error details\n");
    }
    out
}

fn report_internal_compiler_error(detail: &str) {
    let file = compiling_file();
    let show_backtrace_hint = env::var_os("RUST_BACKTRACE").is_none();
    eprint!(
        "{}",
        internal_compiler_error_report(detail, file.as_deref(), show_backtrace_hint)
    );
}

/// Format a panic's message and optional source location into a concise
/// `&lt;message&gt; (at &lt;file&gt;:&lt;line&gt;:&lt;col&gt;)` detail line. Pure, so it is unit-tested
/// directly; `panic_detail` handles pulling these fields out of the hook info.
fn format_panic_detail(message: &str, location: Option<(&str, u32, u32)>) -> String {
    match location {
        Some((file, line, col)) => format!("{message} (at {file}:{line}:{col})"),
        None => message.to_string(),
    }
}

/// Extract a concise description from a panic, for the friendly internal-error
/// report.
fn panic_detail(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unexpected internal panic".to_string());
    let location = info
        .location()
        .map(|loc| (loc.file(), loc.line(), loc.column()));
    format_panic_detail(&message, location)
}

/// Install a panic hook that reframes any compiler panic (a failed `assert`,
/// `unwrap`, `expect`, or `unreachable!`) as an internal compiler error rather
/// than letting Rust's raw "thread 'main' panicked" text reach the user. The
/// full panic and backtrace are preserved for maintainers when `RUST_BACKTRACE` is
/// set; the friendly message is the default.
fn install_internal_error_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        spinner::stop();
        if env::var_os("RUST_BACKTRACE").is_some() {
            default_hook(info);
        }
        report_internal_compiler_error(&panic_detail(info));
    }));
}

/// Native libraries a statically linked runtime needs, by target OS.
///
/// Takes the OS as a parameter rather than reading `cfg!` inline so that every
/// platform's list is reachable from a test on any host. The Windows list in
/// particular is unreachable on the Linux runners CI uses, so inline `cfg!`
/// left it verified by nothing - and it was wrong for months, failing every
/// Windows compile with LNK2019 on symbols these libraries own.
fn native_runtime_deps(target_os: &str) -> &'static [&'static str] {
    match target_os {
        // libSystem already provides these.
        "macos" => &[],
        // What Rust's std and the runtime's vendored C code reference:
        // bcrypt/advapi32 for getrandom, ntdll for pipes, userenv for the home
        // directory, secur32 for the user name, ws2_32 for sockets, dbghelp for
        // backtraces.
        "windows" => &[
            "-ladvapi32",
            "-lbcrypt",
            "-ldbghelp",
            "-lkernel32",
            "-lntdll",
            "-lole32",
            "-loleaut32",
            "-lsecur32",
            "-lshell32",
            "-luserenv",
            "-luuid",
            "-lws2_32",
        ],
        // libm for `**`/pow, and the rest for what the runtime's C deps pull in
        // (issue #291).
        _ => &["-lm", "-lz", "-lpthread", "-ldl"],
    }
}

/// Build the clang argument list that links the emitted object into an
/// executable, minus the output path.
///
/// Extracted from `main` because the platform differences here are genuinely
/// three-way - ELF, Mach-O and PE each want a different spelling of the same
/// three ideas - and inlining them pushed `main` past the cognitive-complexity
/// budget. Keeping them together also means the whole link line can be read in
/// one place.
fn build_linker_args(object_file: &Path, lib_dir: &Path) -> Vec<std::ffi::OsString> {
    build_linker_args_for(env::consts::OS, object_file, lib_dir)
}

fn append_linker_output(args: &mut Vec<std::ffi::OsString>, output: &Path) {
    args.push("-o".into());
    args.push(output.as_os_str().to_owned());
}

/// The link line for a given target OS.
///
/// The OS is a parameter rather than a `cfg!` for the same reason as
/// `native_runtime_deps`: a `cfg`-gated branch is not compiled on other hosts,
/// so every platform's dialect except the host's is unreachable from a test. CI
/// runs on Linux, and the Windows spelling of these flags was wrong for months
/// with nothing able to catch it.
fn build_linker_args_for(
    target_os: &str,
    object_file: &Path,
    lib_dir: &Path,
) -> Vec<std::ffi::OsString> {
    let windows = target_os == "windows";
    let macos = target_os == "macos";

    let mut linker_args = vec![
        object_file.as_os_str().to_owned(),
        "-L".into(),
        lib_dir.as_os_str().to_owned(),
        "-ffunction-sections".into(),
        "-fdata-sections".into(),
    ];

    if windows {
        // Put the whole link on the DYNAMIC CRT, which is what everything else
        // in it already expects.
        //
        // clang's driver passes `-defaultlib:libcmt` unconditionally when it
        // links on Windows - the STATIC CRT - and that cannot be changed by
        // asking politely. `-fms-runtime-lib=dll` only rewrites the
        // `--dependent-lib` directive clang bakes into objects IT compiles; here
        // clang compiles nothing, it links a pre-built object against an
        // archive, so the flag has no effect at all. Verified from the driver's
        // own `-###` output, which shows `-defaultlib:libcmt` present with and
        // without it.
        //
        // Meanwhile mux_runtime.lib's members all request MSVCRT, so libcmt
        // drags in the static libucrt and the link ends up with two CRTs:
        //
        //   LNK4098: defaultlib 'MSVCRT' conflicts with use of other libs
        //   LNK4217: symbol 'free' defined in 'libucrt.lib' is imported by ...
        //   LNK2019: unresolved external symbol __imp_realloc
        //
        // That last one is the giveaway: libucrt exports `realloc`, never
        // `__imp_realloc`, because the `__imp_` form only exists in the import
        // library. So the driver's default has to be countermanded at link time
        // rather than influenced at compile time.
        linker_args.push("-Wl,/NODEFAULTLIB:libcmt".into());
        linker_args.push("-Wl,/NODEFAULTLIB:libucrt".into());
        linker_args.push("-Wl,/DEFAULTLIB:msvcrt".into());
    } else {
        // rpath and the dtags flag are ELF concepts. MSVC's linker answers both
        // with "LNK4044: unrecognized option" and ignores them; Windows resolves
        // a DLL from the executable's own directory, which is where a packaged
        // install puts the runtime.
        let mut rpath = std::ffi::OsString::from("-Wl,-rpath,");
        rpath.push(lib_dir.as_os_str());
        linker_args.push(rpath);
        if !macos {
            linker_args.push("-Wl,--disable-new-dtags".into());
        }

        // Dead-stripping is spelled differently per linker, and MSVC's does it
        // by default at the optimisation levels that matter, so Windows passes
        // nothing rather than an option that would only be warned about.
        linker_args.push(if macos {
            "-Wl,-dead_strip".into()
        } else {
            "-Wl,--gc-sections".into()
        });
    }

    let runtime_static = runtime_static_lib_path(lib_dir);
    let expected_static_name = if windows {
        "mux_runtime.lib"
    } else {
        "libmux_runtime.a"
    };
    if runtime_static.exists()
        && runtime_static.file_name().and_then(std::ffi::OsStr::to_str)
            != Some(expected_static_name)
    {
        linker_args.push(runtime_static.into_os_string());
    } else {
        linker_args.push("-lmux_runtime".into());
    }

    // A static runtime carries no record of its own dependencies, so its
    // undefined native symbols must be resolved explicitly - otherwise a program
    // using libm (e.g. `**`/`pow`) fails to link with "undefined reference to
    // pow" (issue #291). These must follow -lmux_runtime so the archive's
    // references are satisfied by the libraries after it.
    if runtime_lib_dir_is_static_only(lib_dir) {
        for native_lib in native_runtime_deps(target_os) {
            linker_args.push((*native_lib).into());
        }
    }

    linker_args
}

fn main() {
    install_internal_error_panic_hook();
    let (file_path, do_run, output, intermediate, deny_warnings) = parse_args_or_exit();
    set_compiling_file(&file_path);

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
    resolver.borrow_mut().set_emit_diagnostics(false);

    let mut analyzer = semantics::SemanticAnalyzer::new_with_resolver(resolver);
    analyze_semantics_or_exit(&mut analyzer, &nodes, file_id, &mut files, deny_warnings);

    let context = inkwell::context::Context::create();
    let source_name = file_path.file_name().map_or_else(
        || file_path.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut codegen = codegen::CodeGenerator::new(&context, &mut analyzer, &source_name);

    let stem = file_path
        .to_string_lossy()
        .trim_end_matches(".mux")
        .to_string();
    let ir_file = format!("{stem}.ll");

    // Resolved before the scratch directory exists and before codegen runs.
    // Neither depends on codegen, both exit the process on failure, and
    // `process::exit` skips destructors - so anything created first would be
    // left behind. Failing before the expensive work is better regardless.
    let lib_dir = resolve_runtime_lib_dir_or_exit();
    let linker_cmd = find_linker_or_exit();

    let (object_file, mut object) = match create_scratch_object(&stem) {
        Ok(pair) => pair,
        Err(e) => {
            spinner::stop();
            report_internal_compiler_error(&e);
            process::exit(1);
        }
    };
    if let Err(e) = generate_object(
        &mut codegen,
        &nodes,
        &mut object,
        intermediate.then_some(ir_file.as_str()),
    ) {
        // `process::exit` skips destructors, so both of these are explicit.
        // Unix released the name at creation and needs only the handle dropped;
        // elsewhere the name is still on disk and has to be removed too, or a
        // codegen failure leaves the object behind for good.
        drop(object);
        spinner::stop();
        #[cfg(not(unix))]
        remove_scratch_object(&object_file);
        report_internal_compiler_error(&e);
        process::exit(1);
    }

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
        let source_path = file_path.with_extension("");
        let parent = source_path.parent().unwrap_or(Path::new("."));
        parent.join(
            source_path
                .file_stem()
                .unwrap_or_else(|| source_path.as_os_str()),
        )
    };

    let mut linker_args = build_linker_args(&object_file, &lib_dir);

    append_linker_output(&mut linker_args, &exe_file);

    let mut linker = Command::new(&linker_cmd);
    linker.args(&linker_args);
    #[cfg(unix)]
    {
        // `/dev/stdin` in linker_args refers to this descriptor. Moving the
        // handle in also closes this process's copy once the child has it, so the
        // file is released as soon as linking finishes - no cleanup path at all.
        linker.stdin(Stdio::from(object));
    }

    // Elsewhere the linker opens the object by PATH, so this handle must be
    // closed BEFORE the link, not after: an open writer blocks link.exe from
    // opening it and surfaces as "LNK1104: cannot open file". Closing here also
    // flushes the object to disk, which the linker is about to read.
    #[cfg(not(unix))]
    drop(object);

    let linker_output = linker.output();

    spinner::stop();

    // The name outlives the handle now, so it has to be removed explicitly, and
    // before report_clang_output_or_exit - that exits the process on a link
    // failure, which would otherwise leak the object into the temp directory on
    // exactly the runs that produce one. After the spinner stops, so a warning
    // cannot interleave with it.
    #[cfg(not(unix))]
    remove_scratch_object(&object_file);

    report_clang_output_or_exit(linker_output, do_run, &file_path, &ir_file);

    if do_run {
        run_executable_or_exit(&exe_file);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        REQUIRED_LLVM_MAJOR, append_linker_output, build_linker_args, build_linker_args_for,
        clang_failure_detail, clang_version_output, compiling_file, dir_holding_runtime_lib,
        extract_clang_major, find_runtime_lib_in_dir, format_panic_detail,
        internal_compiler_error_report, llvm_config_candidates, materialize_span_edits,
        native_runtime_deps, pick_llvm_for_dev, print_doctor_verdict, print_version_banner,
        relativize_to_cwd, report_clang_for_doctor, report_runtime_for_doctor,
        runtime_lib_dir_is_static_only, set_compiling_file, status_marker,
        validate_llvm_for_doctor,
    };
    use crate::diagnostic::{Diagnostic, DiagnosticCode, Files, SpanEdit};
    use crate::lexer::Span;
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn materializes_text_span_edits_against_the_current_source() {
        let mut files = Files::new();
        let file_id = files.add("program.mux", "abc\n".to_string());
        let diagnostic = Diagnostic::new(DiagnosticCode::RedundantConstruct)
            .with_file_id(file_id)
            .with_span_edit(SpanEdit::machine_applicable_text(
                Span {
                    row_start: 1,
                    row_end: Some(1),
                    col_start: 2,
                    col_end: Some(3),
                },
                "X",
                DiagnosticCode::RedundantConstruct,
            ));

        let edits = materialize_span_edits(&[diagnostic], &files).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].replacement, "X");
        assert_eq!(edits[0].range, crate::diagnostic::SourceRange::new(1, 2));
    }

    #[test]
    fn internal_error_report_names_the_file_when_present() {
        let report = internal_compiler_error_report("codegen error: boom", Some("foo.mux"), false);
        assert!(report.contains("internal compiler error"));
        assert!(report.contains("codegen error: boom"));
        assert!(report.contains("while compiling: foo.mux"));
        assert!(report.contains("please report this error"));
        assert!(report.contains("the file above"));
        // The backtrace hint is suppressed when RUST_BACKTRACE is already set.
        assert!(!report.contains("RUST_BACKTRACE"));
    }

    #[test]
    fn internal_error_report_without_file_avoids_a_dangling_reference() {
        let report = internal_compiler_error_report("boom", None, true);
        assert!(!report.contains("while compiling:"));
        // Must not point at "the file above" when no file line was shown.
        assert!(!report.contains("the file above"));
        assert!(report.contains("the file you ran"));
        assert!(report.contains("re-run with RUST_BACKTRACE=1"));
    }

    #[test]
    fn internal_error_report_indents_every_line_of_a_multiline_detail() {
        // A link failure feeds multi-line linker stderr in as `detail`; each
        // line must be indented, not just the first.
        let detail = "linking failed: ld: error: undefined symbol: pow\n>>> referenced by main";
        let report = internal_compiler_error_report(detail, Some("foo.mux"), false);
        assert!(report.contains("  linking failed: ld: error: undefined symbol: pow\n"));
        assert!(report.contains("  >>> referenced by main\n"));
    }

    #[test]
    fn clang_failure_detail_labels_and_trims_stderr() {
        assert_eq!(
            clang_failure_detail(b"", b"ld: undefined symbol: pow\n\n"),
            "linking failed: ld: undefined symbol: pow"
        );
        assert_eq!(clang_failure_detail(b"", b""), "linking failed: ");
    }

    #[test]
    fn clang_failure_detail_includes_stdout_for_msvc_link() {
        // MSVC's link.exe writes LNK diagnostics to stdout, so a stderr-only
        // report would say nothing but the exit code.
        let detail = clang_failure_detail(
            b"LINK : fatal error LNK1104: cannot open file 'zstd.lib'\n",
            b"clang-22: error: linker command failed with exit code 1104\n",
        );
        assert!(
            detail.contains("LNK1104: cannot open file 'zstd.lib'"),
            "{detail}"
        );
        assert!(
            detail.contains("linker command failed with exit code 1104"),
            "{detail}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn classify_clang_output_distinguishes_link_and_spawn_failures() {
        use super::{ClangOutcome, classify_clang_output};
        use std::os::unix::process::ExitStatusExt;
        use std::process::{ExitStatus, Output};

        let success = Output {
            status: ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(matches!(
            classify_clang_output(Ok(success), "out.ll"),
            ClangOutcome::Linked
        ));

        let failed = Output {
            status: ExitStatus::from_raw(1 << 8), // exit code 1
            stdout: Vec::new(),
            stderr: b"undefined reference to pow".to_vec(),
        };
        match classify_clang_output(Ok(failed), "out.ll") {
            ClangOutcome::LinkFailed(detail) => {
                assert!(detail.contains("linking failed: undefined reference to pow"));
            }
            _ => panic!("expected LinkFailed"),
        }

        let spawn_err = Err(std::io::Error::other("boom"));
        match classify_clang_output(spawn_err, "out.ll") {
            ClangOutcome::SpawnFailed(message) => {
                assert!(message.contains("Failed to run clang"));
                assert!(message.contains("out.ll"));
            }
            _ => panic!("expected SpawnFailed"),
        }
    }

    #[test]
    fn internal_error_report_omits_empty_detail() {
        let report = internal_compiler_error_report("", Some("foo.mux"), false);
        // An empty detail produces no stray "  \n" line before the file line.
        assert!(!report.contains("\n  \n"));
        assert!(report.contains("while compiling: foo.mux"));
    }

    #[test]
    fn compiling_file_records_the_relativized_input() {
        // The panic hook reads the current input through this global; a relative
        // path is stored as-is so an internal-error report can name it.
        set_compiling_file(Path::new("scratch/prog.mux"));
        assert_eq!(compiling_file().as_deref(), Some("scratch/prog.mux"));
    }

    #[test]
    fn panic_detail_formats_message_and_location() {
        assert_eq!(
            format_panic_detail("boom", Some(("src/x.rs", 4, 2))),
            "boom (at src/x.rs:4:2)"
        );
        assert_eq!(format_panic_detail("boom", None), "boom");
    }

    #[test]
    fn relativize_strips_the_cwd_prefix() {
        let cwd = std::env::current_dir().unwrap();
        let absolute = cwd.join("sub").join("file.mux");
        assert_eq!(relativize_to_cwd(&absolute), PathBuf::from("sub/file.mux"));
        // An already-relative path is returned unchanged.
        let relative = Path::new("already/relative.mux");
        assert_eq!(
            relativize_to_cwd(relative),
            PathBuf::from("already/relative.mux")
        );
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
                .any(|c| c == &format!("llvm-config-{REQUIRED_LLVM_MAJOR}"))
        );
    }

    /// A packaged Windows install ships the DLL beside the import-less static
    /// archive, because the loader needs it. That must not read as a dynamic
    /// link: `-lmux_runtime` still resolves `mux_runtime.lib`, so the runtime's
    /// native dependencies still have to be linked explicitly.
    #[test]
    fn a_dll_in_the_lib_dir_does_not_make_the_link_dynamic() {
        let dir = unique_tmp("staticonly_both");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(static_lib_name()), b"x").unwrap();
        std::fs::write(dir.join(dynamic_lib_name()), b"x").unwrap();

        if cfg!(target_family = "windows") {
            assert!(
                runtime_lib_dir_is_static_only(&dir),
                "a DLL beside the static archive is a loader artifact, not a dynamic link"
            );
        } else {
            assert!(
                !runtime_lib_dir_is_static_only(&dir),
                "a shared object beside the archive genuinely is a dynamic link input"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every platform's list, checked on any host. The Windows entries are the
    /// point: they are unreachable on the Linux runners CI uses, so with an
    /// inline cfg! they were verified by nothing - and were in fact missing
    /// entirely, failing every Windows compile with LNK2019 on symbols these
    /// libraries own.
    #[test]
    fn native_runtime_deps_cover_every_platform() {
        assert!(
            native_runtime_deps("macos").is_empty(),
            "libSystem already provides these"
        );

        let unixish = native_runtime_deps("linux");
        for lib in ["-lm", "-lz", "-lpthread", "-ldl"] {
            assert!(unixish.contains(&lib), "unix is missing {lib}");
        }

        let windows = native_runtime_deps("windows");
        for lib in [
            "-ladvapi32",
            "-lbcrypt",
            "-lntdll",
            "-lsecur32",
            "-luserenv",
            "-lws2_32",
        ] {
            assert!(windows.contains(&lib), "windows is missing {lib}");
        }
        assert!(
            !windows.contains(&"-lm"),
            "libm is not a Windows system library"
        );
    }

    /// The link line is what turns a compiled object into a program, and it was
    /// entirely untested until it became a function of its own. These pin the
    /// parts that are easy to break silently.
    #[test]
    fn build_linker_args_links_the_object_and_the_runtime() {
        let dir = unique_tmp("linkargs_basic");
        std::fs::create_dir_all(&dir).unwrap();
        let args = build_linker_args(Path::new("scratch.o"), &dir);

        assert_eq!(
            args.first().map(std::ffi::OsString::as_os_str),
            Some(Path::new("scratch.o").as_os_str())
        );
        assert!(args.iter().any(|a| a == "-lmux_runtime"));
        let l_index = args.iter().position(|a| a == "-L").expect("-L present");
        assert_eq!(args[l_index + 1], dir.as_os_str());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A static archive's undefined symbols are resolved only by libraries that
    /// come AFTER it on the command line, so this ordering is load-bearing
    /// rather than cosmetic - reversing it reintroduces issue #291.
    #[test]
    fn build_linker_args_puts_native_deps_after_the_runtime() {
        let dir = unique_tmp("linkargs_static");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(static_lib_name()), b"x").unwrap();

        let args = build_linker_args(Path::new("scratch.o"), &dir);
        let runtime = args
            .iter()
            .position(|a| a == "-lmux_runtime")
            .expect("runtime is linked");

        if cfg!(target_os = "macos") {
            // libSystem provides these, so nothing extra is expected.
            assert!(!args.iter().any(|a| a == "-lm"));
        } else {
            let native = args
                .iter()
                .position(|a| a.to_string_lossy().starts_with("-l") && a != "-lmux_runtime")
                .expect("native dependencies are linked for a static-only runtime");
            assert!(
                native > runtime,
                "native deps must follow -lmux_runtime, got {args:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A dynamic runtime records its own dependencies, so the explicit list is
    /// only for the static-only case.
    #[test]
    fn build_linker_args_omits_native_deps_when_a_dynamic_runtime_is_present() {
        let dir = unique_tmp("linkargs_dynamic");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(static_lib_name()), b"x").unwrap();
        std::fs::write(dir.join(dynamic_lib_name()), b"x").unwrap();

        let args = build_linker_args(Path::new("scratch.o"), &dir);
        let extra: Vec<&std::ffi::OsString> = args
            .iter()
            .filter(|a| a.to_string_lossy().starts_with("-l") && *a != "-lmux_runtime")
            .collect();
        assert!(extra.is_empty(), "expected no native deps, got {extra:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every platform's link dialect, checked on any host. Each linker rejects
    /// the others' spellings, and CI runs only on Linux - so before this took
    /// the OS as a parameter, two thirds of these were unreachable and the
    /// Windows spelling was wrong for months with nothing able to notice.
    #[test]
    fn build_linker_args_uses_the_right_dialect_per_platform() {
        let dir = unique_tmp("linkargs_dialect");
        std::fs::create_dir_all(&dir).unwrap();
        let windows = build_linker_args_for("windows", Path::new("scratch.o"), &dir)
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        // rpath and the ELF dtags flag draw LNK4044 from MSVC's linker.
        assert!(!windows.contains("-rpath"), "{windows}");
        assert!(!windows.contains("disable-new-dtags"), "{windows}");
        assert!(!windows.contains("gc-sections"), "{windows}");
        // clang hardcodes -defaultlib:libcmt when it links, so the static CRT
        // has to be countermanded at LINK time; -fms-runtime-lib only affects
        // objects clang compiles itself, and it compiles none here.
        assert!(windows.contains("/NODEFAULTLIB:libcmt"), "{windows}");
        assert!(windows.contains("/NODEFAULTLIB:libucrt"), "{windows}");
        assert!(windows.contains("/DEFAULTLIB:msvcrt"), "{windows}");
        assert!(!windows.contains("-fms-runtime-lib"), "{windows}");

        let macos = build_linker_args_for("macos", Path::new("scratch.o"), &dir)
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(macos.contains("-dead_strip"), "{macos}");
        assert!(!macos.contains("gc-sections"), "{macos}");
        assert!(!macos.contains("disable-new-dtags"), "{macos}");
        assert!(!macos.contains("NODEFAULTLIB"), "{macos}");

        let linux = build_linker_args_for("linux", Path::new("scratch.o"), &dir)
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(linux.contains("--gc-sections"), "{linux}");
        assert!(linux.contains("disable-new-dtags"), "{linux}");
        assert!(linux.contains("-rpath"), "{linux}");
        assert!(!linux.contains("NODEFAULTLIB"), "{linux}");

        std::fs::remove_dir_all(&dir).ok();
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

        // A dynamic library alone: a link input everywhere except Windows, where
        // only the import library is linkable and a bare DLL must not make the
        // directory look usable.
        let dyn_dir = unique_tmp("rtlib_dyn");
        std::fs::create_dir_all(&dyn_dir).unwrap();
        let dyn_path = dyn_dir.join(dynamic_lib_name());
        std::fs::write(&dyn_path, b"x").unwrap();
        if cfg!(target_family = "windows") {
            assert!(find_runtime_lib_in_dir(&dyn_dir).is_none());
        } else {
            assert_eq!(find_runtime_lib_in_dir(&dyn_dir), Some(dyn_path));
        }
        std::fs::remove_dir_all(&dyn_dir).ok();

        // A dependency build puts hash-suffixed artifacts in target/.../deps.
        let hashed_dir = unique_tmp("rtlib_hashed");
        let deps_dir = hashed_dir.join("deps");
        std::fs::create_dir_all(&deps_dir).unwrap();
        let hashed_path = deps_dir.join("libmux_runtime-0123456789abcdef.a");
        std::fs::write(&hashed_path, b"x").unwrap();
        assert_eq!(find_runtime_lib_in_dir(&hashed_dir), Some(hashed_path));
        let args = build_linker_args(Path::new("scratch.o"), &hashed_dir);
        assert!(args.iter().any(|arg| {
            arg.to_string_lossy()
                .ends_with("libmux_runtime-0123456789abcdef.a")
        }));
        std::fs::remove_dir_all(&hashed_dir).ok();
    }

    #[test]
    fn hashed_runtime_resolution_fails_closed_on_ambiguous_archives() {
        let dir = unique_tmp("rtlib_hashed_ambiguous");
        let deps = dir.join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        let stale = deps.join("libmux_runtime-stale.a");
        let current = deps.join("libmux_runtime-current.a");
        std::fs::write(&stale, b"stale").unwrap();
        std::fs::write(&current, b"current").unwrap();

        assert!(find_runtime_lib_in_dir(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn linker_args_handle_non_utf8_runtime_paths_without_panicking() {
        let root = unique_tmp("rtlib_non_utf8");
        let dir = root.join(std::ffi::OsString::from_vec(vec![b'r', 0x80]));
        let deps = dir.join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::write(deps.join("libmux_runtime-nonutf8.a"), b"x").unwrap();

        let args = build_linker_args_for("linux", Path::new("scratch.o"), &dir);
        let rpath = args
            .iter()
            .find(|arg| arg.to_string_lossy().starts_with("-Wl,-rpath,"))
            .expect("rpath argument");
        assert!(std::os::unix::ffi::OsStrExt::as_bytes(rpath.as_os_str()).contains(&0x80));
        let library = args
            .iter()
            .find(|arg| arg.to_string_lossy().ends_with("libmux_runtime-nonutf8.a"))
            .expect("runtime archive argument");
        assert!(std::os::unix::ffi::OsStrExt::as_bytes(library.as_os_str()).contains(&0x80));

        let output = dir.join(std::ffi::OsString::from_vec(vec![b'o', 0x80]));
        let mut output_args = Vec::new();
        append_linker_output(&mut output_args, &output);
        assert_eq!(output_args[0], "-o");
        assert!(std::os::unix::ffi::OsStrExt::as_bytes(output_args[1].as_os_str()).contains(&0x80));
        std::fs::remove_dir_all(&root).ok();
    }

    /// A packaged Windows install keeps the DLL beside the executable so the
    /// loader finds it, and the import library under `lib/`. Resolution must
    /// therefore skip `bin/` and choose `lib/` - picking `bin/` passes the
    /// linker a directory holding nothing it can link, which is
    /// "LNK1181: cannot open input file '`mux_runtime.lib`'".
    #[test]
    fn a_dll_beside_the_binary_does_not_shadow_the_import_library() {
        let root = unique_tmp("rtlib_install");
        let bin = root.join("bin");
        let lib = root.join("lib");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(bin.join(dynamic_lib_name()), b"x").unwrap();
        let import_lib = lib.join(static_lib_name());
        std::fs::write(&import_lib, b"x").unwrap();

        if cfg!(target_family = "windows") {
            assert!(
                dir_holding_runtime_lib(&bin).is_none(),
                "a bare DLL in bin/ must not claim to provide a linkable runtime"
            );
        }
        assert_eq!(dir_holding_runtime_lib(&lib), Some(lib.clone()));
        std::fs::remove_dir_all(&root).ok();
    }

    /// The compiler links whatever directory this reports, so it must name the
    /// directory it was asked about - not the parent of the file found inside
    /// it. Returning the file's parent happens to be the same path, which is
    /// why the two are easy to conflate.
    #[test]
    fn dir_holding_runtime_lib_reports_the_directory_itself() {
        let empty_dir = unique_tmp("rtdir_empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        assert!(dir_holding_runtime_lib(&empty_dir).is_none());
        std::fs::remove_dir_all(&empty_dir).ok();

        // Only the names that are actually linkable on this platform. A bare
        // DLL is not one on Windows, so it deliberately reports nothing there.
        let linkable: &[&str] = if cfg!(target_family = "windows") {
            &[static_lib_name()]
        } else {
            &[static_lib_name(), dynamic_lib_name()]
        };
        for name in linkable {
            let dir = unique_tmp("rtdir_present");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), b"x").unwrap();
            assert_eq!(
                dir_holding_runtime_lib(&dir),
                Some(dir.clone()),
                "expected the directory itself for {name}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// Exclusive creation is what keeps the object off a path this process did
    /// not make: an existing file or symlink there must be an error, never a
    /// write redirected through it.
    #[cfg(unix)]
    #[test]
    fn scratch_object_creation_refuses_an_existing_entry() {
        let base = unique_tmp("obj_excl");
        std::fs::create_dir_all(&base).unwrap();

        let victim = base.join("victim");
        std::fs::write(&victim, b"UNTOUCHED").unwrap();
        let squatted = base.join("scratch.o");
        std::os::unix::fs::symlink(&victim, &squatted).unwrap();

        let err = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&squatted)
            .expect_err("create_new must refuse an existing symlink, not follow it");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&victim).unwrap(), b"UNTOUCHED");

        std::fs::remove_dir_all(&base).ok();
    }

    /// A release install puts the library in `../lib` relative to the binary,
    /// and `scripts/install.sh` is what creates that layout. The bundled
    /// library was present but unreachable in v0.6.0, so this pins the shape
    /// the search accepts.
    #[test]
    fn runtime_lib_search_accepts_the_release_install_layout() {
        let prefix = unique_tmp("rtlayout");
        let bin_dir = prefix.join("bin");
        let lib_dir = prefix.join("lib");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&lib_dir).unwrap();

        // Nothing anywhere yet.
        assert!(dir_holding_runtime_lib(&bin_dir).is_none());
        assert!(dir_holding_runtime_lib(&lib_dir).is_none());

        // The archive lands beside the binary, as a bare tarball extract has it.
        std::fs::write(bin_dir.join(static_lib_name()), b"x").unwrap();
        assert_eq!(dir_holding_runtime_lib(&bin_dir), Some(bin_dir.clone()));

        // ...and in the sibling lib/, as the installer lays it out.
        std::fs::write(lib_dir.join(static_lib_name()), b"x").unwrap();
        assert_eq!(dir_holding_runtime_lib(&lib_dir), Some(lib_dir.clone()));

        std::fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn static_only_detection_requires_archive_without_shared_lib() {
        // Static archive alone -> static-only.
        let static_dir = unique_tmp("rtlib_static_only");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join(static_lib_name()), b"x").unwrap();
        assert!(runtime_lib_dir_is_static_only(&static_dir));

        // Shared library beside it -> not static-only (it carries NEEDED).
        std::fs::write(static_dir.join(dynamic_lib_name()), b"x").unwrap();
        assert!(!runtime_lib_dir_is_static_only(&static_dir));
        std::fs::remove_dir_all(&static_dir).ok();

        // No archive at all -> not static-only.
        let dyn_dir = unique_tmp("rtlib_dyn_only");
        std::fs::create_dir_all(&dyn_dir).unwrap();
        std::fs::write(dyn_dir.join(dynamic_lib_name()), b"x").unwrap();
        assert!(!runtime_lib_dir_is_static_only(&dyn_dir));
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
    fn linker_doctor_accepts_any_driver_version() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_tmp("fake_clang");
        std::fs::create_dir_all(&dir).unwrap();
        let write_fake_clang = |name: &str, major: u32| -> PathBuf {
            let path = dir.join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho \"clang version {major}.0.0\"\n"),
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

        // A driver whose version differs from the linked LLVM is fine now: the
        // compiler emits an object file, so nothing parses textual IR and the
        // versions need not agree. This used to be a hard failure.
        let mismatching = write_fake_clang("clang-mismatch", linked_major + 1);
        let mismatching = mismatching.to_str().unwrap();
        assert_eq!(extract_clang_major(mismatching), Some(linked_major + 1));
        assert!(report_clang_for_doctor(Some(mismatching)));

        // No driver at all is still a failure - something has to link.
        assert!(!report_clang_for_doctor(None));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clang_version_detection_handles_missing_binary() {
        // A binary that does not exist makes every exec attempt fail, so the
        // retry loop exhausts and reports None (rather than hanging or panicking).
        let missing = "mux-nonexistent-clang-binary-xyz";
        assert!(clang_version_output(missing).is_none());
        assert!(extract_clang_major(missing).is_none());
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
}
