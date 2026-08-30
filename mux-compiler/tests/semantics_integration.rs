use mux_lang::lexer::Lexer;
use mux_lang::parser::Parser;
use mux_lang::semantics::SemanticAnalyzer;
use mux_lang::source::Source;
use mux_lang::{diagnostic::Files, diagnostic::Level, module_resolver::ModuleResolver};
use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

fn analyze_mux_file(path: &std::path::PathBuf) -> Result<(), String> {
    println!("=== Testing file: {} ===", path.display());

    let content =
        fs::read_to_string(path).map_err(|error| format!("could not read fixture: {error}"))?;
    let mut source = Source::from_test_str(&content);

    let mut lexer = Lexer::new(&mut source);
    let tokens: Vec<_> = std::iter::from_fn(|| match lexer.next_token() {
        Ok(token) if token.token_type == mux_lang::lexer::TokenType::Eof => None,
        Ok(token) => Some(Ok(token)),
        Err(e) => Some(Err(e)),
    })
    .collect::<Result<_, _>>()
    .map_err(|error| format!("lexer error: {error}"))?;

    let mut parser = Parser::new(&tokens);
    let ast = parser
        .parse()
        .map_err(|error| format!("parser error: {error:?}"))?;

    let mut files = Files::new();
    let file_id = files.add(path, content);
    let base_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let resolver = Rc::new(RefCell::new(ModuleResolver::new(base_path)));
    let mut analyzer = SemanticAnalyzer::new_with_resolver(resolver);
    analyzer.set_current_file(path.clone());
    analyzer.set_current_file_id(file_id);
    let diagnostics = analyzer.analyze(&ast, Some(&mut files));
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code.level() == Level::Error)
        .collect();

    if errors.is_empty() {
        println!("✓ Successfully analyzed: {}", path.display());
        Ok(())
    } else {
        println!("✗ Semantic errors in {}:", path.display());
        for error in &errors {
            println!(
                "  {} at {}:{}",
                error.message, error.span.row_start, error.span.col_start
            );
        }
        let details = errors
            .iter()
            .map(|error| {
                format!(
                    "{} at {}:{}",
                    error.message, error.span.row_start, error.span.col_start
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        Err(details)
    }
}

fn collect_mux_files_in_dir(
    test_dir: &Path,
    failures: &mut Vec<(std::path::PathBuf, String)>,
) -> usize {
    let mut count = 0;
    let mut paths = fs::read_dir(test_dir)
        .unwrap_or_else(|error| panic!("Failed to read {}: {error}", test_dir.display()))
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("Failed to read fixture entry: {error}"));
    paths.sort();

    for path in paths {
        if path.extension().and_then(std::ffi::OsStr::to_str) == Some("mux") {
            if let Err(error) = analyze_mux_file(&path) {
                failures.push((path, error));
            }
            count += 1;
        }
    }
    count
}

#[test]
fn test_semantic_analysis() {
    let test_dir = Path::new("../test_scripts");

    if !test_dir.exists() {
        panic!("Test scripts directory not found: {test_dir:?}");
    }

    let mut files_processed = 0;
    let mut failures = Vec::new();

    let operator_test_path = Path::new("tests/operator_overloading.mux");
    if operator_test_path.exists() {
        let path_buf = operator_test_path.to_path_buf();
        if let Err(error) = analyze_mux_file(&path_buf) {
            failures.push((path_buf, error));
        }
        files_processed += 1;
    }

    files_processed += collect_mux_files_in_dir(test_dir, &mut failures);

    assert!(
        files_processed > 0,
        "No .mux files found in test directories"
    );
    assert!(
        failures.is_empty(),
        "Semantic analysis failed for: {}",
        failures
            .iter()
            .map(|(path, error)| format!("{} ({error})", path.display()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("Processed {files_processed} files");
}

#[test]
fn semantic_fixture_failures_are_reported() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!(
        "mux_semantics_fixture_{}_{}",
        std::process::id(),
        nonce
    ));
    fs::create_dir(&temp_dir).expect("temporary fixture directory should be created");
    let invalid_fixture = temp_dir.join("invalid.mux");
    fs::write(
        &invalid_fixture,
        "func main() returns void {\n    print(missing_name)\n    return\n}\n",
    )
    .expect("temporary fixture should be writable");

    let mut failures = Vec::new();
    let files_processed = collect_mux_files_in_dir(&temp_dir, &mut failures);
    fs::remove_dir_all(temp_dir).expect("temporary fixture directory should be removable");

    assert_eq!(files_processed, 1);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, invalid_fixture);
    assert!(failures[0].1.contains("missing_name"));
}
