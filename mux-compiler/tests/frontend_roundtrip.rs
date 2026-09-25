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

fn contains_syntax_data(
    node: &syntax::SyntaxNode,
    predicate: fn(&syntax::SyntaxData) -> bool,
) -> bool {
    node.data().is_some_and(predicate)
        || node.children().iter().any(|child| match child {
            syntax::SyntaxElement::Node(child) => contains_syntax_data(child, predicate),
            syntax::SyntaxElement::Token(_) => false,
        })
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
fn syntax_lowering_preserves_unary_binary_and_postfix_validation() {
    let source = "func valid(int seed = 1) returns void {\n    auto value = -seed + 2\n    value++\n    return\n}\n";
    let parsed = syntax::parse_source(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    parsed
        .lower()
        .expect("unary and binary expressions should lower");

    let negative_default =
        syntax::parse_source("func invalid(int seed = -other) returns void {\n    return\n}\n");
    assert!(
        negative_default.has_errors(),
        "a unary expression must not be accepted as a literal default"
    );

    let nested_postfix = syntax::parse_source(
        "func invalid() returns void {\n    auto value = 0\n    value + value++\n}\n",
    );
    assert!(
        nested_postfix.has_errors(),
        "postfix updates inside binary expressions must remain rejected"
    );

    for source in [
        "func invalid() returns int {\n    auto value = 0\n    return value++\n}\n",
        "func invalid() returns void {\n    auto value = 0\n    if value++ {\n        return\n    }\n}\n",
    ] {
        assert!(
            syntax::parse_source(source).has_errors(),
            "postfix updates in return values and conditions must remain rejected"
        );
    }

    let comparison = syntax::parse_source("auto result = (-value)<int>(value)\n");
    assert!(comparison.errors.is_empty(), "{:?}", comparison.errors);
    assert!(contains_syntax_data(
        comparison.tree.root(),
        |data| matches!(data, syntax::SyntaxData::Binary { .. })
    ));
    assert!(!contains_syntax_data(
        comparison.tree.root(),
        |data| matches!(data, syntax::SyntaxData::Generic { .. })
    ));
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
