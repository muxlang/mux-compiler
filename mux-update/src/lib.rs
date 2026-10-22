mod cancellation;
mod download;
mod receipt;
mod transaction;

pub use download::run_download_worker;
pub use receipt::{
    InstallationLock, install_bundle, installed_runtime_dir, installed_version, register_install,
    shared_installation_lock,
};
use semver::Version;
use std::{env, path::Path};

type Result<T> = std::result::Result<T, String>;
trait Context<T> {
    fn context(self, message: &str) -> Result<T>;
}
impl<T, E: std::fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, message: &str) -> Result<T> {
        self.map_err(|e| format!("{message}: {e}"))
    }
}

pub fn cleanup_old_launchers(executable: &Path) -> Result<()> {
    let Some(receipt) = receipt::load(executable)? else {
        return Ok(());
    };
    let Ok(_lock) = receipt::lock(&receipt.bin_dir, true) else {
        return Ok(());
    };
    for entry in std::fs::read_dir(&receipt.bin_dir).context("Read installation directory")? {
        let entry = entry.context("Read installation entry")?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".mux-update-backup-launcher-")
        {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
                    ) => {}
                Err(error) => return Err(format!("Remove old launcher: {error}")),
            }
        }
    }
    Ok(())
}

pub fn run(check: bool, current_version: &str) -> Result<()> {
    let executable = env::current_exe().context("Locate running Mux")?;
    let installation = receipt::load(&executable)?;
    if !check && installation.is_none() {
        return Err("This Mux installation has no installer receipt. Reinstall using the official installer before running mux update.".into());
    }
    cancellation::install_handler()?;
    let current = Version::parse(
        installation
            .as_ref()
            .map_or(current_version, |r| r.version.as_str()),
    )
    .context("Invalid installed version")?;
    println!("Current version: {current}");
    let release = download::latest()?;
    println!("Latest stable:   {}", release.version);
    if current >= release.version {
        println!("Mux {current} is up to date.");
        return Ok(());
    }
    if check {
        println!("Update available. Run `mux update` to install.");
        return Ok(());
    }
    let installation = installation.ok_or("This Mux installation has no installer receipt. Reinstall using the official installer before running mux update.")?;
    receipt::validate_launcher(&executable)?;
    let _lock = receipt::lock(&installation.bin_dir, true)?;
    let latest = receipt::load(&executable)?.ok_or("Installation receipt disappeared")?;
    if latest != installation {
        return Err(
            "Installation changed while checking for updates. Run mux update again.".into(),
        );
    }
    println!("\nDownloading Mux {}...", release.version);
    let stage = tempfile::tempdir().context("Create update staging directory")?;
    cancellation::check()?;
    let bundle = download::stage(&release, &installation.target, stage.path())?;
    println!("Verifying checksum... OK\nInstalling compiler and bundled libraries...");
    transaction::install(&installation, &bundle, &release.version.to_string(), |_| {
        Ok(())
    })?;
    println!("Updated Mux from {current} to {}.", release.version);
    Ok(())
}
