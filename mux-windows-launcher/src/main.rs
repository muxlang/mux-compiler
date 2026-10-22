use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mux", about = "CLI for the Mux Programming Language")]
struct UpdateCli {
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(short, long)]
    intermediate: bool,
    #[arg(short = 'd', long, global = true)]
    deny_warnings: bool,
    #[command(subcommand)]
    command: UpdateCommand,
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// Update Mux to the latest stable release.
    Update(UpdateArgs),
}

#[derive(Args)]
struct UpdateArgs {
    /// Check for an update without installing it.
    #[arg(long)]
    check: bool,
}

fn main() {
    let result = run();
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32, String> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if let Some(result) = mux_update::run_download_worker(&arguments) {
        return result.map(|()| 0);
    }
    if arguments.len() == 1 && arguments[0] == "__version" {
        println!("{}", env!("MUX_COMPILER_VERSION"));
        return Ok(0);
    }
    let executable = env::current_exe()
        .map_err(|error| format!("Cannot locate the Mux installation: {error}"))?;
    if update_command_index(&arguments).is_some() {
        let options = UpdateCli::parse_from(
            std::iter::once(OsString::from("mux")).chain(arguments.iter().cloned()),
        );
        let UpdateCommand::Update(update) = options.command;
        let current = mux_update::installed_version(&executable)?
            .unwrap_or_else(|| env!("MUX_COMPILER_VERSION").to_owned());
        mux_update::run(update.check, &current)?;
        return Ok(0);
    }

    let _installation_lock = if arguments
        .first()
        .is_some_and(|arg| arg == "__register-install")
    {
        None
    } else {
        mux_update::cleanup_old_launchers(&executable)?;
        mux_update::shared_installation_lock(&executable)?
    };
    forward_to_driver(&executable, &arguments)
}

fn update_command_index(arguments: &[OsString]) -> Option<usize> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let argument = argument.to_str()?;
        match argument {
            "update" => return Some(index),
            "--intermediate" | "--deny-warnings" => {}
            "--output" => index += 1,
            argument if argument.starts_with("--output=") => {}
            argument if argument.starts_with('-') && !argument.starts_with("--") => {
                let mut options = argument[1..].chars().peekable();
                options.peek()?;
                while let Some(option) = options.next() {
                    match option {
                        'd' | 'i' => {}
                        'o' => {
                            if options.peek().is_none() {
                                index += 1;
                            }
                            break;
                        }
                        _ => return None,
                    }
                }
            }
            _ => return None,
        }
        index += 1;
    }
    None
}

fn forward_to_driver(executable: &Path, arguments: &[OsString]) -> Result<i32, String> {
    let driver = executable.with_file_name("mux-driver.exe");
    install_console_handler()?;
    let status = Command::new(&driver)
        .args(arguments)
        .status()
        .map_err(|error| {
            format!(
                "Cannot start {}: {error}. Reinstall Mux if its files are missing.",
                driver.display()
            )
        })?;
    if let Some(code) = status.code() {
        return Ok(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(128 + status.signal().unwrap_or(1))
    }
    #[cfg(not(unix))]
    {
        Err("The Mux compiler exited without an exit status".to_owned())
    }
}

#[cfg(windows)]
fn install_console_handler() -> Result<(), String> {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, GetConsoleCP, SetConsoleCtrlHandler,
    };

    unsafe extern "system" fn handler(event: u32) -> i32 {
        i32::from(matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT))
    }

    if unsafe { GetConsoleCP() } == 0 {
        return Ok(());
    }
    // The driver receives console signals too. Keep the launcher waiting for its exit.
    if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
        return Err(format!(
            "Cannot configure Mux console handling: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn install_console_handler() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::update_command_index;
    use std::ffi::OsString;

    fn index(arguments: &[&str]) -> Option<usize> {
        update_command_index(&arguments.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn recognizes_update_after_compiler_root_options() {
        assert_eq!(index(&["update", "--check"]), Some(0));
        assert_eq!(index(&["-d", "update"]), Some(1));
        assert_eq!(index(&["-di", "update"]), Some(1));
        assert_eq!(index(&["--output", "program", "update"]), Some(2));
        assert_eq!(index(&["-oprogram", "update"]), Some(1));
        assert_eq!(index(&["--output=program", "update"]), Some(1));
    }

    #[test]
    fn does_not_intercept_other_command_arguments() {
        for arguments in [
            vec!["run", "update"],
            vec!["build", "update"],
            vec!["help", "update"],
            vec!["--output", "update", "version"],
            vec!["--unknown", "update"],
            vec!["--help"],
            vec!["lsp"],
            vec![],
        ] {
            assert_eq!(index(&arguments), None, "{arguments:?}");
        }
    }
}
