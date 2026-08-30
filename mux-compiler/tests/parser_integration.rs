use insta::assert_debug_snapshot;
use mux_lang::lexer::Lexer;
use mux_lang::parser::Parser;
use mux_lang::source::Source;
use std::fs;
use std::path::{Path, PathBuf};

fn parse_file_to_ast(test_file: &Path) -> String {
    let path_str = test_file.to_string_lossy();
    let mut src = Source::new(&path_str)
        .unwrap_or_else(|_| panic!("Failed to open source file: {}", path_str));

    let mut lexer = Lexer::new(&mut src);
    let tokens = lexer
        .lex_all()
        .unwrap_or_else(|e| panic!("Lexing failed: {}", e));

    let mut parser = Parser::new(&tokens);
    let result = parser.parse().unwrap_or_else(|(ast, errors)| {
        panic!(
            "Parsing failed with errors on file {}: {:#?}\n\nAST: {:#?}",
            path_str, errors, ast
        )
    });

    // Convert the AST to a nicely formatted string for snapshots
    let mut output = String::new();
    for node in result {
        output.push_str(&format!("{:#?}\n\n", node));
    }
    output
}

#[test]
fn parser_snapshot_inventory_matches_fixtures() {
    let fixture_dir = std::env::var_os("MUX_TEST_SCRIPTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test_scripts"));
    let snapshot_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");

    let fixture_stems: Vec<_> = fs::read_dir(&fixture_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", fixture_dir.display()))
        .map(|entry| entry.expect("failed to read fixture entry").path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("mux"))
        .filter_map(|path| path.file_stem().map(|stem| stem.to_os_string()))
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
        println!("\n=== Testing file: {} ===", file_name);

        match std::panic::catch_unwind(|| {
            println!("Parsing file: {}", path.display());
            let ast_string = parse_file_to_ast(&path);
            let snapshot_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown_file");
            println!("Creating snapshot for: {}", snapshot_name);
            assert_debug_snapshot!(snapshot_name, ast_string);
            println!("✓ Successfully processed: {}", file_name);
        }) {
            Ok(_) => {}
            Err(e) => {
                println!("❌ Error processing file {}: {:?}", file_name, e);
                panic!("Test failed while processing: {}", file_name);
            }
        }
    }
}
