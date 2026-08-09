use crate::ast::{BinaryOp, PrimitiveType};
use crate::lexer::Span;
use crate::semantics::types::Type;

pub fn format_span_location(span: &Span) -> String {
    format!("{}:{}", span.row_start, span.col_start)
}

pub fn format_type(t: &Type) -> String {
    match t {
        Type::Primitive(p) => format_primitive_type(p),
        Type::List(inner) => format!("list<{}>", format_type(inner)),
        Type::Map(k, v) => format!("map<{}, {}>", format_type(k), format_type(v)),
        Type::Set(inner) => format!("set<{}>", format_type(inner)),
        // Every other composite prints in the form the parser accepts, and a
        // tuple must too: "(int, string)" is not writable syntax, so a
        // diagnostic naming that type gave the reader nothing they could type.
        Type::Tuple(l, r) => format!("tuple<{}, {}>", format_type(l), format_type(r)),
        Type::Optional(inner) => format!("optional<{}>", format_type(inner)),
        Type::Result(ok, err) => format!("result<{}, {}>", format_type(ok), format_type(err)),
        Type::Reference(inner) => format!("&{}", format_type(inner)),
        Type::Void => "void".to_string(),
        Type::Never => "never".to_string(),
        Type::EmptyList => "list<?>".to_string(),
        Type::EmptyMap => "map<?, ?>".to_string(),
        Type::EmptySet => "set<?>".to_string(),
        Type::Function {
            params,
            returns,
            default_count: _,
        } => {
            let params_str = params
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", ");
            format!("fn({params_str}) -> {}", format_type(returns))
        }
        Type::Named(name, args) => {
            if args.is_empty() {
                name.clone()
            } else {
                let args_str = args.iter().map(format_type).collect::<Vec<_>>().join(", ");
                format!("{name}<{args_str}>")
            }
        }
        Type::Variable(name) | Type::Generic(name) => name.clone(),
        Type::Instantiated(name, args) => {
            let args_str = args.iter().map(format_type).collect::<Vec<_>>().join(", ");
            format!("{name}<{args_str}>")
        }
        Type::Module(name) => format!("module:{name}"),
    }
}

fn format_primitive_type(p: &PrimitiveType) -> String {
    match p {
        PrimitiveType::Int => "int".to_string(),
        PrimitiveType::Float => "float".to_string(),
        PrimitiveType::Bool => "bool".to_string(),
        PrimitiveType::Char => "char".to_string(),
        PrimitiveType::Str => "string".to_string(),
        PrimitiveType::Void => "void".to_string(),
        PrimitiveType::Auto => "auto".to_string(),
    }
}

pub fn format_binary_op(op: &BinaryOp) -> String {
    match op {
        BinaryOp::Add => "+".to_string(),
        BinaryOp::Subtract => "-".to_string(),
        BinaryOp::Multiply => "*".to_string(),
        BinaryOp::Divide => "/".to_string(),
        BinaryOp::Modulo => "%".to_string(),
        BinaryOp::Exponent => "**".to_string(),
        BinaryOp::Equal => "==".to_string(),
        BinaryOp::NotEqual => "!=".to_string(),
        BinaryOp::Less => "<".to_string(),
        BinaryOp::LessEqual => "<=".to_string(),
        BinaryOp::Greater => ">".to_string(),
        BinaryOp::GreaterEqual => ">=".to_string(),
        BinaryOp::LogicalAnd => "&&".to_string(),
        BinaryOp::LogicalOr => "||".to_string(),
        BinaryOp::In => "in".to_string(),
        BinaryOp::Assign => "=".to_string(),
        BinaryOp::AddAssign => "+=".to_string(),
        BinaryOp::SubtractAssign => "-=".to_string(),
        BinaryOp::MultiplyAssign => "*=".to_string(),
        BinaryOp::DivideAssign => "/=".to_string(),
        BinaryOp::ModuloAssign => "%=".to_string(),
    }
}
