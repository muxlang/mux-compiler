use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const REQUIRED_LLVM_MAJOR: u32 = 22;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("mux-compiler should have a parent directory");

    // Set up git hooks path only in development (when .git exists, indicating a cloned repo)
    let is_dev_environment = workspace_root.join(".git").exists();
    if is_dev_environment && env::var("CI").is_err() {
        let _ = set_git_hooks_dir::setup(workspace_root.join(".github/hooks"), workspace_root);
    }

    let profile = if env::var("CARGO_PROFILE_RELEASE").is_ok() {
        "release"
    } else {
        "debug"
    };

    let target_dir = env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root.join("target"));

    let debug_path = target_dir.join("debug");
    let release_path = target_dir.join("release");
    let profile_path = target_dir.join(profile);

    let (static_lib, dynamic_lib) =
        detect_runtime_library(workspace_root, &debug_path, &release_path, &profile_path);

    println!(
        "cargo:rustc-env=MUX_RUNTIME_STATIC={}",
        static_lib.display()
    );
    println!(
        "cargo:rustc-env=MUX_RUNTIME_DYNAMIC={}",
        dynamic_lib.display()
    );
    println!("cargo:rustc-env=MUX_RUNTIME_DIR={}", profile_path.display());

    ensure_llvm_prefix(workspace_root);

    detect_and_emit_llvm_version();

    generate_embedded_std_sources(&manifest_dir);

    emit_runtime_version(workspace_root);
}

/// Read the locked `mux-runtime` version from the workspace `Cargo.lock` and
/// emit it as `MUX_RUNTIME_VERSION`. Decouples the runtime version from the
/// compiler version: the compiler releases independently while still resolving
/// and reporting the exact runtime it was built against.
fn emit_runtime_version(workspace_root: &Path) {
    let lock_path = workspace_root.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());

    let version = read_locked_runtime_version(&lock_path).unwrap_or_else(|| {
        panic!(
            "could not determine mux-runtime version from {}",
            lock_path.display()
        )
    });
    println!("cargo:rustc-env=MUX_RUNTIME_VERSION={}", version);
}

/// Parse `Cargo.lock` for the `[[package]] name = "mux-runtime"` entry and
/// return its `version`.
fn read_locked_runtime_version(lock_path: &Path) -> Option<String> {
    let contents = fs::read_to_string(lock_path).ok()?;
    let mut in_runtime_pkg = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_runtime_pkg = false;
            continue;
        }
        if trimmed == "name = \"mux-runtime\"" {
            in_runtime_pkg = true;
            continue;
        }
        if in_runtime_pkg && let Some(rest) = trimmed.strip_prefix("version = \"") {
            return Some(rest.trim_end_matches('"').to_string());
        }
    }
    None
}

/// Ensure `LLVM_SYS_221_PREFIX` is configured so `llvm-sys` links against
/// LLVM 22 instead of a different system LLVM. If not already set, detect
/// `llvm-config-22` on PATH and write the prefix to `.cargo/config.toml`.
/// Exits with an error on first-time setup so the next build picks it up.
fn ensure_llvm_prefix(workspace_root: &Path) {
    // Already set via environment — nothing to do.
    if env::var("LLVM_SYS_221_PREFIX").is_ok() {
        return;
    }

    let config_path = workspace_root.join(".cargo").join("config.toml");
    if let Ok(contents) = fs::read_to_string(&config_path)
        && contents.contains("LLVM_SYS_221_PREFIX")
    {
        return;
    }

    // llvm-sys doesn't probe "llvm-config-22" (it probes "llvm-config-221"),
    // so we detect it ourselves and tell llvm-sys where to look.
    let prefix = detect_llvm22_prefix();
    let Some(prefix) = prefix else {
        return;
    };

    if let Err(e) = write_llvm_prefix_to_config(workspace_root, &prefix) {
        eprintln!("error[build]: {}", e);
        std::process::exit(1);
    }
    // Force a rebuild so the newly written .cargo/config.toml takes effect.
    std::process::exit(1);
}

/// Probe for `llvm-config-22` on PATH and return its installation prefix
/// (e.g. `/usr`).
fn detect_llvm22_prefix() -> Option<String> {
    let binary = format!("llvm-config-{}", REQUIRED_LLVM_MAJOR);
    let output = Command::new(&binary).arg("--prefix").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(stdout.trim().to_string())
}

