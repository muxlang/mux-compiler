use mux_lang::diagnostic::Level;
use mux_lang::lexer::{Lexer, TokenType};
use mux_lang::parser::Parser;
use mux_lang::semantics::SemanticAnalyzer;
use mux_lang::source::Source;

fn errors(text: &str) -> Vec<String> {
    let mut source = Source::from_test_str(text);
    let mut lexer = Lexer::new(&mut source);
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token().expect("valid source tokens");
        if token.token_type == TokenType::Eof {
            break;
        }
        tokens.push(token);
    }
    let ast = Parser::new(&tokens).parse().expect("valid source grammar");
    SemanticAnalyzer::new()
        .analyze(&ast, None)
        .into_iter()
        .filter(|diagnostic| diagnostic.code.level() == Level::Error)
        .map(|diagnostic| diagnostic.message.to_string())
        .collect()
}

#[test]
fn propagation_allows_different_success_types_and_expression_positions() {
    let diagnostics = errors(
        r#"
func input() returns result<int, string> { return ok(2) }
func add(int a, int b) returns int { return a + b }
func number(optional<int> input) returns optional<string> {
    return some((use input).to_string())
}
func calculate() returns result<string, string> {
    auto total = add(use input(), use input())
    total = total + use input()
    auto values = [use input(), total]
    if false && (use input()) > 0 { return err("unexpected") }
    return ok(values.size().to_string())
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn propagation_rejects_incompatible_return_contexts() {
    for source in [
        "func f() returns void { use some(1) return }",
        "func f() returns optional<int> { auto x = use ok(1) return some(x) }",
        "func f() returns result<int, string> { auto x = use some(1) return ok(x) }",
        "func f() returns int { return use some(1) }",
        "func f() returns optional<int> { return some(use 1) }",
        "auto x = use some(1)",
    ] {
        let diagnostics = errors(source);
        assert!(
            diagnostics.iter().any(|message| message.contains("'use'")),
            "{source}: {diagnostics:?}"
        );
    }
}

#[test]
fn propagation_uses_the_lambda_return_context() {
    let diagnostics = errors(
        r#"
func f() returns result<int, string> {
    auto closure = func() returns optional<int> {
        auto n = use some(4)
        return some(n)
    }
    auto n = use "5".to_int()
    return ok(n)
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let diagnostics = errors(
        r#"
func f() returns result<int, string> {
    auto closure = func() returns void {
        use "5".to_int()
        return
    }
    return ok(1)
}
"#,
    );
    assert!(diagnostics.iter().any(|message| message.contains("'use'")));
}

#[test]
fn propagation_allows_void_result_operations() {
    let diagnostics = errors(
        r#"
import std.crypto
func save(bytes key) returns result<void, CryptoError> {
    use crypto.seal_file(key, "input.bin", "sealed.bin", b"context")
    return crypto.seal_file(key, "input.bin", "sealed.bin", b"context")
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn narrowing_tracks_direct_saved_and_composed_checks() {
    let diagnostics = errors(
        r#"
func direct(result<int, string> input) returns int {
    if input.is_ok() {
        return input.value()
    }
    return 0
}
func saved(result<int, string> input) returns int {
    auto failed = input.is_err()
    if failed {
        return 0
    }
    return input.value()
}
func composed(result<int, string> input) returns int {
    if !input.is_err() && input.is_ok() {
        return input.value()
    }
    return 0
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn narrowing_treats_loop_exits_as_terminating_paths() {
    let diagnostics = errors(
        r#"
func first_or_zero(optional<int> input) returns int {
    while true {
        if input.is_none() {
            break
        }
        return input.value()
    }
    return 0
}
func continue_with_present(result<int, string> input) returns int {
    while true {
        if input.is_err() {
            continue
        }
        return input.value()
    }
    return 0
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn narrowing_rejects_unproven_or_stale_extraction() {
    for source in [
        r#"
func f(result<int, string> input) returns int {
    if input.is_err() {
        return 0
    }
    input = err("changed")
    return input.value()
}
"#,
        r#"
func f(result<int, string> input) returns int {
    auto failed = input.is_err()
    input = err("changed")
    if failed {
        return 0
    }
    return input.value()
}
"#,
        r#"
func f(result<int, string> input) returns int {
    return input.value()
}
"#,
    ] {
        let diagnostics = errors(source);
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("variant")),
            "{source}: {diagnostics:?}"
        );
    }
}

#[test]
fn sql_json_constructor_and_accessor_are_typed() {
    let diagnostics = errors(
        r#"
import std.data.json
import std.sql
func f() returns result<SqlValue, SqlError> {
    auto parsed = json.parse("{\"name\":\"Mux\"}")
    if parsed.is_err() {
        return err(SqlError.from_message(parsed.error().message()))
    }
    auto document = parsed.value()
    auto parameter = use sql.json(document)
    auto decoded = use parameter.as_json()
    return ok(parameter)
}
"#,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
