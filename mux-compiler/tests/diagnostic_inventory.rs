//! Guard the typed-diagnostic boundary against message-only regressions.

use std::fs;
use std::path::{Path, PathBuf};

const CONSTRUCTORS: &[&str] = &[
    "LexerError::new(",
    "LexerError::with_help(",
    "ParserError::new(",
    "ParserError::with_help(",
    "ParserError::from_token(",
    "SemanticError::new(",
    "SemanticError::with_help(",
];

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source directory") {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

fn matching_parenthesis(source: &str, open: usize) -> usize {
    let mut depth = 0;
    let mut quote = None;
    let mut escaped = false;
    for (offset, byte) in source.as_bytes()[open..].iter().enumerate() {
        let ch = *byte as char;
        if update_quote_state(ch, &mut quote, &mut escaped) {
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                return open + offset;
            }
        }
    }
    panic!("unclosed constructor at byte {open}");
}

fn update_quote_state(ch: char, quote: &mut Option<char>, escaped: &mut bool) -> bool {
    let Some(active_quote) = *quote else {
        return false;
    };
    if *escaped {
        *escaped = false;
    } else if ch == '\\' {
        *escaped = true;
    } else if ch == active_quote {
        *quote = None;
    }
    true
}

#[test]
fn every_frontend_constructor_selects_a_typed_code() {
    for file in frontend_rust_files() {
        assert_file_constructors_are_typed(&file);
    }
}

fn frontend_rust_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    rust_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files
}

fn assert_file_constructors_are_typed(file: &Path) {
    let source = fs::read_to_string(file).expect("read Rust source");
    for constructor in CONSTRUCTORS {
        assert_constructor_calls_are_typed(file, &source, constructor);
    }
}

fn assert_constructor_calls_are_typed(file: &Path, source: &str, constructor: &str) {
    let mut cursor = 0;
    while let Some(relative) = source[cursor..].find(constructor) {
        let start = cursor + relative;
        let open = start + constructor.len() - 1;
        let end = matching_parenthesis(source, open);
        let call = &source[start..=end];
        assert!(
            call.contains("DiagnosticCode::") || call.contains(".code"),
            "{} at byte {} has no typed diagnostic code",
            file.display(),
            start
        );
        cursor = end + 1;
    }
}

#[test]
fn every_frontend_structured_error_literal_selects_a_typed_code() {
    let mut files = Vec::new();
    rust_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );

    for file in files {
        let source = fs::read_to_string(&file).expect("read Rust source");
        let lines = source.lines().collect::<Vec<_>>();
        for (line_number, line) in lines.iter().enumerate() {
            if !line.contains("SemanticError {")
                || line.contains("struct SemanticError")
                || line.contains("impl ")
                || line.contains("-> SemanticError")
            {
                continue;
            }
            let end = (line_number + 12).min(lines.len());
            let literal = lines[line_number..end].join("\n");
            assert!(
                literal.contains("code: DiagnosticCode::"),
                "{}:{} has no typed diagnostic code",
                file.display(),
                line_number + 1
            );
        }
    }
}
