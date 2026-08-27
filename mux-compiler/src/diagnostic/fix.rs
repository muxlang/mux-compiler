//! Validation, application, and transactional persistence of compiler edits.

use super::{Applicability, FileId, Files, SourceRange, TextEdit};
use crate::lexer::Span;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;

/// Source intervals produced while recovering from syntax errors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryIntervals {
    intervals: HashMap<FileId, Vec<SourceRange>>,
}

impl RecoveryIntervals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, file_id: FileId, range: SourceRange) {
        self.intervals.entry(file_id).or_default().push(range);
    }

    pub fn extend(&mut self, other: Self) {
        for (file_id, ranges) in other.intervals {
            for range in ranges {
                self.add(file_id, range);
            }
        }
    }

    pub fn touches(&self, file_id: FileId, range: SourceRange) -> bool {
        self.intervals.get(&file_id).is_some_and(|ranges| {
            ranges
                .iter()
                .any(|candidate| ranges_overlap(*candidate, range))
        })
    }
}

/// The result of applying validated edits in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEdits {
    pub files: BTreeMap<FileId, String>,
    pub changed_files: Vec<FileId>,
}

/// Errors that prevent a fix transaction from being applied.
#[derive(Debug)]
pub enum FixError {
    UnknownFile(FileId),
    EmbeddedFile(FileId),
    NonMachineApplicable {
        code: super::DiagnosticCode,
        applicability: Applicability,
    },
    InvalidRange {
        file_id: FileId,
        range: SourceRange,
        source_len: usize,
    },
    OverlappingEdits {
        file_id: FileId,
    },
    RecoveryRange {
        file_id: FileId,
        range: SourceRange,
    },
    InvalidLocation {
        span: Span,
        reason: &'static str,
    },
    Io(std::io::Error),
    Transaction(String),
}

impl fmt::Display for FixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFile(file_id) => write!(f, "edit refers to unknown file {file_id:?}"),
            Self::EmbeddedFile(file_id) => {
                write!(f, "edits to embedded file {file_id:?} are not allowed")
            }
            Self::NonMachineApplicable {
                code,
                applicability,
            } => write!(f, "{} is not machine-applicable ({applicability:?})", code),
            Self::InvalidRange {
                file_id,
                range,
                source_len,
            } => write!(
                f,
                "edit for {file_id:?} has invalid byte range {}..{} (source length {})",
                range.start_byte, range.end_byte, source_len
            ),
            Self::OverlappingEdits { file_id } => {
                write!(f, "machine-applicable edits overlap in {file_id:?}")
            }
            Self::RecoveryRange { file_id, range } => write!(
                f,
                "edit for {file_id:?} touches recovered source at {}..{}",
                range.start_byte, range.end_byte
            ),
            Self::InvalidLocation { span, reason } => write!(
                f,
                "cannot map diagnostic span at {}:{} to bytes: {reason}",
                span.row_start, span.col_start
            ),
            Self::Io(error) => write!(f, "I/O error while applying fixes: {error}"),
            Self::Transaction(message) => write!(f, "fix transaction failed: {message}"),
        }
    }
}

impl std::error::Error for FixError {}

impl From<std::io::Error> for FixError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Convert an existing row/column span to bytes using the exact source text.
///
/// Mux columns count terminal display width, not UTF-8 bytes. A location in
/// the middle of a wide character is rejected instead of risking a corrupted
/// source file. This mapping is deliberately performed only by fix handling.
pub fn source_range_for_span(source: &str, span: Span) -> Result<SourceRange, FixError> {
    let start = byte_offset_for_position(source, span.row_start, span.col_start, span)?;
    let end = match (span.row_end, span.col_end) {
        (Some(row), Some(column)) => byte_offset_for_position(source, row, column, span)?,
        (None, None) => start,
        _ => {
            return Err(FixError::InvalidLocation {
                span,
                reason: "span has only one end coordinate",
            });
        }
    };

    if end < start {
        return Err(FixError::InvalidLocation {
            span,
            reason: "span end precedes its start",
        });
    }
    Ok(SourceRange::new(start, end))
}

