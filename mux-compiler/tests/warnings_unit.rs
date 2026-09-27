use mux_lang::diagnostic::DiagnosticCode;
use mux_lang::semantics::SemanticAnalyzer;
use mux_lang::syntax::parse_source;

fn analyze(source_text: &str) -> Vec<(DiagnosticCode, String)> {
    let ast = parse_source(source_text)
        .lower()
        .expect("source should parse");
    let mut analyzer = SemanticAnalyzer::new();
    analyzer
        .analyze(&ast, None)
        .into_iter()
        .map(|error| (error.code, error.message.to_string()))
        .collect()
}

#[test]
fn reports_unused_bindings_and_parameters() {
    let diagnostics = analyze(
        r"
func main(int unused_parameter) returns void {
    auto unused_local = 1
    auto used = 2
    print(used.to_string())
    return
}
",
    );

    assert_eq!(
        diagnostics
            .iter()
            .filter(|(code, _)| *code == DiagnosticCode::UnusedBinding)
            .count(),
        2
    );
}

#[test]
fn reports_shadowing_and_dead_assignment() {
    let diagnostics = analyze(
        r"
func main() returns void {
    auto value = 1
    value = 2
    value = 3
    {
        auto value = 3
        print(value.to_string())
    }
    print(value.to_string())
    return
}
",
    );

    assert!(
        diagnostics
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::ShadowedBinding)
    );
    assert!(
        diagnostics
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::DeadAssignment)
    );
}

#[test]
fn bare_underscore_is_not_reported() {
    let diagnostics = analyze(
        r"
func main() returns void {
    auto _ = 1
    return
}
",
    );
    assert!(
        !diagnostics
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::UnusedBinding)
    );
}
