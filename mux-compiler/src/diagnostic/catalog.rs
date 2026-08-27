//! Stable diagnostic codes and their reference metadata.

use super::Level;
use std::fmt;

/// A stable identifier for a compiler diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DiagnosticCode {
    LexUnexpectedCharacter,
    LexUnterminatedString,
    LexUnknownEscape,
    LexUnterminatedComment,
    LexInvalidNumber,
    LexRangeLiteral,
    LexInvalidCharacterLiteral,
    ParseExpectedToken,
    ParseExpectedExpression,
    ParseExpectedType,
    ParseControlFlowOutsideLoop,
    ParseReturnOutsideFunction,
    ParseErrorLimit,
    UndefinedName,
    DuplicateDeclaration,
    TypeMismatch,
    WrongArgumentCount,
    MissingReturn,
    NonExhaustiveMatch,
    CannotAssign,
    UnknownMember,
    InvalidOperation,
    DivisionByZero,
    NestedFunctionCapture,
    InvalidPattern,
    InvalidTypeArguments,
    UninitializedReadError,
    ModuleNotFound,
    ImportFailure,
    InternalCompiler,
    UnusedBinding,
    ShadowedBinding,
    UnreachableCode,
    DeadAssignment,
    UninitializedRead,
    ConstantCondition,
    RedundantConstruct,
}

/// Documentation for one registered diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticInfo {
    pub code: DiagnosticCode,
    pub level: Level,
    pub title: &'static str,
    pub trigger: &'static str,
    pub example: &'static str,
    pub explanation: &'static str,
    pub fix: &'static str,
}

