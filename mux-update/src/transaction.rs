use crate::{
    Context, Result,
    receipt::{self, Area, OwnedFile, RECEIPT, Receipt},
};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
struct Prepared {
    destination: PathBuf,
    replacement: Option<PathBuf>,
    backup: PathBuf,
    existed: bool,
}
pub(crate) fn install(
    old: &Receipt,
    bundle: &Path,
    version: &str,
    after_replace: impl Fn(usize) -> Result<()>,
) -> Result<()> {
    crate::cancellation::check()?;
    validate_binary(bundle, version)?;
    crate::cancellation::check()?;
    let mut new = old.clone();
    new.version = version.into();
    new.files = receipt::bundle_files(bundle)?;
    let bin_stage = tempfile::Builder::new()
        .prefix(".mux-update-")
        .tempdir_in(&old.bin_dir)
        .context("Stage binary files on installation volume")?;
    let lib_stage = tempfile::Builder::new()
        .prefix(".mux-update-")
        .tempdir_in(&old.lib_dir)
        .context("Stage library files on installation volume")?;
    let prepared = prepare(old, &new, bundle, bin_stage.path(), lib_stage.path())?;
    let mut applied = 0;
    let result = (|| {
        for (index, item) in prepared.iter().enumerate() {
            crate::cancellation::check()?;
            if item.existed {
                fs::rename(&item.destination, &item.backup).context(
                    "Back up installed file; close other Mux processes and editors, then retry",
                )?;
            }
            applied = index + 1;
            if let Some(replacement) = &item.replacement {
                fs::rename(replacement, &item.destination).context("Install updated file")?;
            }
            after_replace(index)?;
        }
        crate::cancellation::check()?;
        validate_installed(&new, version)?;
        crate::cancellation::check()
    })();
    if let Err(failure) = result {
        if let Err(recovery) = rollback(&prepared[..applied]) {
            let bin = bin_stage.keep();
            let lib = lib_stage.keep();
            return Err(format!(
                "{failure}. Rollback failed: {recovery}. Recovery files remain in {} and {}; do not delete them before restoring Mux.",
                bin.display(),
                lib.display()
            ));
        }
        return Err(format!("{failure}. Previous Mux installation restored."));
    }
    Ok(())
}
fn prepare(
    old: &Receipt,
    new: &Receipt,
    bundle: &Path,
    bin_stage: &Path,
    lib_stage: &Path,
) -> Result<Vec<Prepared>> {
    let old_files: BTreeSet<_> = old.files.iter().cloned().collect();
    let new_files: BTreeSet<_> = new.files.iter().cloned().collect();
    let all: BTreeSet<_> = old_files.union(&new_files).cloned().collect();
    let mut prepared = Vec::new();
    for (index, file) in all.iter().enumerate() {
        let destination = new.destination(file);
        let existed = is_regular_existing(&destination)?;
        if existed && !old_files.contains(file) {
            return Err(format!(
                "Update would overwrite a file not owned by Mux: {}",
                destination.display()
            ));
        }
        let stage = match file.area {
            Area::Bin => bin_stage,
            Area::Lib => lib_stage,
        };
        let replacement = if new_files.contains(file) {
            let staged = stage.join(format!("new-{index}"));
            fs::copy(source(bundle, file), &staged).context("Stage updated file")?;
            Some(staged)
        } else {
            None
        };
        let backup = if cfg!(windows) && file.area == Area::Bin && file.name == "mux.exe" {
            let id = bin_stage
                .file_name()
                .ok_or("Missing staging directory name")?
                .to_string_lossy();
            old.bin_dir
                .join(format!(".mux-update-backup-launcher-{id}"))
        } else {
            stage.join(format!("old-{index}"))
        };
        prepared.push(Prepared {
            destination,
            replacement,
            backup,
            existed,
        });
    }
    let receipt_new = bin_stage.join("receipt-new");
    fs::write(
        &receipt_new,
        serde_json::to_vec_pretty(new).context("Serialize updated receipt")?,
    )
    .context("Stage updated receipt")?;
    let destination = old.bin_dir.join(RECEIPT);
    let existed = is_regular_existing(&destination)?;
    prepared.push(Prepared {
        destination,
        replacement: Some(receipt_new),
        backup: bin_stage.join("receipt-old"),
        existed,
    });
    Ok(prepared)
}
fn source(bundle: &Path, file: &OwnedFile) -> PathBuf {
    let area = match file.area {
        Area::Bin => "bin",
        Area::Lib => "lib",
    };
    let source = bundle.join(area).join(&file.name);
    if !source.exists() && cfg!(windows) && file.area == Area::Bin && file.name.ends_with(".dll") {
        bundle.join("lib").join(&file.name)
    } else {
        source
    }
}
fn is_regular_existing(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Installation path is not a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("Inspect installation file: {error}")),
    }
}
fn rollback(applied: &[Prepared]) -> Result<()> {
    let mut errors = Vec::new();
    for item in applied.iter().rev() {
        if let Err(error) = fs::remove_file(&item.destination)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!("Remove {}: {error}", item.destination.display()));
            continue;
        }
        if item.existed
            && let Err(error) = fs::rename(&item.backup, &item.destination)
        {
            errors.push(format!(
                "Restore {} from {}: {error}",
                item.destination.display(),
                item.backup.display()
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}
fn validate_binary(bundle: &Path, version: &str) -> Result<()> {
    validate_directory(&bundle.join("bin"), version)
}
fn validate_installed(receipt: &Receipt, version: &str) -> Result<()> {
    validate_directory(&receipt.bin_dir, version)
}
fn validate_directory(bin: &Path, version: &str) -> Result<()> {
    if cfg!(windows) {
        probe(&bin.join("mux.exe"), version)?;
        probe(&bin.join("mux-driver.exe"), version)
    } else {
        probe(&bin.join("mux"), version)
    }
}
fn probe(binary: &Path, version: &str) -> Result<()> {
    let bin_dir = binary
        .parent()
        .ok_or("Compiler binary has no parent directory")?;
    let mut command = Command::new(binary);
    command
        .arg("__version")
        .current_dir(bin_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        let system_root = std::env::var_os("SystemRoot")
            .ok_or("SystemRoot is missing; cannot validate Windows DLL dependencies")?;
        let root = PathBuf::from(system_root);
        let path = std::env::join_paths([bin_dir.to_path_buf(), root.join("System32"), root])
            .context("Prepare isolated compiler validation PATH")?;
        command.env("PATH", path);
    }
    let mut child = command
        .spawn()
        .context("Start updated compiler for validation")?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().context("Wait for compiler validation")? {
            break status;
        }
        if started.elapsed() > Duration::from_secs(15) {
            child.kill().context("Stop timed-out compiler validation")?;
            child.wait().context("Reap timed-out compiler validation")?;
            return Err("Updated compiler validation timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .ok_or("Compiler validation stdout missing")?
        .take(4097)
        .read_to_end(&mut stdout)
        .context("Read compiler validation")?;
    let reported = std::str::from_utf8(&stdout)
        .context("Invalid compiler version output")?
        .trim();
    if !status.success() || reported != version {
        return Err(format!(
            "Updated compiler validation failed: expected {version}, got {reported:?} ({status})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn launcher() -> &'static str {
        if cfg!(windows) { "mux.exe" } else { "mux" }
    }
    fn compile_fake_compiler(version: &str) -> Vec<u8> {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("version.rs");
        let binary = directory.path().join(launcher());
        fs::write(&source, format!("fn main() {{ println!({version:?}); }}")).unwrap();
        let output = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read(binary).unwrap()
    }
    fn fake_compiler() -> &'static [u8] {
        static BINARY: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BINARY.get_or_init(|| compile_fake_compiler("0.15.0"))
    }
    fn fixture() -> (tempfile::TempDir, Receipt, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let lib = root.path().join("lib");
        let bundle = root.path().join("bundle");
        for dir in [&bin, &lib, &bundle.join("bin"), &bundle.join("lib")] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(bin.join(launcher()), "old binary").unwrap();
        fs::write(lib.join("old.a"), "old runtime").unwrap();
        fs::write(lib.join("unrelated"), "user file").unwrap();
        fs::write(bundle.join("bin").join(launcher()), fake_compiler()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                bundle.join("bin").join(launcher()),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        fs::write(bundle.join("lib/new.a"), "new runtime").unwrap();
        if cfg!(windows) {
            fs::write(bundle.join("bin/mux-driver.exe"), fake_compiler()).unwrap();
            for file in ["LLVM-C.dll", "libxml2.dll", "zlib.dll", "zstd.dll"] {
                fs::write(bundle.join("bin").join(file), "support DLL").unwrap();
            }
            for file in ["mux_runtime.lib", "mux_runtime.dll"] {
                fs::write(bundle.join("lib").join(file), "runtime").unwrap();
            }
        } else {
            fs::write(bundle.join("lib/libmux_runtime.a"), "runtime archive").unwrap();
            let dynamic = if cfg!(target_os = "macos") {
                "libmux_runtime.dylib"
            } else {
                "libmux_runtime.so"
            };
            fs::write(bundle.join("lib").join(dynamic), "runtime shared").unwrap();
        }
        let receipt = Receipt {
            schema: 1,
            target: if cfg!(windows) {
                "windows-x86_64"
            } else if cfg!(target_os = "macos") {
                if cfg!(target_arch = "aarch64") {
                    "macos-aarch64"
                } else {
                    "macos-x86_64"
                }
            } else {
                "linux-x86_64"
            }
            .into(),
            version: "0.14.0".into(),
            bin_dir: fs::canonicalize(bin).unwrap(),
            lib_dir: fs::canonicalize(lib).unwrap(),
            files: vec![
                OwnedFile {
                    area: Area::Bin,
                    name: launcher().into(),
                },
                OwnedFile {
                    area: Area::Lib,
                    name: "old.a".into(),
                },
            ],
        };
        fs::write(
            receipt.bin_dir.join(RECEIPT),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        (root, receipt, bundle)
    }
    #[test]
    fn replaces_owned_files_and_preserves_unrelated() {
        let (_root, old, bundle) = fixture();
        install(&old, &bundle, "0.15.0", |_| Ok(())).unwrap();
        assert!(!old.lib_dir.join("old.a").exists());
        assert_eq!(
            fs::read_to_string(old.lib_dir.join("new.a")).unwrap(),
            "new runtime"
        );
        assert_eq!(
            fs::read_to_string(old.lib_dir.join("unrelated")).unwrap(),
            "user file"
        );
        assert_eq!(
            receipt::installed_version(&old.bin_dir.join(launcher()))
                .unwrap()
                .unwrap(),
            "0.15.0"
        );
    }
    #[test]
    fn failure_restores_binary_libraries_and_receipt() {
        let (_root, old, bundle) = fixture();
        let before = fs::read(old.bin_dir.join(RECEIPT)).unwrap();
        let all: BTreeSet<_> = old
            .files
            .iter()
            .cloned()
            .chain(receipt::bundle_files(&bundle).unwrap())
            .collect();
        let receipt_index = all.len();
        assert!(
            install(&old, &bundle, "0.15.0", |index| if index == receipt_index {
                Err("injected failure".into())
            } else {
                Ok(())
            })
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(old.bin_dir.join(launcher())).unwrap(),
            "old binary"
        );
        assert_eq!(
            fs::read_to_string(old.lib_dir.join("old.a")).unwrap(),
            "old runtime"
        );
        assert!(!old.lib_dir.join("new.a").exists());
        assert_eq!(fs::read(old.bin_dir.join(RECEIPT)).unwrap(), before);
    }
    #[test]
    fn refuses_unowned_conflict_before_mutating() {
        let (_root, old, bundle) = fixture();
        fs::write(old.lib_dir.join("new.a"), "user file").unwrap();
        assert!(install(&old, &bundle, "0.15.0", |_| Ok(())).is_err());
        assert_eq!(
            fs::read_to_string(old.bin_dir.join(launcher())).unwrap(),
            "old binary"
        );
        assert_eq!(
            fs::read_to_string(old.lib_dir.join("new.a")).unwrap(),
            "user file"
        );
    }
    fn receipt_replacement_index(old: &Receipt, bundle: &Path) -> usize {
        old.files
            .iter()
            .cloned()
            .chain(receipt::bundle_files(bundle).unwrap())
            .collect::<BTreeSet<_>>()
            .len()
    }
    fn assert_previous_installation(old: &Receipt, before: &[u8], bundle: &Path) {
        assert_eq!(
            fs::read_to_string(old.bin_dir.join(launcher())).unwrap(),
            "old binary"
        );
        assert_eq!(
            fs::read_to_string(old.lib_dir.join("old.a")).unwrap(),
            "old runtime"
        );
        assert_eq!(fs::read(old.bin_dir.join(RECEIPT)).unwrap(), before);
        for file in receipt::bundle_files(bundle).unwrap() {
            if !old.files.contains(&file) {
                assert!(
                    !old.destination(&file).exists(),
                    "New file survived rollback: {}",
                    file.name
                );
            }
        }
    }
    #[test]
    fn installed_validation_failure_restores_previous_installation() {
        let (_root, old, bundle) = fixture();
        let before = fs::read(old.bin_dir.join(RECEIPT)).unwrap();
        let wrong_version = compile_fake_compiler("0.0.0");
        let receipt_index = receipt_replacement_index(&old, &bundle);
        let failure = install(&old, &bundle, "0.15.0", |index| {
            if index == receipt_index {
                fs::write(old.bin_dir.join(launcher()), &wrong_version).unwrap();
            }
            Ok(())
        })
        .unwrap_err();
        assert!(
            failure.contains("expected 0.15.0, got \"0.0.0\""),
            "{failure}"
        );
        assert_previous_installation(&old, &before, &bundle);
    }
    #[cfg(unix)]
    #[test]
    fn termination_during_install_rolls_back() {
        const CHILD_MARKER: &str = "MUX_UPDATE_TEST_CANCELLATION_CHILD";
        if std::env::var_os(CHILD_MARKER).is_none() {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "transaction::tests::termination_during_install_rolls_back",
                    "--nocapture",
                ])
                .env(CHILD_MARKER, "1")
                .spawn()
                .unwrap();
            let started = Instant::now();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "Cancellation child failed: {status}");
                    return;
                }
                if started.elapsed() > Duration::from_secs(20) {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("Cancellation child timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        crate::cancellation::install_handler().unwrap();
        let (_root, old, bundle) = fixture();
        let before = fs::read(old.bin_dir.join(RECEIPT)).unwrap();
        let receipt_index = receipt_replacement_index(&old, &bundle);
        let failure = install(&old, &bundle, "0.15.0", |index| {
            if index == receipt_index {
                let status = Command::new("kill")
                    .args(["-TERM", &std::process::id().to_string()])
                    .status()
                    .unwrap();
                assert!(status.success());
                let started = Instant::now();
                while crate::cancellation::check().is_ok() {
                    assert!(
                        started.elapsed() < Duration::from_secs(2),
                        "SIGTERM handler did not observe cancellation"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(())
        })
        .unwrap_err();
        assert!(failure.contains("Update cancelled"), "{failure}");
        assert_previous_installation(&old, &before, &bundle);
    }
    #[cfg(windows)]
    #[test]
    fn locked_windows_library_rolls_back_previous_swaps() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_root, mut old, bundle) = fixture();
        let library = old.lib_dir.join("mux_runtime.lib");
        fs::write(&library, "previous runtime").unwrap();
        old.files.push(OwnedFile {
            area: Area::Lib,
            name: "mux_runtime.lib".into(),
        });
        fs::write(old.bin_dir.join(RECEIPT), serde_json::to_vec(&old).unwrap()).unwrap();
        let locked = std::cell::RefCell::new(None);
        let failure = install(&old, &bundle, "0.15.0", |index| {
            if index == 0 {
                *locked.borrow_mut() = Some(
                    fs::OpenOptions::new()
                        .read(true)
                        .share_mode(0)
                        .open(&library)
                        .unwrap(),
                );
            }
            Ok(())
        })
        .unwrap_err();
        assert!(
            failure.contains("Previous Mux installation restored"),
            "{failure}"
        );
        drop(locked.into_inner());
        assert_eq!(
            fs::read_to_string(old.bin_dir.join(launcher())).unwrap(),
            "old binary"
        );
        assert_eq!(fs::read_to_string(library).unwrap(), "previous runtime");
        assert!(!old.bin_dir.join("mux-driver.exe").exists());
        assert_eq!(
            receipt::installed_version(&old.bin_dir.join(launcher()))
                .unwrap()
                .unwrap(),
            "0.14.0"
        );
    }
}
