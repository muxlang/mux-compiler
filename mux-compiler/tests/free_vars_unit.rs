//! Tests for free-variable (closure capture) analysis in
//! `mux_lang::semantics::free_vars`.
//!
//! That module is crate-internal (`pub(super)`), so it can only be exercised by
//! running the full lex/parse/analyze pipeline over Mux source containing
//! lambdas. Each program below captures outer variables from inside a variety of
//! expression and statement positions, driving the `handle_*` walk over a lambda
//! body. A clean analysis (no semantic errors) confirms the captures type-check.

use mux_lang::lexer::Lexer;
use mux_lang::parser::Parser;
use mux_lang::semantics::{SemanticAnalyzer, SemanticError};
use mux_lang::source::Source;

fn analyze(src: &str) -> Vec<SemanticError> {
    let mut source = Source::from_test_str(src);
    let mut lexer = Lexer::new(&mut source);
    let tokens: Vec<_> = std::iter::from_fn(|| match lexer.next_token() {
        Ok(token) if token.token_type == mux_lang::lexer::TokenType::Eof => None,
        Ok(token) => Some(Ok(token)),
        Err(e) => Some(Err(e)),
    })
    .collect::<Result<_, _>>()
    .expect("lexing should succeed");
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse().expect("parsing should succeed");
    let mut analyzer = SemanticAnalyzer::new();
    analyzer.analyze(&ast, None)
}

fn assert_clean(src: &str) {
    let errors = analyze(src);
    assert!(
        errors.is_empty(),
        "expected no semantic errors, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn captures_in_binary_and_call_positions() {
    assert_clean(
        r"
func apply(func(int) returns int f, int x) returns int {
    return f(x)
}

func main() returns void {
    int base = 10
    auto add = func(int y) returns int {
        return base + y
    }
    int r = apply(add, 5)
    print(r.to_string())
    return
}
",
    );
}

#[test]
fn captures_through_nested_lambdas() {
    assert_clean(
        r"
func main() returns void {
    int base = 5
    auto outer = func(int a) returns int {
        auto inner = func(int b) returns int {
            return a + b + base
        }
        return inner(a)
    }
    print(outer(2).to_string())
    return
}
",
    );
}

#[test]
fn captures_in_list_literal_and_index() {
    assert_clean(
        r"
func main() returns void {
    int idx = 1
    int bump = 100
    auto pick = func() returns int {
        auto items = [bump, bump + 1, bump + 2]
        return items[idx]
    }
    print(pick().to_string())
    return
}
",
    );
}

#[test]
fn captures_in_while_and_if_statements() {
    assert_clean(
        r"
func main() returns void {
    int limit = 3
    auto run = func() returns int {
        int total = 0
        int i = 0
        while i < limit {
            total = total + i
            i = i + 1
        }
        if total > limit {
            total = total + limit
        }
        return total
    }
    print(run().to_string())
    return
}
",
    );
}

#[test]
fn captures_in_for_loop_and_unary() {
    assert_clean(
        r"
func main() returns void {
    int limit = 4
    bool flag = true
    auto run = func() returns int {
        int total = 0
        for int i in range(0, limit) {
            if !flag {
                total = total + i
            }
        }
        return total
    }
    print(run().to_string())
    return
}
",
    );
}

#[test]
fn non_capturing_lambda_is_clean() {
    assert_clean(
        r"
func main() returns void {
    auto square = func(int n) returns int {
        return n * n
    }
    print(square(6).to_string())
    return
}
",
    );
}