impl DiagnosticCode {
    /// Every registered code, in stable numeric order.
    pub const fn all() -> &'static [Self] {
        &[
            Self::LexUnexpectedCharacter,
            Self::LexUnterminatedString,
            Self::LexUnknownEscape,
            Self::LexUnterminatedComment,
            Self::LexInvalidNumber,
            Self::LexRangeLiteral,
            Self::LexInvalidCharacterLiteral,
            Self::ParseExpectedToken,
            Self::ParseExpectedExpression,
            Self::ParseExpectedType,
            Self::ParseControlFlowOutsideLoop,
            Self::ParseReturnOutsideFunction,
            Self::ParseErrorLimit,
            Self::UndefinedName,
            Self::DuplicateDeclaration,
            Self::TypeMismatch,
            Self::WrongArgumentCount,
            Self::MissingReturn,
            Self::NonExhaustiveMatch,
            Self::CannotAssign,
            Self::UnknownMember,
            Self::InvalidOperation,
            Self::InvalidPattern,
            Self::InvalidTypeArguments,
            Self::UninitializedReadError,
            Self::DivisionByZero,
            Self::NestedFunctionCapture,
            Self::ModuleNotFound,
            Self::ImportFailure,
            Self::InternalCompiler,
            Self::UnreachableCode,
            Self::ConstantCondition,
            Self::RedundantConstruct,
        ]
    }

    /// Allocated codes whose producers are not yet part of the public
    /// diagnostic contract. Keeping these out of `all` prevents `explain` and
    /// synchronization tests from promising diagnostics the compiler cannot
    /// currently emit.
    pub const fn reserved() -> &'static [Self] {
        &[
            Self::UnusedBinding,
            Self::ShadowedBinding,
            Self::DeadAssignment,
            Self::UninitializedRead,
        ]
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LexUnexpectedCharacter => "E0100",
            Self::LexUnterminatedString => "E0101",
            Self::LexUnknownEscape => "E0102",
            Self::LexUnterminatedComment => "E0103",
            Self::LexInvalidNumber => "E0104",
            Self::LexRangeLiteral => "E0105",
            Self::LexInvalidCharacterLiteral => "E0106",
            Self::ParseExpectedToken => "E0200",
            Self::ParseExpectedExpression => "E0201",
            Self::ParseExpectedType => "E0202",
            Self::ParseControlFlowOutsideLoop => "E0203",
            Self::ParseReturnOutsideFunction => "E0204",
            Self::ParseErrorLimit => "E0205",
            Self::UndefinedName => "E0300",
            Self::DuplicateDeclaration => "E0301",
            Self::TypeMismatch => "E0302",
            Self::WrongArgumentCount => "E0303",
            Self::MissingReturn => "E0304",
            Self::NonExhaustiveMatch => "E0305",
            Self::CannotAssign => "E0306",
            Self::UnknownMember => "E0307",
            Self::InvalidOperation => "E0308",
            Self::DivisionByZero => "E0312",
            Self::NestedFunctionCapture => "E0313",
            Self::InvalidPattern => "E0309",
            Self::InvalidTypeArguments => "E0310",
            Self::UninitializedReadError => "E0311",
            Self::ModuleNotFound => "E0400",
            Self::ImportFailure => "E0401",
            Self::InternalCompiler => "E0900",
            Self::UnusedBinding => "W0300",
            Self::ShadowedBinding => "W0301",
            Self::UnreachableCode => "W0302",
            Self::DeadAssignment => "W0303",
            Self::UninitializedRead => "W0304",
            Self::ConstantCondition => "W0305",
            Self::RedundantConstruct => "W0306",
        }
    }

    pub const fn level(self) -> Level {
        match self {
            Self::UnusedBinding
            | Self::ShadowedBinding
            | Self::UnreachableCode
            | Self::DeadAssignment
            | Self::UninitializedRead
            | Self::ConstantCondition
            | Self::RedundantConstruct => Level::Warning,
            _ => Level::Error,
        }
    }

    /// Return the user-facing metadata embedded in the compiler.
    pub const fn info(self) -> DiagnosticInfo {
        let (title, trigger, example, explanation, fix) = match self {
            Self::LexUnexpectedCharacter => (
                "unexpected character",
                "The lexer sees a character that is not Mux syntax.",
                "?",
                "Mux source uses a defined syntax alphabet.",
                "Remove it or replace it with valid syntax.",
            ),
            Self::LexUnterminatedString => (
                "unterminated string",
                "A string reaches end of file before its closing quote.",
                "\"hello",
                "A string literal needs a matching closing quote.",
                "Close the string with the matching quote.",
            ),
            Self::LexUnknownEscape => (
                "unknown escape sequence",
                "A string contains an unsupported escape.",
                "\"hi\\z\"",
                "Only documented string escapes are accepted.",
                "Use a supported escape such as \\n or \\t.",
            ),
            Self::LexUnterminatedComment => (
                "unterminated block comment",
                "A block comment reaches end of file without */.",
                "/* note",
                "Block comments must be closed.",
                "Close the comment with */.",
            ),
            Self::LexInvalidNumber => (
                "invalid number literal",
                "A numeric literal contains invalid characters.",
                "12abc",
                "Numeric literals cannot be followed immediately by identifier characters.",
                "Separate the number from the identifier or correct the literal.",
            ),
            Self::LexInvalidCharacterLiteral => (
                "invalid character literal",
                "A character literal is unterminated or contains more than one character.",
                "'ab'",
                "A character literal must contain exactly one character and a closing quote.",
                "Use one character, such as 'a', or close the literal.",
            ),
            Self::LexRangeLiteral => (
                "range literal syntax is not supported",
                "The source uses .. as a range literal.",
                "range(0, 10)",
                "Mux creates numeric ranges with the range function.",
                "Use range(start, end).",
            ),
            Self::ParseExpectedToken => (
                "expected token",
                "Required grammar is missing at the reported token.",
                "func main( {",
                "The parser reports where the grammar first becomes invalid.",
                "Add the token named by the diagnostic.",
            ),
            Self::ParseExpectedExpression => (
                "expected expression",
                "A statement position cannot start with the next token.",
                "if { print(\"hi\") }",
                "Conditions, arguments, and initializers require expressions.",
                "Write an expression at the reported location.",
            ),
            Self::ParseExpectedType => (
                "expected type",
                "A type position is empty or invalid.",
                "func f() returns",
                "Declarations and return signatures must name a type.",
                "Add the intended type.",
            ),
            Self::ParseControlFlowOutsideLoop => (
                "loop control outside a loop",
                "break or continue appears outside a loop.",
                "break",
                "Loop-control statements only have meaning inside for or while.",
                "Move it into a loop or remove it.",
            ),
            Self::ParseReturnOutsideFunction => (
                "return outside a function",
                "return appears outside a function.",
                "return 42",
                "A return transfers control to an enclosing function.",
                "Put the return in a function.",
            ),
            Self::ParseErrorLimit => (
                "too many parse errors",
                "Parser recovery reached its 100-diagnostic limit.",
                "many malformed declarations",
                "Further parser errors are usually cascades from the first errors.",
                "Fix the earliest reported syntax errors and compile again.",
            ),
            Self::UndefinedName => (
                "undefined name",
                "Code refers to a name that is not in scope.",
                "print(missing)",
                "Mux resolves names statically before code generation.",
                "Declare, import, or correctly spell the name.",
            ),
            Self::DuplicateDeclaration => (
                "duplicate declaration",
                "A scope contains two declarations with the same name.",
                "auto x = 1\\nauto x = 2",
                "A declaration must have one unambiguous binding.",
                "Rename one declaration or assign to the existing binding.",
            ),
            Self::TypeMismatch => (
                "type mismatch",
                "An expression does not have the required type.",
                "int n = \"hello\"",
                "Mux does not insert implicit conversions between unrelated types.",
                "Change the expression or declare the matching type.",
            ),
            Self::WrongArgumentCount => (
                "wrong number of arguments",
                "A call supplies the wrong number of arguments.",
                "add(1)",
                "Calls must satisfy the complete function signature.",
                "Pass the missing arguments or remove extras.",
            ),
            Self::MissingReturn => (
                "missing return",
                "A non-void function can finish without returning a value.",
                "func f() returns int { if true { return 1 } }",
                "Every reachable path must produce the declared type.",
                "Return a value on every path or change the return type.",
            ),
            Self::NonExhaustiveMatch => (
                "non-exhaustive match",
                "A match does not handle every possible value.",
                "match x { 1 { print(\"one\") } }",
                "The compiler must know what happens for every input.",
                "Add the missing cases or a final _ arm.",
            ),
            Self::CannotAssign => (
                "invalid assignment",
                "An assignment targets a constant or non-assignable value.",
                "const x = 1\\nx = 2",
                "Constants and computed values cannot be changed.",
                "Use a mutable declaration or the intended target.",
            ),
            Self::UnknownMember => (
                "unknown member",
                "A field or method is not defined for the receiver type.",
                "value.missing()",
                "Member lookup uses the receiver's declared type.",
                "Correct the member name or use a supported API.",
            ),
            Self::InvalidOperation => (
                "invalid operation",
                "An operator is not defined for the operand types.",
                "true + false",
                "Operators have explicit type rules and do not coerce operands.",
                "Use compatible operands or a supported operation.",
            ),
            Self::DivisionByZero => (
                "division by zero",
                "A division or modulo operation has a divisor provably equal to zero.",
                "10 / 0",
                "The operation would panic on every execution.",
                "Use a non-zero divisor or handle the zero case before dividing.",
            ),
            Self::NestedFunctionCapture => (
                "nested function captures a local",
                "A named nested function refers to a local binding from its enclosing function.",
                "func outer() returns void { int value = 1\nfunc inner() returns void { print(value) } }",
                "Named nested functions do not carry a captured environment.",
                "Pass the value as a parameter or use a capturing lambda.",
            ),
            Self::InvalidPattern => (
                "invalid pattern",
                "A match pattern does not fit its matched value.",
                "match value { Unknown(x) { } }",
                "Patterns must fit the matched type.",
                "Use a pattern supported by that type.",
            ),
            Self::InvalidTypeArguments => (
                "invalid type arguments",
                "A generic type has the wrong arguments.",
                "list<int, string>",
                "Generic declarations define their accepted argument count.",
                "Pass the declared type arguments.",
            ),
            Self::UninitializedReadError => (
                "read before assignment",
                "A variable is read before it has been assigned on every path.",
                "int value\nprint(value)",
                "Mux does not allow a possibly uninitialized value to reach code generation.",
                "Initialize it or assign it on every path before reading it.",
            ),
            Self::ModuleNotFound => (
                "module not found",
                "An import path does not resolve to a Mux module.",
                "import missing",
                "Imports resolve from the project and embedded standard library.",
                "Correct the path or add the module.",
            ),
            Self::ImportFailure => (
                "module import failed",
                "An imported module could not be read or parsed.",
                "import broken",
                "Dependent analysis cannot proceed until the imported module is valid.",
                "Fix the reported problem in that module.",
            ),
            Self::InternalCompiler => (
                "internal compiler error",
                "Mux reached an impossible state or code-generation failure.",
                "compiler-internal failure",
                "This is a compiler bug, not a source-language error.",
                "Report the complete diagnostic and compiler version.",
            ),
            Self::UnusedBinding => (
                "unused binding",
                "A declared binding is never read.",
                "auto unused = 1",
                "The declaration has no observable use.",
                "Remove it, use it, or write _ intentionally.",
            ),
            Self::ShadowedBinding => (
                "shadowed binding",
                "A declaration hides another binding with the same name.",
                "auto x = 1\\nif true { auto x = 2 }",
                "Shadowing can make a nearby use refer to a different value.",
                "Rename the inner binding if accidental.",
            ),
            Self::UnreachableCode => (
                "unreachable code",
                "Control flow can never reach a statement or branch.",
                "return\\nprint(\"never\")",
                "The analyzer proved that an earlier terminator exits the path.",
                "Remove the code or move it before the terminator.",
            ),
            Self::DeadAssignment => (
                "dead assignment",
                "A value is overwritten before it is read.",
                "x = 1\\nx = 2",
                "The first store has no effect on the result.",
                "Remove the first assignment or use its value.",
            ),
            Self::UninitializedRead => (
                "possibly uninitialized read",
                "A variable is read before assignment on a possible path.",
                "int value\\nprint(value)",
                "The analyzer tracks assignments across control-flow paths.",
                "Initialize it or assign on every path.",
            ),
            Self::ConstantCondition => (
                "constant condition",
                "A condition is provably always true or false.",
                "if 1 == 1 { print(\"yes\") }",
                "Constant folding proved the branch outcome.",
                "Remove the condition or use the intended runtime value.",
            ),
            Self::RedundantConstruct => (
                "redundant construct",
                "A construct can be simplified without changing its result.",
                "value && true",
                "The analyzer proved that a boolean operand has no effect on the result.",
                "Apply the suggested simplification.",
            ),
        };
        DiagnosticInfo {
            code: self,
            level: self.level(),
            title,
            trigger,
            example,
            explanation,
            fix,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|code| code.as_str() == value)
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_codes_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for code in DiagnosticCode::all() {
            assert!(seen.insert(code.as_str()));
            assert_eq!(DiagnosticCode::parse(code.as_str()), Some(*code));
            let info = code.info();
            assert_eq!(info.level, code.level());
            assert!(!info.trigger.is_empty());
            assert!(!info.example.is_empty());
            assert!(!info.explanation.is_empty());
            assert!(!info.fix.is_empty());
        }
    }

    #[test]
    fn reserved_codes_are_not_published_as_emitted_diagnostics() {
        let emitted = DiagnosticCode::all();
        for code in DiagnosticCode::reserved() {
            assert!(!emitted.contains(code), "{} is reserved and emitted", code);
        }
    }
}
