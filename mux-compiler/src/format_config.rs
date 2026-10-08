use std::env;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use mux_lang::formatter::{BraceStyle, FormatOptions, IndentType, TrailingComma, WherePosition};

const CONFIG_NAME: &str = "mux-project.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FormatConfig {
    pub enabled: bool,
    pub options: FormatOptions,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            options: FormatOptions::default(),
        }
    }
}

/// Load the nearest project formatter settings from the current directory.
/// The search stops after checking the nearest Git worktree root and never
/// checks the filesystem root.
pub(crate) fn load_from_current_directory() -> (FormatConfig, Vec<String>) {
    let Ok(current_dir) = env::current_dir() else {
        return (
            FormatConfig::default(),
            vec!["could not determine the working directory; using formatter defaults".into()],
        );
    };
    load_from_directory(&current_dir)
}

pub(crate) fn load_from_directory(current_dir: &Path) -> (FormatConfig, Vec<String>) {
    for directory in current_dir.ancestors() {
        if directory.parent().is_none() {
            break;
        }
        let config_path = directory.join(CONFIG_NAME);
        match fs::symlink_metadata(&config_path) {
            Ok(_) => return load_file(&config_path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return (
                    FormatConfig::default(),
                    vec![format!(
                        "{}: could not inspect formatter config ({error}); using defaults",
                        config_path.display()
                    )],
                );
            }
        }
        if directory.join(".git").exists() {
            break;
        }
    }
    (FormatConfig::default(), Vec::new())
}

fn load_file(path: &Path) -> (FormatConfig, Vec<String>) {
    let display_path = path.display().to_string();
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            return (
                FormatConfig::default(),
                vec![format!(
                    "{display_path}: could not read formatter config ({error}); using defaults"
                )],
            );
        }
    };
    let value: Value = match serde_json::from_str(&source) {
        Ok(value) => value,
        Err(error) => {
            return (
                FormatConfig::default(),
                vec![format!(
                    "{display_path}: invalid JSON ({error}); using formatter defaults"
                )],
            );
        }
    };
    let Some(root) = value.as_object() else {
        return (
            FormatConfig::default(),
            vec![format!(
                "{display_path}: expected a JSON object; using formatter defaults"
            )],
        );
    };

    let mut config = FormatConfig::default();
    let mut warnings = Vec::new();
    warn_unknown_fields(root, &["format"], "", &display_path, &mut warnings);
    let Some(format_value) = root.get("format") else {
        return (config, warnings);
    };
    let Some(format) = format_value.as_object() else {
        warnings.push(format!(
            "{display_path}: 'format' must be an object; using formatter defaults"
        ));
        return (config, warnings);
    };

    warn_unknown_fields(
        format,
        &[
            "enabled",
            "indent_type",
            "indent_count",
            "line_width",
            "brace_style",
            "where_position",
            "blank_lines_between_declarations",
            "blank_lines_between_members",
            "blank_lines_before_functions",
            "trailing_comma",
        ],
        "format.",
        &display_path,
        &mut warnings,
    );

    if let Some(value) = format.get("enabled") {
        if let Some(enabled) = value.as_bool() {
            config.enabled = enabled;
        } else {
            invalid(&mut warnings, &display_path, "format.enabled", "a boolean");
        }
    }

    let options = &mut config.options;

    if let Some(value) = format.get("indent_type") {
        match value.as_str() {
            Some("space") => options.indent_type = IndentType::Space,
            Some("tab") => options.indent_type = IndentType::Tab,
            _ => invalid(
                &mut warnings,
                &display_path,
                "format.indent_type",
                "'space' or 'tab'",
            ),
        }
    }
    options.indent_count = match options.indent_type {
        IndentType::Space => 4,
        IndentType::Tab => 1,
    };
    if let Some(value) = format.get("indent_count") {
        if let Some(count) = positive_count(value) {
            options.indent_count = count;
        } else {
            invalid(
                &mut warnings,
                &display_path,
                "format.indent_count",
                "a positive integer",
            );
        }
    }
    if let Some(value) = format.get("line_width") {
        if let Some(width) = positive_count(value) {
            options.line_width = width;
        } else {
            invalid(
                &mut warnings,
                &display_path,
                "format.line_width",
                "a positive integer",
            );
        }
    }
    if let Some(value) = format.get("brace_style") {
        match value.as_str() {
            Some("same_line") => options.brace_style = BraceStyle::SameLine,
            Some("next_line") => options.brace_style = BraceStyle::NextLine,
            _ => invalid(
                &mut warnings,
                &display_path,
                "format.brace_style",
                "'same_line' or 'next_line'",
            ),
        }
    }
    if let Some(value) = format.get("where_position") {
        match value.as_str() {
            Some("own_line") => options.where_position = WherePosition::OwnLine,
            Some("same_line") => options.where_position = WherePosition::SameLine,
            _ => invalid(
                &mut warnings,
                &display_path,
                "format.where_position",
                "'own_line' or 'same_line'",
            ),
        }
    }
    set_blank_lines(
        format,
        "blank_lines_between_declarations",
        &display_path,
        &mut warnings,
        &mut options.blank_lines_between_declarations,
    );
    set_blank_lines(
        format,
        "blank_lines_between_members",
        &display_path,
        &mut warnings,
        &mut options.blank_lines_between_members,
    );
    set_blank_lines(
        format,
        "blank_lines_before_functions",
        &display_path,
        &mut warnings,
        &mut options.blank_lines_before_functions,
    );
    if let Some(value) = format.get("trailing_comma") {
        match value.as_str() {
            Some("multiline") => options.trailing_comma = TrailingComma::Multiline,
            Some("never") => options.trailing_comma = TrailingComma::Never,
            Some("always") => options.trailing_comma = TrailingComma::Always,
            _ => invalid(
                &mut warnings,
                &display_path,
                "format.trailing_comma",
                "'multiline', 'never', or 'always'",
            ),
        }
    }
    (config, warnings)
}

