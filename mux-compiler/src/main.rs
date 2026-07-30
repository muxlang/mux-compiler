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
use std::collections::HashSet;
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
    let versioned = format!("clang-{}", linked_major);
    let candidates: &[&str] = &[versioned.as_str(), "clang", "cc", "gcc"];
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
    if cfg!(target_family = "windows") {
        dir.join("mux_runtime.lib")
    } else {
        dir.join("libmux_runtime.a")
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
    let dynamic_lib = runtime_dynamic_lib_path(dir);
    !dynamic_lib.exists()
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

/// Run codegen, then write the object file the linker consumes - and, when
/// `-i` asked for it, the readable `.ll` alongside it.
///
/// Only the object is on the critical path. The textual IR used to be, which is
/// what tied an install to a clang matching this compiler's LLVM major.
fn generate_object_or_exit(
    codegen: &mut codegen::CodeGenerator,
    nodes: &[ast::AstNode],
    object_file: &str,
    ir_file: Option<&str>,
    scratch_dir: &Path,
) {
    if let Err(e) = codegen.generate(nodes) {
        spinner::stop();
        // A codegen failure is an internal compiler bug, not a language-level
        // error in the user's program, so it gets the internal-error framing
        // rather than a bare message.
        remove_scratch_dir(scratch_dir);
        report_internal_compiler_error(&format!("codegen error: {}", e));
        process::exit(1);
    }
    // IR first: when the module is invalid, the emitted `.ll` is exactly what is
    // needed to debug it, and object emission verifies and would bail first.
    if let Some(ir_file) = ir_file
        && let Err(e) = codegen.emit_ir_to_file(ir_file)
    {
        spinner::stop();
        remove_scratch_dir(scratch_dir);
        report_internal_compiler_error(&format!("failed to emit IR: {}", e));
        process::exit(1);
    }
    if let Err(e) = codegen.emit_object_to_file(object_file) {
        spinner::stop();
        remove_scratch_dir(scratch_dir);
        report_internal_compiler_error(&format!("failed to emit object file: {}", e));
        process::exit(1);
    }
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

/// Build the internal-error detail line for a failed link from clang's stderr.
/// Trailing whitespace is trimmed so the report does not end with a blank line;
/// pure so the wording is unit-tested without spawning clang.
fn clang_failure_detail(stderr: &[u8]) -> String {
    format!(
        "linking failed: {}",
        String::from_utf8_lossy(stderr).trim_end()
    )
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
        Ok(output) => ClangOutcome::LinkFailed(clang_failure_detail(&output.stderr)),
        Err(e) => ClangOutcome::SpawnFailed(format!(
            "Failed to run clang: {}. IR file generated at: {}",
            e, ir_file
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

/// Name of the object file inside the scratch directory. Shared so cleanup
/// removes exactly the file emission created.
const SCRATCH_OBJECT_NAME: &str = "module.o";

/// A private directory to emit the object file into, and the path within it.
///
/// Not `<source>.o`: that would overwrite a `foo.o` the user already had beside
/// `foo.mux` and then delete it during cleanup, losing a file the compiler does
/// not own.
///
/// Not a bare path in the shared temp directory either. A predictable name that
/// LLVM opens without exclusive creation is a symlink-squatting target - a local
/// process can pre-create it pointing at any file this user can write, and the
/// emitted object lands there instead. So the *directory* is created
/// exclusively: `create_dir` fails if the path exists at all, including as a
/// symlink, and mode 0700 keeps anyone else from placing entries inside it
/// afterwards. The object name within it can then be fixed and boring.
fn scratch_object_dir() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    // Distinct candidates rather than secret ones: exclusive creation is what
    // provides the safety, and the nanosecond clock only avoids self-collision
    // between concurrent compiles.
    for _ in 0..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let candidate = env::temp_dir().join(format!(
            "mux-{}-{}-{}",
            process::id(),
            nanos,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            // Taken already - by a concurrent compile or by a squatter. Either
            // way, try a different name rather than writing into it.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "failed to create a temporary directory at {}: {}",
                    candidate.display(),
                    e
                ));
            }
        }
    }

    Err("could not create a temporary directory for the object file".to_string())
}

/// Remove exactly what was put in the scratch directory, then the directory
/// itself. The object is a build artifact the user did not ask for, unlike the
/// `.ll`, which is only written when `-i` requested it and is never cleaned up
/// here.
///
/// Deliberately not `remove_dir_all`: that recursively deletes whatever is at
/// the path, and the path is all this holds - if the directory it names is no
/// longer the one this process created, a recursive delete would take unrelated
/// data with it. Removing the single known file and then a non-recursive
/// `remove_dir` cannot: `remove_dir` refuses a directory containing anything
/// else, and refuses a symlink outright. Anything unexpected is left alone.
///
/// Best-effort throughout: failing to clean up a temporary file must not fail an
/// otherwise successful compile.
fn remove_scratch_dir(dir: &Path) {
    let _ = fs::remove_file(dir.join(SCRATCH_OBJECT_NAME));
    let _ = fs::remove_dir(dir);
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
/// "the file above" or the RUST_BACKTRACE hint - is unit-testable without
/// capturing stderr or a real panic.
fn internal_compiler_error_report(
    detail: &str,
    file: Option<&str>,
    show_backtrace_hint: bool,
) -> String {
    let mut out = String::from("\nerror: internal compiler error\n");
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
        out.push_str(&format!("  while compiling: {}\n", file));
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
    out.push_str(&format!(
        "tip: please report this error, along with {} and your system details\n",
        subject
    ));
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
/// "<message> (at <file>:<line>:<col>)" detail line. Pure, so it is unit-tested
/// directly; `panic_detail` handles pulling these fields out of the hook info.
fn format_panic_detail(message: &str, location: Option<(&str, u32, u32)>) -> String {
    match location {
        Some((file, line, col)) => format!("{} (at {}:{}:{})", message, file, line, col),
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
/// full panic and backtrace are preserved for maintainers when RUST_BACKTRACE is
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

fn main() {
    install_internal_error_panic_hook();
    let (file_path, do_run, output, intermediate) = parse_args_or_exit();
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

    let mut analyzer = semantics::SemanticAnalyzer::new_with_resolver(resolver);
    analyze_semantics_or_exit(&mut analyzer, &nodes, file_id, &mut files);

    let context = inkwell::context::Context::create();
    let source_name = file_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.to_string_lossy().into_owned());
    let mut codegen = codegen::CodeGenerator::new(&context, &mut analyzer, &source_name);

    let stem = file_path
        .to_string_lossy()
        .trim_end_matches(".mux")
        .to_string();
    let ir_file = format!("{}.ll", stem);

    // Resolved before the scratch directory exists and before codegen runs.
    // Neither depends on codegen, both exit the process on failure, and
    // `process::exit` skips destructors - so anything created first would be
    // left behind. Failing before the expensive work is better regardless.
    let lib_dir = resolve_runtime_lib_dir_or_exit();
    let lib_path_str = lib_dir
        .to_str()
        .expect("library path should be valid Unicode")
        .to_string();
    let linker_cmd = find_linker_or_exit();

    let scratch_dir = match scratch_object_dir() {
        Ok(dir) => dir,
        Err(e) => {
            spinner::stop();
            report_internal_compiler_error(&e);
            process::exit(1);
        }
    };
    let object_file = scratch_dir
        .join(SCRATCH_OBJECT_NAME)
        .to_string_lossy()
        .into_owned();
    generate_object_or_exit(
        &mut codegen,
        &nodes,
        &object_file,
        intermediate.then_some(ir_file.as_str()),
        &scratch_dir,
    );

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

    let mut linker_args = vec![
        object_file.clone(),
        "-L".to_string(),
        lib_path_str.to_string(),
        format!("-Wl,-rpath,{}", lib_path_str),
        "-ffunction-sections".to_string(),
        "-fdata-sections".to_string(),
    ];

    #[cfg(not(target_os = "macos"))]
    linker_args.push("-Wl,--disable-new-dtags".to_string());

    let gc_sections_flag = if cfg!(target_os = "macos") {
        "-Wl,-dead_strip".to_string()
    } else {
        "-Wl,--gc-sections".to_string()
    };
    linker_args.push(gc_sections_flag);
    linker_args.push("-lmux_runtime".to_string());

    // A static-only libmux_runtime.a carries no NEEDED entries, so its undefined
    // native symbols must be resolved explicitly - otherwise a program using
    // libm (e.g. `**`/`pow`) fails to link with "undefined reference to pow"
    // (issue #291). These must follow -lmux_runtime so the archive's references
    // are satisfied by the libraries after it. The cdylib pulls these in on its
    // own, so only the static-only case needs them. Skipped on Windows (the
    // import lib records its own deps) and macOS (libSystem provides them).
    if !cfg!(target_os = "windows")
        && !cfg!(target_os = "macos")
        && runtime_lib_dir_is_static_only(&lib_dir)
    {
        for native_lib in ["-lm", "-lz", "-lpthread", "-ldl"] {
            linker_args.push(native_lib.to_string());
        }
    }

    linker_args.push("-o".to_string());
    linker_args.push(
        exe_file
            .to_str()
            .expect("executable path should be valid Unicode")
            .to_string(),
    );

    let linker_output = Command::new(&linker_cmd).args(&linker_args).output();

    spinner::stop();
    // Before interpreting the result: a spawn failure or a nonzero linker exit
    // ends the process inside the call below, so cleaning up afterwards would
    // leak the scratch object on exactly the paths that fail.
    remove_scratch_dir(&scratch_dir);
    report_clang_output_or_exit(linker_output, do_run, &file_path, &ir_file);

    if do_run {
        run_executable_or_exit(&exe_file);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        REQUIRED_LLVM_MAJOR, clang_failure_detail, clang_version_output, compiling_file,
        dir_holding_runtime_lib, extract_clang_major, find_runtime_lib_in_dir, format_panic_detail,
        internal_compiler_error_report, llvm_config_candidates, pick_llvm_for_dev,
        print_doctor_verdict, print_version_banner, relativize_to_cwd, remove_scratch_dir,
        report_clang_for_doctor, report_runtime_for_doctor, runtime_lib_dir_is_static_only,
        set_compiling_file, status_marker, validate_llvm_for_doctor,
    };
    use std::path::{Path, PathBuf};

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
            clang_failure_detail(b"ld: undefined symbol: pow\n\n"),
            "linking failed: ld: undefined symbol: pow"
        );
        assert_eq!(clang_failure_detail(b""), "linking failed: ");
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
                assert!(detail.contains("linking failed: undefined reference to pow"))
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

        for name in [static_lib_name(), dynamic_lib_name()] {
            let dir = unique_tmp("rtdir_present");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), b"x").unwrap();
            assert_eq!(
                dir_holding_runtime_lib(&dir),
                Some(dir.clone()),
                "expected the directory itself for {}",
                name
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// Cleanup must remove only what emission created. The scratch path is all
    /// the compiler holds, so if the directory it names is no longer the one
    /// this process made, a recursive delete would take unrelated data with it.
    /// This pins that a directory holding anything else survives.
    #[test]
    fn scratch_cleanup_leaves_a_replaced_directory_alone() {
        let base = unique_tmp("scratch_replace");
        std::fs::create_dir_all(&base).unwrap();

        // Stand in for a directory that is not the one emission created: it has
        // unrelated contents and no object file.
        let replacement = base.join("scratch");
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::write(replacement.join("someone_elses_data"), b"KEEP").unwrap();

        remove_scratch_dir(&replacement);

        assert!(
            replacement.is_dir(),
            "a directory holding unrelated files must survive cleanup"
        );
        assert_eq!(
            std::fs::read(replacement.join("someone_elses_data")).unwrap(),
            b"KEEP"
        );

        // The directory this process did create - one object file, nothing else -
        // is removed completely.
        let ours = base.join("ours");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::write(ours.join("module.o"), b"obj").unwrap();
        remove_scratch_dir(&ours);
        assert!(!ours.exists(), "our own scratch directory should be gone");

        std::fs::remove_dir_all(&base).ok();
    }

    /// The object file is created exclusively too, not just the directory
    /// holding it. LLVM emits into memory and the compiler writes the bytes
    /// itself, so the only filesystem entry involved is one this process
    /// creates - a file or symlink already at that path is an error rather than
    /// a write redirected somewhere else.
    #[cfg(unix)]
    #[test]
    fn object_file_creation_refuses_an_existing_symlink() {
        let base = unique_tmp("obj_squat");
        std::fs::create_dir_all(&base).unwrap();
        let victim = base.join("victim");
        std::fs::write(&victim, b"UNTOUCHED").unwrap();

        let object = base.join("module.o");
        std::os::unix::fs::symlink(&victim, &object).unwrap();

        let err = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&object)
            .expect_err("create_new must refuse an existing symlink, not follow it");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&victim).unwrap(), b"UNTOUCHED");

        std::fs::remove_dir_all(&base).ok();
    }

    /// The scratch directory must be created exclusively, because a bare
    /// predictable path in a shared temp directory is a symlink-squatting
    /// target: a local process pre-creates it pointing at a file this user can
    /// write, and the object LLVM emits lands there instead. `create_dir` is
    /// what prevents that - this pins that it refuses an existing symlink
    /// rather than following it.
    #[cfg(unix)]
    #[test]
    fn scratch_dir_creation_refuses_an_existing_symlink() {
        let base = unique_tmp("scratch_squat");
        std::fs::create_dir_all(&base).unwrap();
        let target = base.join("attacker_target");
        std::fs::create_dir_all(&target).unwrap();

        let squatted = base.join("scratch");
        std::os::unix::fs::symlink(&target, &squatted).unwrap();

        let err = std::fs::DirBuilder::new()
            .create(&squatted)
            .expect_err("creating over an existing symlink must fail, not follow it");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);

        // Nothing was written through the symlink.
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);

        // A fresh name still succeeds, which is what the retry loop relies on.
        let fresh = base.join("scratch-2");
        std::fs::DirBuilder::new().create(&fresh).unwrap();
        assert!(fresh.is_dir());

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