fn byte_offset_for_position(
    source: &str,
    row: usize,
    column: usize,
    span: Span,
) -> Result<usize, FixError> {
    if row == 0 || column == 0 {
        return Err(FixError::InvalidLocation {
            span,
            reason: "locations are one-based",
        });
    }

    let mut current_row = 1;
    let mut current_column = 1;
    for (byte, character) in source.char_indices() {
        if current_row == row && current_column == column {
            return Ok(byte);
        }
        if character == '\n' {
            current_row += 1;
            current_column = 1;
        } else {
            current_column += UnicodeWidthChar::width(character).unwrap_or(1);
        }
    }

    if current_row == row && current_column == column {
        return Ok(source.len());
    }

    Err(FixError::InvalidLocation {
        span,
        reason: "location is outside the source or inside a wide character",
    })
}

/// Apply machine-applicable edits without touching the filesystem.
pub fn apply_in_memory(
    files: &Files,
    edits: &[TextEdit],
    recovery: &RecoveryIntervals,
) -> Result<AppliedEdits, FixError> {
    let mut by_file: BTreeMap<FileId, Vec<&TextEdit>> = BTreeMap::new();

    for edit in edits {
        if !edit.is_machine_applicable() {
            return Err(FixError::NonMachineApplicable {
                code: edit.diagnostic_code,
                applicability: edit.applicability,
            });
        }

        let source = files
            .source(edit.file_id)
            .ok_or(FixError::UnknownFile(edit.file_id))?;
        let path = files
            .path(edit.file_id)
            .ok_or(FixError::UnknownFile(edit.file_id))?;
        if path.to_string_lossy().starts_with("<embedded>/") {
            return Err(FixError::EmbeddedFile(edit.file_id));
        }
        validate_range(edit.file_id, edit.range, source)?;
        if recovery.touches(edit.file_id, edit.range) {
            return Err(FixError::RecoveryRange {
                file_id: edit.file_id,
                range: edit.range,
            });
        }
        by_file.entry(edit.file_id).or_default().push(edit);
    }

    let mut updated = BTreeMap::new();
    let mut changed_files = Vec::new();
    for (file_id, mut file_edits) in by_file {
        file_edits.sort_by_key(|edit| (edit.range.start_byte, edit.range.end_byte));
        for pair in file_edits.windows(2) {
            if ranges_overlap(pair[0].range, pair[1].range) {
                return Err(FixError::OverlappingEdits { file_id });
            }
        }

        let original = files
            .source(file_id)
            .ok_or(FixError::UnknownFile(file_id))?;
        let mut source = original.to_string();
        for edit in file_edits.into_iter().rev() {
            source.replace_range(
                edit.range.start_byte..edit.range.end_byte,
                &edit.replacement,
            );
        }
        if source != original {
            changed_files.push(file_id);
        }
        updated.insert(file_id, source);
    }

    Ok(AppliedEdits {
        files: updated,
        changed_files,
    })
}

/// Stage edits and validate the complete staged source set before returning it.
/// The callback is where the compiler's parser and semantic analyzer are run;
/// no filesystem operation is performed by this function.
pub fn apply_and_validate<F>(
    files: &Files,
    edits: &[TextEdit],
    recovery: &RecoveryIntervals,
    validate: F,
) -> Result<AppliedEdits, FixError>
where
    F: FnOnce(&BTreeMap<FileId, String>) -> Result<(), String>,
{
    let applied = apply_in_memory(files, edits, recovery)?;
    if !applied.changed_files.is_empty() {
        validate(&applied.files).map_err(FixError::Transaction)?;
    }
    Ok(applied)
}

fn validate_range(file_id: FileId, range: SourceRange, source: &str) -> Result<(), FixError> {
    if range.start_byte > range.end_byte
        || range.end_byte > source.len()
        || !source.is_char_boundary(range.start_byte)
        || !source.is_char_boundary(range.end_byte)
    {
        return Err(FixError::InvalidRange {
            file_id,
            range,
            source_len: source.len(),
        });
    }
    Ok(())
}

fn ranges_overlap(left: SourceRange, right: SourceRange) -> bool {
    if left.is_empty() && right.is_empty() {
        return left.start_byte == right.start_byte;
    }
    if left.is_empty() {
        return left.start_byte >= right.start_byte && left.start_byte < right.end_byte;
    }
    if right.is_empty() {
        return right.start_byte >= left.start_byte && right.start_byte < left.end_byte;
    }
    left.start_byte < right.end_byte && right.start_byte < left.end_byte
}

