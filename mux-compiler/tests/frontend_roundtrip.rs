use mux_lang::{formatter, syntax};
use std::{fs, path::PathBuf};

fn fixtures() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test_scripts");
    let mut paths: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "mux"))
        .collect();
    paths.sort();
    paths
}

fn reconstruct(node: &syntax::SyntaxNode, tree: &syntax::SyntaxTree, output: &mut String) {
    for child in node.children() {
        match child {
            syntax::SyntaxElement::Node(child) => reconstruct(child, tree, output),
            syntax::SyntaxElement::Token(index) => {
                output.push_str(tree.token_text(*index).unwrap())
            }
        }
    }
}

fn validate_tree(node: &syntax::SyntaxNode, tree: &syntax::SyntaxTree, indices: &mut Vec<usize>) {
    for child in node.children() {
        let range = match child {
            syntax::SyntaxElement::Node(child) => {
                validate_tree(child, tree, indices);
                child.range()
            }
            syntax::SyntaxElement::Token(index) => {
                indices.push(*index);
                tree.tokens()[*index].range()
            }
        };
        assert!(
            range.start >= node.range().start && range.end <= node.range().end,
            "child {range:?} escapes {:?} {:?}",
            node.kind(),
            node.range()
        );
    }
}

#[test]
fn corpus_syntax_trees_reconstruct_the_original_source() {
    for path in fixtures() {
        let source = fs::read_to_string(&path).unwrap();
        let parsed = syntax::parse_source(&source);
        assert!(
            !parsed.has_errors(),
            "{}: {:?}",
            path.display(),
            parsed.errors
        );
        let mut reconstructed = String::new();
        reconstruct(parsed.tree.root(), &parsed.tree, &mut reconstructed);
        assert_eq!(source, reconstructed, "{}", path.display());
        let mut indices = Vec::new();
        validate_tree(parsed.tree.root(), &parsed.tree, &mut indices);
        assert_eq!(
            indices,
            (0..parsed.tree.tokens().len()).collect::<Vec<_>>(),
            "{}: token leaves must occur exactly once in source order",
            path.display()
        );
    }
}

#[test]
fn corpus_syntax_lowering_succeeds_for_every_fixture() {
    for path in fixtures() {
        let source = fs::read_to_string(&path).unwrap();
        let parsed = syntax::parse_source(&source);
        assert!(
            !parsed.has_errors(),
            "{}: {:?}",
            path.display(),
            parsed.errors
        );
        parsed
            .lower()
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
}

#[test]
fn corpus_formatting_preserves_ast_and_is_idempotent() {
    let spans = regex::Regex::new(r"Span \{[^}]*\}").unwrap();
    for path in fixtures() {
        let source = fs::read_to_string(&path).unwrap();
        let original = syntax::parse_source(&source);
        assert!(
            !original.has_errors(),
            "{}: {:?}",
            path.display(),
            original.errors
        );
        let original_ast = original
            .lower()
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let formatted = formatter::format_source(&source)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let reparsed = syntax::parse_source(&formatted);
        assert!(
            !reparsed.has_errors(),
            "{}: {:?}\n{formatted}",
            path.display(),
            reparsed.errors
        );
        let reparsed_ast = reparsed
            .lower()
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(
            spans.replace_all(&format!("{original_ast:?}"), "SPAN"),
            spans.replace_all(&format!("{reparsed_ast:?}"), "SPAN"),
            "{}: formatting changed program structure",
            path.display()
        );
        assert_eq!(
            formatted,
            formatter::format_source(&formatted).unwrap(),
            "{}: second format changed output",
            path.display()
        );
    }
}

#[test]
fn malformed_source_is_preserved_in_the_syntax_tree() {
    for source in [
        "",
        "\0",
        "func (",
        "/* unfinished",
        "\"unfinished",
        "\u{e9}\u{301}\r\nfunc ??? {",
        "auto =\n}\n",
    ] {
        let parsed = syntax::parse_source(source);
        let mut reconstructed = String::new();
        reconstruct(parsed.tree.root(), &parsed.tree, &mut reconstructed);
        assert_eq!(source, reconstructed);
    }
}