fn set_blank_lines(
    format: &Map<String, Value>,
    key: &str,
    path: &str,
    warnings: &mut Vec<String>,
    target: &mut usize,
) {
    if let Some(value) = format.get(key) {
        if let Some(count) = value.as_u64().and_then(|count| usize::try_from(count).ok()) {
            *target = count;
        } else {
            invalid(
                warnings,
                path,
                &format!("format.{key}"),
                "a nonnegative integer",
            );
        }
    }
}

fn positive_count(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .filter(|value| *value > 0)
        .and_then(|value| usize::try_from(value).ok())
}

fn invalid(warnings: &mut Vec<String>, path: &str, key: &str, expected: &str) {
    warnings.push(format!(
        "{path}: invalid {key}; expected {expected}, using its default"
    ));
}

fn warn_unknown_fields(
    object: &Map<String, Value>,
    known: &[&str],
    prefix: &str,
    path: &str,
    warnings: &mut Vec<String>,
) {
    for key in object.keys().filter(|key| !known.contains(&key.as_str())) {
        warnings.push(format!(
            "{path}: unknown config field '{prefix}{key}', ignoring it"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("mux-format-config-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn reads_partial_format_options_and_warns_for_unknown_fields() {
        let root = temp_dir();
        fs::write(
            root.join(CONFIG_NAME),
            r#"{"format":{"indent_type":"tab","line_width":100,"extra":true},"other":1}"#,
        )
        .unwrap();

        let (config, warnings) = load_from_directory(&root);

        assert_eq!(config.options.indent_type, IndentType::Tab);
        assert_eq!(config.options.indent_count, 1);
        assert_eq!(config.options.line_width, 100);
        assert_eq!(config.options.blank_lines_between_declarations, 1);
        assert!(config.enabled);
        assert_eq!(warnings.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn formatting_can_be_disabled_in_project_config() {
        let root = temp_dir();
        fs::write(root.join(CONFIG_NAME), r#"{"format":{"enabled":false}}"#).unwrap();

        let (config, warnings) = load_from_directory(&root);

        assert!(!config.enabled);
        assert!(warnings.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_values_use_their_defaults_while_valid_values_apply() {
        let root = temp_dir();
        fs::write(
            root.join(CONFIG_NAME),
            r#"{"format":{"indent_type":"tab","indent_count":0,"line_width":100,"blank_lines_between_members":-1}}"#,
        )
        .unwrap();

        let (config, warnings) = load_from_directory(&root);

        assert_eq!(config.options.indent_count, 1);
        assert_eq!(config.options.line_width, 100);
        assert_eq!(config.options.blank_lines_between_members, 0);
        assert_eq!(warnings.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_json_uses_all_defaults() {
        let root = temp_dir();
        fs::write(root.join(CONFIG_NAME), "{").unwrap();

        let (config, warnings) = load_from_directory(&root);

        assert_eq!(config, FormatConfig::default());
        assert_eq!(warnings.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nearest_config_wins_before_worktree_root() {
        let root = temp_dir();
        let nested = root.join("src");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(CONFIG_NAME), r#"{"format":{"line_width":100}}"#).unwrap();
        fs::write(nested.join(CONFIG_NAME), r#"{"format":{"line_width":120}}"#).unwrap();

        let (config, _) = load_from_directory(&nested);

        assert_eq!(config.options.line_width, 120);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_does_not_cross_the_worktree_root() {
        let parent = temp_dir();
        fs::write(parent.join(CONFIG_NAME), r#"{"format":{"line_width":120}}"#).unwrap();
        let worktree = parent.join("worktree");
        fs::create_dir_all(worktree.join(".git")).unwrap();

        let (config, warnings) = load_from_directory(&worktree);

        assert_eq!(config.options.line_width, 80);
        assert!(warnings.is_empty());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn unusable_nearest_config_does_not_fall_back_to_parent() {
        let parent = temp_dir();
        fs::write(parent.join(CONFIG_NAME), r#"{"format":{"line_width":120}}"#).unwrap();
        let worktree = parent.join("worktree");
        fs::create_dir_all(worktree.join(".git")).unwrap();
        fs::create_dir(worktree.join(CONFIG_NAME)).unwrap();

        let (config, warnings) = load_from_directory(&worktree);

        assert_eq!(config.options.line_width, 80);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("could not read formatter config"));
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_nearest_config_is_reported_instead_of_skipped() {
        use std::os::unix::fs::symlink;

        let parent = temp_dir();
        fs::write(parent.join(CONFIG_NAME), r#"{"format":{"line_width":120}}"#).unwrap();
        let worktree = parent.join("worktree");
        fs::create_dir_all(worktree.join(".git")).unwrap();
        symlink("missing.json", worktree.join(CONFIG_NAME)).unwrap();

        let (config, warnings) = load_from_directory(&worktree);

        assert_eq!(config.options.line_width, 80);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("could not read formatter config"));
        fs::remove_dir_all(parent).unwrap();
    }
}