/// Write `LLVM_SYS_221_PREFIX` into `.cargo/config.toml`, preserving existing
/// content.
fn write_llvm_prefix_to_config(workspace_root: &Path, prefix: &str) -> std::io::Result<()> {
    let cargo_dir = workspace_root.join(".cargo");
    let config_path = cargo_dir.join("config.toml");

    fs::create_dir_all(&cargo_dir).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("failed to create {}: {}", cargo_dir.display(), e),
        )
    })?;

    if let Ok(existing) = fs::read_to_string(&config_path) {
        // File exists — append to [env] section or create it.
        if existing.contains("[env]") {
            let updated = existing.replacen(
                "[env]",
                &format!("[env]\nLLVM_SYS_221_PREFIX = \"{}\"", prefix),
                1,
            );
            fs::write(&config_path, updated).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to write {}: {}", config_path.display(), e),
                )
            })?;
        } else {
            let updated = format!(
                "{}\n\n[env]\nLLVM_SYS_221_PREFIX = \"{}\"\n",
                existing.trim_end(),
                prefix,
            );
            fs::write(&config_path, updated).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to write {}: {}", config_path.display(), e),
                )
            })?;
        }
    } else {
        // File does not exist — create it.
        let contents = format!("[env]\nLLVM_SYS_221_PREFIX = \"{}\"\n", prefix);
        fs::write(&config_path, contents).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("failed to write {}: {}", config_path.display(), e),
            )
        })?;
    }
    Ok(())
}

/// Detect the LLVM version that llvm-sys will link against and emit it as
/// `MUX_LLVM_MAJOR`. This allows the mux binary at runtime to pick a clang
/// binary that matches the linked LLVM version, avoiding IR parse errors.
fn detect_and_emit_llvm_version() {
    let mut candidates: Vec<String> = Vec::new();

    if let Ok(prefix) = env::var("LLVM_SYS_221_PREFIX") {
        let from_prefix = PathBuf::from(&prefix).join("bin").join("llvm-config");
        if from_prefix.exists() {
            candidates.push(from_prefix.to_string_lossy().to_string());
        }
    }

    // Prefer the versioned binary to avoid picking up a different system LLVM.
    candidates.push(format!("llvm-config-{}", REQUIRED_LLVM_MAJOR));
    candidates.push("llvm-config".to_string());

    for candidate in &candidates {
        let output = match Command::new(candidate).arg("--version").output() {
            Ok(output) if output.status.success() => output,
            _ => continue,
        };

        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let major = raw.split('.').next().and_then(|s| s.parse::<u32>().ok());

        if let Some(major) = major {
            println!("cargo:rustc-env=MUX_LLVM_MAJOR={}", major);
            return;
        }
    }

    // Fallback: assume the required version
    println!("cargo:rustc-env=MUX_LLVM_MAJOR={}", REQUIRED_LLVM_MAJOR);
}

fn generate_embedded_std_sources(manifest_dir: &Path) {
    let std_dir = manifest_dir.join("std");
    println!("cargo:rerun-if-changed={}", std_dir.display());

    let mut std_files = Vec::new();
    collect_mux_files(&std_dir, &std_dir, &mut std_files);
    std_files.sort_by(|a, b| a.0.cmp(&b.0));

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_file = Path::new(&out_dir).join("embedded_std.rs");

    let mut generated = String::new();
    generated.push_str("// @generated by build.rs. Do not edit manually.\n");
    generated.push_str("use std::collections::HashMap;\n");
    generated.push_str("use std::sync::OnceLock;\n\n");
    generated.push_str("pub fn embedded_std_sources() -> &'static HashMap<String, String> {\n");
    generated
        .push_str("    static SOURCES: OnceLock<HashMap<String, String>> = OnceLock::new();\n");
    generated.push_str("    SOURCES.get_or_init(|| {\n");
    generated.push_str("        let mut m = HashMap::new();\n");

    for (module_key, relative_path) in std_files {
        let abs_path = std_dir.join(&relative_path);
        let abs_path_str = abs_path
            .canonicalize()
            .expect("failed to canonicalize std file path")
            .to_string_lossy()
            .replace('\\', "/");
        generated.push_str(&format!(
            "        m.insert(\"{}\".to_string(), include_str!(\"{}\").to_string());\n",
            module_key, abs_path_str
        ));
    }

    generated.push_str("        m\n");
    generated.push_str("    })\n");
    generated.push_str("}\n");

    fs::write(&out_file, generated).expect("failed to write embedded_std.rs to OUT_DIR");
}