/// Produce a concise unified-style diff for changed files.
pub fn unified_diff(files: &Files, applied: &AppliedEdits) -> String {
    let mut output = String::new();
    for (file_id, updated) in &applied.files {
        let Some(original) = files.source(*file_id) else {
            continue;
        };
        if original == updated {
            continue;
        }
        let path = files
            .path(*file_id)
            .map_or_else(|| "<unknown>".into(), |path| path.display().to_string());
        let old_lines = diff_lines(original);
        let new_lines = diff_lines(updated);
        let old_start = if old_lines.is_empty() { 0 } else { 1 };
        let new_start = if new_lines.is_empty() { 0 } else { 1 };
        output.push_str(&format!(
            "--- {path}\n+++ {path}\n@@ -{old_start},{} +{new_start},{} @@\n",
            old_lines.len(),
            new_lines.len()
        ));
        for (line, has_newline) in old_lines {
            append_diff_line(&mut output, '-', line, has_newline);
        }
        for (line, has_newline) in new_lines {
            append_diff_line(&mut output, '+', line, has_newline);
        }
    }
    output
}

fn diff_lines(source: &str) -> Vec<(&str, bool)> {
    if source.is_empty() {
        return Vec::new();
    }
    source
        .split_inclusive('\n')
        .map(|line| match line.strip_suffix('\n') {
            Some(line) => (line, true),
            None => (line, false),
        })
        .collect()
}

fn append_diff_line(output: &mut String, prefix: char, line: &str, has_newline: bool) {
    output.push(prefix);
    output.push_str(line);
    output.push('\n');
    if !has_newline {
        output.push_str("\\ No newline at end of file\n");
    }
}

