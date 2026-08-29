use insta::assert_debug_snapshot;
use mux_lang::lexer::Lexer;
use mux_lang::source::Source;
use std::fs;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    std::env::var_os("MUX_TEST_SCRIPTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test_scripts"))
}

fn root_mux_files() -> Vec<PathBuf> {
    let dir_path = fixture_dir();
    if !dir_path.exists() {
        panic!(
            "Test scripts directory not found: {} (set MUX_TEST_SCRIPTS_DIR to override)",
            dir_path.display()
        );
    }

    let mut test_files: Vec<_> = fs::read_dir(&dir_path)
        .unwrap_or_else(|error| {
            panic!(
                "Failed to read test directory {}: {error}",
                dir_path.display()
            )
        })
        .map(|entry| entry.expect("Failed to read directory entry").path())
        .filter(|path| path.extension().and_then(std::ffi::OsStr::to_str) == Some("mux"))
        .collect();
    test_files.sort();
    test_files
}

#[test]
fn lexer_snapshot_inventory_matches_fixtures() {
    let fixture_stems: Vec<_> = root_mux_files()
        .into_iter()
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect();
    let unique_fixture_stems: std::collections::BTreeSet<_> =
        fixture_stems.iter().cloned().collect();
    assert_eq!(
        fixture_stems.len(),
        unique_fixture_stems.len(),
        "root fixtures must have unique stems so each fixture maps to one snapshot"
    );
    let snapshot_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let snapshot_stems: std::collections::BTreeSet<_> = fs::read_dir(&snapshot_dir)
        .unwrap_or_else(|error| panic!("Failed to read snapshot directory: {error}"))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter_map(|name| {
            name.strip_prefix("lexer_integration__file_lexer__")
                .and_then(|name| name.strip_suffix(".snap"))
                .map(str::to_owned)
        })
        .collect();

    assert_eq!(
        unique_fixture_stems, snapshot_stems,
        "every root fixture must have exactly one named lexer snapshot; update the fixture and snapshot together"
    );
}

#[test]
fn test_file_lexer() {
    for path in root_mux_files() {
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        println!("\n=== Testing file: {} ===", file_name);

        let mut src = Source::new(&path.to_string_lossy())
            .unwrap_or_else(|_| panic!("Failed to open source file: {}", path.display()));

        let mut lexer = Lexer::new(&mut src);
        let tokens = lexer
            .lex_all()
            .unwrap_or_else(|e| panic!("Lexing failed for file {}: {}", file_name, e));

        let snapshot_name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown_file");
        assert_debug_snapshot!(format!("file_lexer__{snapshot_name}"), tokens);
        println!("✓ Successfully processed: {}", file_name);
    }
}
