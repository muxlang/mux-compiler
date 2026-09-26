use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{FormatError, format_source};

const IGNORED_DIRECTORIES: &[&str] = &[".git", "target", "node_modules"];
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FormatOutcome {
    /// Paths whose contents differed from the formatter output.
    pub changed: Vec<PathBuf>,
}

/// Format explicit files or recursively discover `.mux` files under paths.
/// If no paths are given, discovery starts at the current directory.
///
/// All inputs are read and formatted before any file is written. This keeps a
/// parse failure from leaving a partially formatted set of files.
pub fn format_paths(paths: &[PathBuf], check: bool) -> Result<FormatOutcome, FormatError> {
    let files = discover_files(paths)?;
    let mut changes = Vec::new();

    for path in files {
        let original = fs::read_to_string(&path).map_err(|error| {
            FormatError::io(format!("could not read {}: {error}", path.display()))
        })?;
        let formatted = format_source(&original).map_err(|error| error.with_path(&path))?;
        if original != formatted {
            changes.push(Change {
                path,
                original,
                formatted,
            });
        }
    }

    let changed = changes.iter().map(|change| change.path.clone()).collect();
    if !check {
        for change in changes {
            replace_if_unchanged(change)?;
        }
    }
    Ok(FormatOutcome { changed })
}

fn discover_files(requested: &[PathBuf]) -> Result<Vec<PathBuf>, FormatError> {
    let roots = if requested.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        requested.to_vec()
    };
    let mut found = Vec::new();
    for root in roots {
        let metadata = fs::symlink_metadata(&root)
            .map_err(|error| FormatError::io(format!("{}: {error}", root.display())))?;
        let resolved = if metadata.file_type().is_symlink() {
            fs::canonicalize(&root)
                .map_err(|error| FormatError::io(format!("{}: {error}", root.display())))?
        } else {
            root.clone()
        };
        let resolved_metadata = fs::metadata(&resolved)
            .map_err(|error| FormatError::io(format!("{}: {error}", root.display())))?;
        if resolved_metadata.is_dir() {
            discover_directory(&resolved, &mut found)?;
        } else if resolved_metadata.is_file() {
            if !is_mux_file(&root) {
                return Err(FormatError::input(format!(
                    "{} is not a .mux file",
                    root.display()
                )));
            }
            // Store the canonical target so replacement does not replace an
            // explicitly supplied symlink with a regular file.
            found.push(resolved);
        } else {
            return Err(FormatError::input(format!(
                "{} is not a regular file or directory",
                root.display()
            )));
        }
    }

    // Canonical identities collapse spellings such as `dir/../dir/a.mux` and
    // repeated explicit paths while preserving one stable user-facing path.
    found.sort();
    let mut unique = Vec::with_capacity(found.len());
    let mut identities = HashSet::new();
    for path in found {
        let identity = fs::canonicalize(&path)
            .map_err(|error| FormatError::io(format!("{}: {error}", path.display())))?;
        if identities.insert(identity.clone()) {
            unique.push(identity);
        }
    }
    Ok(unique)
}

fn discover_directory(path: &Path, found: &mut Vec<PathBuf>) -> Result<(), FormatError> {
    let entries = fs::read_dir(path).map_err(|error| {
        FormatError::io(format!(
            "could not read directory {}: {error}",
            path.display()
        ))
    })?;
    let mut entries = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|error| {
            FormatError::io(format!(
                "could not read directory {}: {error}",
                path.display()
            ))
        })?;
    entries.sort();

    for entry in entries {
        let metadata = fs::symlink_metadata(&entry)
            .map_err(|error| FormatError::io(format!("{}: {error}", entry.display())))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if entry
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| IGNORED_DIRECTORIES.contains(&name))
            {
                continue;
            }
            discover_directory(&entry, found)?;
        } else if metadata.is_file() && is_mux_file(&entry) {
            found.push(entry);
        }
    }
    Ok(())
}

fn is_mux_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "mux")
}

struct Change {
    path: PathBuf,
    original: String,
    formatted: String,
}

fn replace_if_unchanged(change: Change) -> Result<(), FormatError> {
    replace_if_unchanged_with(change, || {})
}

fn replace_if_unchanged_with(
    change: Change,
    after_staging: impl FnOnce(),
) -> Result<(), FormatError> {
    let current = fs::read(&change.path).map_err(|error| {
        FormatError::io(format!(
            "could not re-read {}: {error}",
            change.path.display()
        ))
    })?;
    if current != change.original.as_bytes() {
        return Err(FormatError::io(format!(
            "{} changed while formatting; refusing to overwrite it",
            change.path.display()
        )));
    }

    let permissions = fs::metadata(&change.path)
        .map_err(|error| {
            FormatError::io(format!(
                "could not inspect {}: {error}",
                change.path.display()
            ))
        })?
        .permissions();
    let temporary = temporary_path(&change.path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| {
            FormatError::io(format!("could not create {}: {error}", temporary.display()))
        })?;

    let staged = (|| -> io::Result<()> {
        file.write_all(change.formatted.as_bytes())?;
        file.set_permissions(permissions)?;
        file.sync_all()
    })();
    drop(file);
    if let Err(error) = staged {
        let _ = fs::remove_file(&temporary);
        return Err(FormatError::io(format!(
            "could not stage {}: {error}",
            change.path.display()
        )));
    }

    // This seam makes the last-minute concurrent-edit guard deterministic in
    // tests; production callers leave the file untouched here.
    after_staging();

    // Recheck just before replacement. A concurrent writer can still race
    // between this read and the atomic rename, so this is a best-effort guard.
    let current = fs::read(&change.path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        FormatError::io(format!(
            "could not re-read {}: {error}",
            change.path.display()
        ))
    })?;
    if current != change.original.as_bytes() {
        let _ = fs::remove_file(&temporary);
        return Err(FormatError::io(format!(
            "{} changed while formatting; refusing to overwrite it",
            change.path.display()
        )));
    }

    if let Err(error) = atomic_replace(&temporary, &change.path) {
        let _ = fs::remove_file(&temporary);
        return Err(FormatError::io(format!(
            "could not replace {}: {error}",
            change.path.display()
        )));
    }
    Ok(())
}

