use insta::assert_debug_snapshot;
use mux_lang::syntax;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn parse_file_to_ast(test_file: &Path) -> String {
    let path_str = test_file.to_string_lossy();
    let source_text = fs::read_to_string(test_file)
        .unwrap_or_else(|error| panic!("Failed to open source file {path_str}: {error}"));
    let parsed = syntax::parse_source(&source_text);
    if !parsed.errors.is_empty() {
        panic!(
            "Parsing failed with errors on file {path_str}: {:#?}",
            parsed.errors
        );
    }
    let result = parsed
        .lower()
        .unwrap_or_else(|error| panic!("Lowering failed on file {path_str}: {error}"));

    // Snapshots assert AST structure, while range validity is checked
    // independently so location representation changes do not rewrite them.
    let mut output = String::new();
    for node in result {
        let debug = format!("{node:#?}");
        let _ = writeln!(&mut output, "{}", without_span_debug(&debug));
        output.push('\n');
    }
    output
}

fn ast_without_spans(nodes: &[mux_lang::ast::AstNode]) -> String {
    without_span_debug(&format!("{nodes:#?}"))
}

fn without_span_debug(debug: &str) -> String {
    let mut normalized = String::with_capacity(debug.len());
    let mut remainder = debug;
    while let Some(start) = remainder.find("Span {") {
        normalized.push_str(&remainder[..start]);
        normalized.push_str("Span");
        let span_end = remainder[start..]
            .find('}')
            .expect("debug span has a closing brace");
        remainder = &remainder[start + span_end + 1..];
    }
    normalized.push_str(remainder);
    normalized
}

#[test]
fn parser_snapshot_inventory_matches_fixtures() {
    let fixture_dir = std::env::var_os("MUX_TEST_SCRIPTS_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test_scripts"),
        PathBuf::from,
    );
    let snapshot_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");

    let fixture_stems: Vec<_> = fs::read_dir(&fixture_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", fixture_dir.display()))
        .map(|entry| entry.expect("failed to read fixture entry").path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("mux"))
        .filter_map(|path| path.file_stem().map(std::ffi::OsStr::to_os_string))
        .collect();
    let fixtures: std::collections::BTreeSet<_> = fixture_stems.iter().cloned().collect();
    assert_eq!(
        fixture_stems.len(),
        fixtures.len(),
        "root fixtures must have unique stems so each fixture maps to one snapshot"
    );
    let snapshots: std::collections::BTreeSet<_> = fs::read_dir(&snapshot_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", snapshot_dir.display()))
        .map(|entry| entry.expect("failed to read snapshot entry").path())
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let stem = name
                .strip_prefix("parser_integration__")?
                .strip_suffix(".snap")?;
            if stem.starts_with("legacy_ast_") {
                return None;
            }
            Some(stem.to_owned().into())
        })
        .collect();

    assert_eq!(
        fixtures, snapshots,
        "every root fixture must have exactly one parser snapshot; update the fixture and snapshot together"
    );
}

#[test]
fn test_parse_all_mux_files_in_dir() {
    let test_dir = "../test_scripts";
    let dir_path = PathBuf::from(&test_dir);

    if !dir_path.exists() {
        panic!(
            "Test scripts directory not found: {} (set MUX_TEST_SCRIPTS_DIR to override)",
            dir_path.display()
        );
    }

    println!("Scanning directory: {}", dir_path.display());

    let entries = fs::read_dir(&dir_path).unwrap_or_else(|e| {
        panic!(
            "Failed to read test directory {}: {}",
            dir_path.display(),
            e
        )
    });

    let mut test_files = Vec::new();
    for entry in entries {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("mux") {
            test_files.push(path);
        }
    }

    // Sort files for consistent test order
    test_files.sort();

    for path in test_files {
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        println!("\n=== Testing file: {file_name} ===");

        match std::panic::catch_unwind(|| {
            println!("Parsing file: {}", path.display());
            let ast_string = parse_file_to_ast(&path);
            let snapshot_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown_file");
            println!("Creating snapshot for: {snapshot_name}");
            assert_debug_snapshot!(snapshot_name, ast_string);
            println!("✓ Successfully processed: {file_name}");
        }) {
            Ok(()) => {}
            Err(e) => {
                println!("❌ Error processing file {file_name}: {e:?}");
                panic!("Test failed while processing: {file_name}");
            }
        }
    }
}

#[test]
fn syntax_frontend_lowering_matches_legacy_ast_baselines() {
    let fixture_dir = std::env::var_os("MUX_TEST_SCRIPTS_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test_scripts"),
        PathBuf::from,
    );
    let mut files: Vec<_> = fs::read_dir(&fixture_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", fixture_dir.display()))
        .map(|entry| entry.expect("failed to read fixture entry").path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("mux"))
        .collect();
    files.sort();

    for path in files {
        let source_text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let fixture_name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("fixture path has a UTF-8 file stem");

        let parsed = syntax::parse_source(&source_text);
        assert!(
            parsed.errors.is_empty(),
            "syntax frontend failed on {}: {:#?}",
            path.display(),
            parsed.errors
        );
        let lowered = parsed.lower().unwrap_or_else(|error| {
            panic!(
                "syntax frontend could not lower {}: {error}",
                path.display()
            )
        });
        insta::with_settings!({ omit_expression => true }, {
            insta::assert_snapshot!(
                format!("legacy_ast_{fixture_name}"),
                ast_without_spans(&lowered)
            );
        });
    }
}