/// Atomically write all changed files, following symlinks to their targets.
///
/// Every temporary file is created beside its target. Originals are moved to
/// private backups before replacement; if any later replacement fails, all
/// already-replaced files are restored before the error is returned.
pub fn write_transaction(
    files: &Files,
    updates: &BTreeMap<FileId, String>,
) -> Result<Vec<PathBuf>, FixError> {
    let mut changed_updates = Vec::new();
    for (file_id, contents) in updates {
        let original = files
            .source(*file_id)
            .ok_or(FixError::UnknownFile(*file_id))?;
        if original != contents {
            changed_updates.push((file_id, contents));
        }
    }
    if changed_updates.is_empty() {
        return Ok(Vec::new());
    }

    let mut targets = Vec::new();
    let mut seen_targets = HashSet::new();
    for (file_id, contents) in changed_updates {
        let displayed_path = files
            .path(*file_id)
            .ok_or(FixError::UnknownFile(*file_id))?;
        if displayed_path.to_string_lossy().starts_with("<embedded>/") {
            return Err(FixError::EmbeddedFile(*file_id));
        }
        let target = fs::canonicalize(displayed_path)?;
        if !seen_targets.insert(target.clone()) {
            return Err(FixError::Transaction(format!(
                "multiple edits resolve to the same file: {}",
                target.display()
            )));
        }
        let metadata = fs::metadata(&target)?;
        if !metadata.is_file() {
            return Err(FixError::Transaction(format!(
                "{} is not a regular file",
                target.display()
            )));
        }
        targets.push((*file_id, target, contents, metadata.permissions()));
    }

    let transaction_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut staged = Vec::new();
    for (index, (_, target, contents, permissions)) in targets.iter().enumerate() {
        let temp = target.with_file_name(format!(
            ".{}.mux-fix-{}-{}.tmp",
            target.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id(),
            transaction_id + index as u128
        ));
        let result = (|| -> Result<(), FixError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            file.set_permissions(permissions.clone())?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temp);
            for path in staged {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
        staged.push(temp);
    }

    let mut backups = Vec::new();
    for (index, (_, target, _, _)) in targets.iter().enumerate() {
        let backup = target.with_file_name(format!(
            ".{}.mux-fix-{}-{}.bak",
            target.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id(),
            transaction_id + index as u128
        ));
        if let Err(error) = fs::rename(target, &backup) {
            rollback_staged(&staged);
            if let Err(rollback_error) = rollback_backups(&backups) {
                return Err(FixError::Transaction(format!(
                    "could not move {} to its backup: {error}; rollback also failed: {rollback_error}",
                    target.display()
                )));
            }
            return Err(FixError::Io(error));
        }
        backups.push((target.clone(), backup));
    }

    for (index, (target, _backup)) in backups.iter().enumerate() {
        if let Err(error) = fs::rename(&staged[index], target) {
            for remaining in staged.iter().skip(index) {
                let _ = fs::remove_file(remaining);
            }
            if let Err(rollback_error) = rollback_backups(&backups) {
                return Err(FixError::Transaction(format!(
                    "could not replace {}: {error}; rollback also failed: {rollback_error}",
                    target.display()
                )));
            }
            return Err(FixError::Io(error));
        }
    }

    // Replacement is already committed at this point. Backup cleanup is
    // deliberately best-effort: reporting cleanup as a failed transaction
    // after replacing the files would falsely claim that nothing changed.
    for (_, backup) in &backups {
        let _ = fs::remove_file(backup);
    }
    Ok(targets
        .into_iter()
        .map(|(_, target, _, _)| target)
        .collect())
}

fn rollback_staged(staged: &[PathBuf]) {
    for path in staged {
        let _ = fs::remove_file(path);
    }
}

fn rollback_backups(backups: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let mut failures = Vec::new();
    for (target, backup) in backups.iter().rev() {
        if target.exists()
            && let Err(error) = fs::remove_file(target)
        {
            failures.push(format!("remove {}: {error}", target.display()));
            continue;
        }
        if let Err(error) = fs::rename(backup, target) {
            failures.push(format!("restore {}: {error}", target.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{DiagnosticCode, Files};

    fn files(source: &str) -> (Files, FileId) {
        let mut files = Files::new();
        let file_id = files.add("test.mux", source.to_string());
        (files, file_id)
    }

    #[test]
    fn maps_ascii_and_unicode_spans_to_utf8_bytes() {
        let source = "auto x = こんにちは\nvalue\n";
        let span = Span {
            row_start: 1,
            row_end: Some(1),
            col_start: 10,
            col_end: Some(20),
        };
        let range = source_range_for_span(source, span).unwrap();
        assert_eq!(&source[range.start_byte..range.end_byte], "こんにちは");

        let second_line = Span {
            row_start: 2,
            row_end: None,
            col_start: 1,
            col_end: None,
        };
        assert_eq!(
            source_range_for_span(source, second_line)
                .unwrap()
                .start_byte,
            25
        );
    }

    #[test]
    fn rejects_invalid_and_overlapping_edits() {
        let (files, file_id) = files("abcdef");
        let edits = [
            TextEdit::machine_applicable(
                file_id,
                SourceRange::new(1, 3),
                "x",
                DiagnosticCode::RedundantConstruct,
            ),
            TextEdit::machine_applicable(
                file_id,
                SourceRange::new(2, 4),
                "y",
                DiagnosticCode::RedundantConstruct,
            ),
        ];
        assert!(matches!(
            apply_in_memory(&files, &edits, &RecoveryIntervals::new()),
            Err(FixError::OverlappingEdits { .. })
        ));

        let invalid = [TextEdit::machine_applicable(
            file_id,
            SourceRange::new(2, 20),
            "x",
            DiagnosticCode::RedundantConstruct,
        )];
        assert!(matches!(
            apply_in_memory(&files, &invalid, &RecoveryIntervals::new()),
            Err(FixError::InvalidRange { .. })
        ));
    }

    #[test]
    fn rejects_non_machine_applicable_and_recovery_edits() {
        let (files, file_id) = files("abcdef");
        let mut recovery = RecoveryIntervals::new();
        recovery.add(file_id, SourceRange::new(2, 4));
        let edit = TextEdit {
            file_id,
            range: SourceRange::new(2, 3),
            replacement: "x".into(),
            applicability: Applicability::MaybeIncorrect,
            diagnostic_code: DiagnosticCode::RedundantConstruct,
        };
        assert!(matches!(
            apply_in_memory(&files, &[edit], &recovery),
            Err(FixError::NonMachineApplicable { .. })
        ));

        let edit = TextEdit::machine_applicable(
            file_id,
            SourceRange::new(2, 3),
            "x",
            DiagnosticCode::RedundantConstruct,
        );
        assert!(matches!(
            apply_in_memory(&files, &[edit], &recovery),
            Err(FixError::RecoveryRange { .. })
        ));
    }

    #[test]
    fn applies_edits_from_right_to_left() {
        let (files, file_id) = files("abcdef");
        let edits = [
            TextEdit::machine_applicable(
                file_id,
                SourceRange::new(0, 1),
                "A",
                DiagnosticCode::RedundantConstruct,
            ),
            TextEdit::machine_applicable(
                file_id,
                SourceRange::new(4, 6),
                "EF",
                DiagnosticCode::RedundantConstruct,
            ),
        ];
        let result = apply_in_memory(&files, &edits, &RecoveryIntervals::new()).unwrap();
        assert_eq!(result.files.get(&file_id).unwrap(), "AbcdEF");
        assert_eq!(result.changed_files, vec![file_id]);
    }

    #[test]
    fn unified_diff_has_valid_hunk_counts_and_no_newline_markers() {
        let (files, file_id) = files("first\nsecond");
        let edit = TextEdit::machine_applicable(
            file_id,
            SourceRange::new(6, 12),
            "changed",
            DiagnosticCode::RedundantConstruct,
        );
        let applied = apply_in_memory(&files, &[edit], &RecoveryIntervals::new()).unwrap();
        let diff = unified_diff(&files, &applied);
        assert!(diff.contains("@@ -1,2 +1,2 @@\n"), "diff: {diff}");
        assert!(diff.contains("-first\n-second\n\\ No newline at end of file\n"));
        assert!(diff.contains("+first\n+changed\n\\ No newline at end of file\n"));
    }

    #[test]
    fn writes_changed_source_transactionally_and_preserves_newlines() {
        let directory = std::env::temp_dir().join(format!(
            "mux_fix_write_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("program.mux");
        let original = "auto value = 1\r\n";
        std::fs::write(&path, original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        #[cfg(unix)]
        let original_mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&path).unwrap().permissions().mode()
        };

        let mut files = Files::new();
        let file_id = files.add(&path, original.to_string());
        let edit = TextEdit::machine_applicable(
            file_id,
            SourceRange::new(13, 14),
            "2",
            DiagnosticCode::RedundantConstruct,
        );
        let applied = apply_in_memory(&files, &[edit], &RecoveryIntervals::new()).unwrap();
        write_transaction(&files, &applied.files).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "auto value = 2\r\n"
        );
        #[cfg(unix)]
        assert_eq!(
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(&path).unwrap().permissions().mode()
            },
            original_mode
        );
        assert!(!path.with_file_name(".program.mux-fix").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn does_not_rewrite_an_unchanged_update() {
        let directory = std::env::temp_dir().join(format!(
            "mux_fix_noop_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("program.mux");
        std::fs::write(&path, "abc").unwrap();

        let mut files = Files::new();
        let file_id = files.add(&path, "abc".to_string());
        let edit = TextEdit::machine_applicable(
            file_id,
            SourceRange::new(0, 3),
            "abc",
            DiagnosticCode::RedundantConstruct,
        );
        let applied = apply_in_memory(&files, &[edit], &RecoveryIntervals::new()).unwrap();
        assert!(applied.changed_files.is_empty());
        assert!(
            write_transaction(&files, &applied.files)
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinks_when_writing() {
        use std::os::unix::fs::symlink;

        let directory = std::env::temp_dir().join(format!(
            "mux_fix_link_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let target = directory.join("target.mux");
        let link = directory.join("link.mux");
        std::fs::write(&target, "abc").unwrap();
        symlink(&target, &link).unwrap();

        let mut files = Files::new();
        let file_id = files.add(&link, "abc".to_string());
        let edit = TextEdit::machine_applicable(
            file_id,
            SourceRange::new(0, 3),
            "xyz",
            DiagnosticCode::RedundantConstruct,
        );
        let applied = apply_in_memory(&files, &[edit], &RecoveryIntervals::new()).unwrap();
        write_transaction(&files, &applied.files).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "xyz");
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "xyz");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