fn collect_mux_files(base: &Path, current: &Path, out: &mut Vec<(String, String)>) {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_mux_files(base, &path, out);
            continue;
        }

        if path.extension().is_none_or(|ext| ext != "mux") {
            continue;
        }

        let Ok(relative) = path.strip_prefix(base) else {
            continue;
        };

        let relative_str = relative.to_string_lossy().replace('\\', "/");
        let mut module = relative_str
            .trim_end_matches(".mux")
            .replace('/', ".")
            .to_string();
        if !module.starts_with("std.") {
            module = format!("std.{}", module);
        }

        out.push((module.clone(), relative_str.clone()));

        if let Some(short) = module.strip_prefix("std.") {
            out.push((short.to_string(), relative_str));
        }
    }
}

fn runtime_search_candidates(
    workspace_root: &Path,
    debug_path: &Path,
    release_path: &Path,
    profile_path: &Path,
) -> Vec<(PathBuf, &'static str)> {
    vec![
        (
            profile_path.to_path_buf(),
            "profile-specific (release or debug)",
        ),
        (release_path.to_path_buf(), "release"),
        (debug_path.to_path_buf(), "debug"),
        (
            workspace_root.join("target").join("release"),
            "workspace/target/release",
        ),
        (
            workspace_root.join("target").join("debug"),
            "workspace/target/debug",
        ),
    ]
}

fn emit_missing_runtime_warning(
    _runtime_name: &str,
    _profile_path: &Path,
    _release_path: &Path,
    _debug_path: &Path,
    _workspace_root: &Path,
) {
}

#[cfg(target_family = "unix")]
fn detect_runtime_library(
    workspace_root: &Path,
    debug_path: &Path,
    release_path: &Path,
    profile_path: &Path,
) -> (PathBuf, PathBuf) {
    let candidates =
        runtime_search_candidates(workspace_root, debug_path, release_path, profile_path);

    for (path, _) in &candidates {
        let static_lib = path.join("libmux_runtime.a");
        let dynamic_lib = path.join("libmux_runtime.so");
        if static_lib.exists() || dynamic_lib.exists() {
            return (static_lib, dynamic_lib);
        }
    }

    let static_lib = profile_path.join("libmux_runtime.a");
    let dynamic_lib = profile_path.join("libmux_runtime.so");

    if !static_lib.exists() && !dynamic_lib.exists() {
        emit_missing_runtime_warning(
            "libmux_runtime",
            profile_path,
            release_path,
            debug_path,
            workspace_root,
        );
    }

    (static_lib, dynamic_lib)
}

#[cfg(target_family = "windows")]
fn detect_runtime_library(
    workspace_root: &Path,
    debug_path: &Path,
    release_path: &Path,
    profile_path: &Path,
) -> (PathBuf, PathBuf) {
    let check_path = |p: &Path| -> (PathBuf, PathBuf) {
        let static_lib = p.join("mux_runtime.lib");
        let dynamic_lib = p.join("mux_runtime.dll");
        (static_lib, dynamic_lib)
    };

    let candidates =
        runtime_search_candidates(workspace_root, debug_path, release_path, profile_path);

    for (path, _description) in &candidates {
        let (static_lib, dynamic_lib) = check_path(path);
        if static_lib.exists() || dynamic_lib.exists() {
            return (static_lib, dynamic_lib);
        }
    }

    let static_lib = profile_path.join("mux_runtime.lib");
    let dynamic_lib = profile_path.join("mux_runtime.dll");

    if !static_lib.exists() && !dynamic_lib.exists() {
        emit_missing_runtime_warning(
            "mux_runtime",
            profile_path,
            release_path,
            debug_path,
            workspace_root,
        );
    }

    (static_lib, dynamic_lib)
}