fn temporary_path(target: &Path) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    target.with_file_name(format!(
        ".{}.mux-format-{}-{sequence}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ))
}

fn atomic_replace(replacement: &Path, target: &Path) -> io::Result<()> {
    crate::diagnostic::fix::atomic_replace(replacement, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let id = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("mux-format-files-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovery_is_recursive_sorted_and_skips_generated_directories() {
        let scratch = Scratch::new();
        fs::create_dir(scratch.path().join("target")).unwrap();
        fs::create_dir(scratch.path().join("nested")).unwrap();
        fs::write(scratch.path().join("z.mux"), "").unwrap();
        fs::write(scratch.path().join("nested/a.mux"), "").unwrap();
        fs::write(scratch.path().join("target/ignored.mux"), "").unwrap();
        fs::write(scratch.path().join("notes.txt"), "").unwrap();

        let files = discover_files(&[scratch.path().to_path_buf()]).unwrap();
        assert_eq!(
            files,
            vec![
                scratch.path().join("nested/a.mux"),
                scratch.path().join("z.mux")
            ]
        );
    }

    #[test]
    fn explicit_paths_must_exist_and_mux_files_must_have_the_extension() {
        let scratch = Scratch::new();
        let missing = scratch.path().join("missing.mux");
        assert!(discover_files(&[missing]).is_err());

        let other = scratch.path().join("source.txt");
        fs::write(&other, "").unwrap();
        assert!(discover_files(&[other]).is_err());
    }

    #[test]
    fn duplicate_explicit_inputs_are_processed_once() {
        let scratch = Scratch::new();
        let file = scratch.path().join("source.mux");
        fs::write(&file, "auto value=1").unwrap();

        let outcome = format_paths(&[file.clone(), file.clone()], false).unwrap();

        assert_eq!(outcome.changed, vec![fs::canonicalize(&file).unwrap()]);
        assert_eq!(fs::read_to_string(&file).unwrap(), "auto value = 1\n");
    }

    #[test]
    fn empty_directory_is_a_noop() {
        let scratch = Scratch::new();

        let outcome = format_paths(&[scratch.path().to_path_buf()], false).unwrap();

        assert!(outcome.changed.is_empty());
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn parse_error_in_batch_does_not_write_other_files() {
        let scratch = Scratch::new();
        let valid = scratch.path().join("a-valid.mux");
        let invalid = scratch.path().join("z-invalid.mux");
        fs::write(&valid, "auto value=1").unwrap();
        fs::write(&invalid, "auto =\n").unwrap();

        let error = format_paths(&[valid.clone(), invalid], false).unwrap_err();

        assert!(error.to_string().contains("z-invalid.mux"));
        assert_eq!(fs::read_to_string(valid).unwrap(), "auto value=1");
    }

    #[test]
    fn replacement_refuses_to_overwrite_a_source_changed_after_staging() {
        let scratch = Scratch::new();
        let path = scratch.path().join("source.mux");
        fs::write(&path, "auto value=1").unwrap();
        let change = Change {
            path: path.clone(),
            original: "auto value=1".to_string(),
            formatted: "auto value = 1\n".to_string(),
        };

        let error = replace_if_unchanged_with(change, || {
            fs::write(&path, "auto value=2").unwrap();
        })
        .unwrap_err();

        assert!(error.to_string().contains("changed while formatting"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "auto value=2");
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_permissions_and_check_mode_never_writes() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new();
        let file = scratch.path().join("source.mux");
        fs::write(&file, "auto value=1").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();

        let check = format_paths(std::slice::from_ref(&file), true).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "auto value=1");
        assert_eq!(check.changed, vec![fs::canonicalize(&file).unwrap()]);

        let formatted = format_paths(std::slice::from_ref(&file), false).unwrap();
        assert_eq!(formatted.changed, vec![fs::canonicalize(&file).unwrap()]);
        assert_eq!(fs::read_to_string(&file).unwrap(), "auto value = 1\n");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_discovery_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new();
        let child = scratch.path().join("child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("found.mux"), "").unwrap();
        symlink(scratch.path(), child.join("cycle")).unwrap();
        symlink(&child, scratch.path().join("linked-dir")).unwrap();

        assert_eq!(
            discover_files(&[scratch.path().to_path_buf()]).unwrap(),
            vec![child.join("found.mux")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_directory_symlink_is_traversed_without_preserving_the_link_path() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new();
        let target = scratch.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("source.mux"), "").unwrap();
        let alias = scratch.path().join("alias");
        symlink(&target, &alias).unwrap();

        assert_eq!(
            discover_files(&[alias]).unwrap(),
            vec![target.join("source.mux")]
        );
    }
}
