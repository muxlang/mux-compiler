use crate::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub(crate) const RECEIPT: &str = ".mux-install.json";
const LOCK: &str = ".mux-install.lock";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OwnedFile {
    pub area: Area,
    pub name: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Area {
    Bin,
    Lib,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub schema: u32,
    pub target: String,
    pub version: String,
    pub bin_dir: PathBuf,
    pub lib_dir: PathBuf,
    pub files: Vec<OwnedFile>,
}
pub struct InstallationLock {
    _file: File,
}

impl Drop for InstallationLock {
    fn drop(&mut self) {
        // A concurrently spawned child may still hold an inherited descriptor.
        // Explicit unlocking releases this guard without waiting for its exec.
        if let Err(error) = fs2::FileExt::unlock(&self._file) {
            eprintln!("Could not release the Mux installation lock: {error}");
        }
    }
}

pub(crate) fn lock(bin: &Path, exclusive: bool) -> Result<InstallationLock> {
    let path = bin.join(LOCK);
    let file = if exclusive || !path.exists() {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
    } else {
        OpenOptions::new().read(true).open(&path)
    }
    .context("Open installation lock")?;
    let result = if exclusive {
        fs2::FileExt::try_lock_exclusive(&file)
    } else {
        fs2::FileExt::try_lock_shared(&file)
    };
    result.context("Mux installation is busy; close Mux processes and editors, then retry")?;
    Ok(InstallationLock { _file: file })
}
pub fn shared_installation_lock(executable: &Path) -> Result<Option<InstallationLock>> {
    let bin = executable
        .parent()
        .ok_or("Executable has no parent directory")?;
    if !bin.join(LOCK).exists() && !bin.join(RECEIPT).exists() {
        return Ok(None);
    }
    // Lock before reading: the receipt is briefly absent while it is replaced.
    let guard = lock(bin, false)?;
    load(executable)?
        .ok_or("Mux installation receipt is missing; reinstall Mux to recover the installation")?;
    Ok(Some(guard))
}
pub(crate) fn validate_launcher(executable: &Path) -> Result<()> {
    let expected = if cfg!(windows) { "mux.exe" } else { "mux" };
    if executable.file_name().and_then(|n| n.to_str()) != Some(expected) {
        return Err("Run mux update through the installed mux launcher.".into());
    }
    Ok(())
}
pub(crate) fn load(executable: &Path) -> Result<Option<Receipt>> {
    let parent = executable
        .parent()
        .ok_or("Executable has no parent directory")?;
    let bytes = match fs::read(parent.join(RECEIPT)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Read installation receipt: {e}")),
    };
    let receipt: Receipt =
        serde_json::from_slice(&bytes).context("Invalid installation receipt")?;
    let canonical = fs::canonicalize(parent).context("Resolve installation directory")?;
    if receipt.schema != 1
        || receipt.bin_dir != canonical
        || fs::canonicalize(&receipt.lib_dir).context("Resolve library directory")?
            != receipt.lib_dir
    {
        return Err("Installation receipt does not match this executable. Reinstall Mux.".into());
    }
    if receipt.bin_dir == receipt.lib_dir {
        return Err("Binary and library installation directories must be different".into());
    }
    validate_target(&receipt.target)?;
    semver::Version::parse(&receipt.version).context("Invalid receipt version")?;
    validate_ownership(&receipt.files)?;
    let launcher = if cfg!(windows) { "mux.exe" } else { "mux" };
    if !receipt
        .files
        .iter()
        .any(|f| f.area == Area::Bin && f.name == launcher)
    {
        return Err("Receipt does not own the Mux launcher".into());
    }
    Ok(Some(receipt))
}
pub fn installed_runtime_dir(executable: &Path) -> Result<Option<PathBuf>> {
    Ok(load(executable)?.map(|r| r.lib_dir))
}
pub fn installed_version(executable: &Path) -> Result<Option<String>> {
    Ok(load(executable)?.map(|r| r.version))
}
pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':'])
        || name.starts_with(".mux-")
    {
        return Err(format!("Unsafe bundle filename: {name}"));
    }
    #[cfg(windows)]
    {
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
        if device || name.ends_with(['.', ' ']) {
            return Err(format!("Unsafe Windows bundle filename: {name}"));
        }
    }
    Ok(())
}
fn validate_ownership(files: &[OwnedFile]) -> Result<()> {
    let mut destinations = std::collections::BTreeSet::new();
    for file in files {
        validate_name(&file.name)?;
        let name = if cfg!(windows) {
            file.name.to_lowercase()
        } else {
            file.name.clone()
        };
        if !destinations.insert((file.area, name)) {
            return Err("Receipt or bundle contains duplicate destination paths".into());
        }
    }
    Ok(())
}
pub(crate) fn validate_target(target: &str) -> Result<()> {
    let targets: &[&str] = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        &["linux-x86_64", "linux-x86_64-ubuntu-latest"]
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        &["macos-x86_64"]
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        &["macos-aarch64"]
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        &["windows-x86_64"]
    } else {
        &[]
    };
    if !targets.contains(&target) {
        return Err(format!("Unsupported installation target: {target}"));
    }
    Ok(())
}
impl Receipt {
    pub fn destination(&self, file: &OwnedFile) -> PathBuf {
        match file.area {
            Area::Bin => &self.bin_dir,
            Area::Lib => &self.lib_dir,
        }
        .join(&file.name)
    }
}
pub(crate) fn bundle_files(bundle: &Path) -> Result<Vec<OwnedFile>> {
    let mut files = Vec::new();
    for (area, directory) in [(Area::Bin, "bin"), (Area::Lib, "lib")] {
        for entry in fs::read_dir(bundle.join(directory)).context("Read bundle directory")? {
            let entry = entry.context("Read bundle file")?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Non-UTF8 bundle filename")?;
            validate_name(&name)?;
            if !entry
                .file_type()
                .context("Read bundle file type")?
                .is_file()
            {
                return Err("Bundle may contain only regular files in bin and lib".into());
            }
            files.push(OwnedFile {
                area,
                name: name.clone(),
            });
            if cfg!(windows) && area == Area::Lib && name.ends_with(".dll") {
                files.push(OwnedFile {
                    area: Area::Bin,
                    name,
                });
            }
        }
    }
    let required: &[(&str, &str)] = if cfg!(windows) {
        &[
            ("bin", "mux-driver.exe"),
            ("lib", "mux_runtime.dll"),
            ("lib", "mux_runtime.lib"),
        ]
    } else if cfg!(target_os = "macos") {
        &[("lib", "libmux_runtime.a"), ("lib", "libmux_runtime.dylib")]
    } else {
        &[("lib", "libmux_runtime.a"), ("lib", "libmux_runtime.so")]
    };
    for (area, name) in required {
        if !bundle.join(area).join(name).is_file() {
            return Err(format!("Bundle is missing {area}/{name}"));
        }
    }
    files.sort();
    files.dedup();
    validate_ownership(&files)?;
    let launcher = if cfg!(windows) { "mux.exe" } else { "mux" };
    if !files
        .iter()
        .any(|f| f.area == Area::Bin && f.name == launcher)
    {
        return Err("Bundle has no Mux executable".into());
    }
    Ok(files)
}
pub fn register_install(
    bundle: &Path,
    bin_dir: &Path,
    lib_dir: &Path,
    target: &str,
    version: &str,
) -> Result<()> {
    validate_target(target)?;
    semver::Version::parse(version).context("Invalid installation version")?;
    let receipt = Receipt {
        schema: 1,
        target: target.into(),
        version: version.into(),
        bin_dir: fs::canonicalize(bin_dir).context("Resolve binary directory")?,
        lib_dir: fs::canonicalize(lib_dir).context("Resolve library directory")?,
        files: bundle_files(bundle)?,
    };
    if receipt.bin_dir == receipt.lib_dir {
        return Err("Binary and library installation directories must be different".into());
    }
    let _lock = lock(&receipt.bin_dir, true)?;
    for file in &receipt.files {
        if !receipt.destination(file).is_file() {
            return Err(format!("Installed bundle file is missing: {}", file.name));
        }
    }
    fs::write(
        receipt.bin_dir.join(RECEIPT),
        serde_json::to_vec_pretty(&receipt).context("Serialize receipt")?,
    )
    .context("Write installation receipt")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let lib = root.path().join("custom-libraries");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&lib).unwrap();
        let launcher = if cfg!(windows) { "mux.exe" } else { "mux" };
        let target = if cfg!(windows) {
            "windows-x86_64"
        } else if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                "macos-aarch64"
            } else {
                "macos-x86_64"
            }
        } else {
            "linux-x86_64"
        };
        let receipt = Receipt {
            schema: 1,
            target: target.into(),
            version: "0.14.0".into(),
            bin_dir: fs::canonicalize(&bin).unwrap(),
            lib_dir: fs::canonicalize(&lib).unwrap(),
            files: vec![OwnedFile {
                area: Area::Bin,
                name: launcher.into(),
            }],
        };
        fs::write(bin.join(RECEIPT), serde_json::to_vec(&receipt).unwrap()).unwrap();
        let executable = bin.join(launcher);
        (root, executable)
    }
    #[test]
    fn receipt_preserves_custom_library_directory() {
        let (root, exe) = fixture();
        assert_eq!(
            installed_runtime_dir(&exe).unwrap().unwrap(),
            fs::canonicalize(root.path().join("custom-libraries")).unwrap()
        );
    }
    #[test]
    fn shared_and_exclusive_installation_locks_conflict() {
        let (_root, exe) = fixture();
        let receipt = load(&exe).unwrap().unwrap();
        let first = shared_installation_lock(&exe).unwrap().unwrap();
        let second = shared_installation_lock(&exe).unwrap().unwrap();
        assert!(lock(&receipt.bin_dir, true).is_err());
        drop(first);
        drop(second);
        let exclusive = lock(&receipt.bin_dir, true).unwrap();
        assert!(shared_installation_lock(&exe).is_err());
        drop(exclusive);
        assert!(shared_installation_lock(&exe).is_ok());
    }
    #[test]
    fn mismatched_receipt_and_unsafe_ownership_are_rejected() {
        let (_root, exe) = fixture();
        let mut receipt = load(&exe).unwrap().unwrap();
        receipt.files.push(OwnedFile {
            area: Area::Lib,
            name: "../user-file".into(),
        });
        fs::write(
            receipt.bin_dir.join(RECEIPT),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert!(load(&exe).is_err());
        receipt.files.pop();
        receipt.lib_dir = receipt.bin_dir.clone();
        fs::write(
            receipt.bin_dir.join(RECEIPT),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert!(load(&exe).is_err());
    }
    #[test]
    fn missing_receipt_during_update_cannot_bypass_lock() {
        let (_root, exe) = fixture();
        let receipt = load(&exe).unwrap().unwrap();
        let exclusive = lock(&receipt.bin_dir, true).unwrap();
        fs::remove_file(receipt.bin_dir.join(RECEIPT)).unwrap();
        assert!(shared_installation_lock(&exe).is_err());
        drop(exclusive);
        assert!(shared_installation_lock(&exe).is_err());
    }
}
