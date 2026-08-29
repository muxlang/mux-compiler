use crate::ast::PrimitiveType;
use crate::lexer::Span;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum SymbolKind {
    Function,
    Variable,
    Class,
    Interface,
    Enum,
    Constant,
    Import,
    Type, // For generic type parameters
}

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub span: Span,
    pub type_: Option<Type>,
    pub interfaces: HashMap<String, (Vec<Type>, HashMap<String, MethodSig>)>,
    pub methods: HashMap<String, MethodSig>,
    pub fields: HashMap<String, (Type, bool)>, // (Type, is_const)
    pub type_params: Vec<(String, Vec<String>)>,
    pub original_name: Option<String>, // For imported symbols with aliases, stores the original source name
    pub llvm_name: Option<String>,     // For imported symbols, stores the mangled LLVM name
    pub default_param_count: usize, // Number of parameters with default values (must be at the end)
    pub variants: Option<Vec<String>>, // For enums: list of variant names. None for non-enums
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Primitive(PrimitiveType),
    List(Box<Type>),
    Map(Box<Type>, Box<Type>),
    Set(Box<Type>),
    Tuple(Box<Type>, Box<Type>),

    Optional(Box<Type>),
    Result(Box<Type>, Box<Type>),
    Reference(Box<Type>),
    Void,
    Never,
    EmptyList,
    EmptyMap,
    EmptySet,
    Function {
        params: Vec<Type>,
        returns: Box<Type>,
        default_count: usize, // Number of parameters with default values (must be at the end)
    },
    Named(String, Vec<Type>),
    Variable(String),
    Generic(String), // Generic parameter like "T", "U"
    // used in codegen for concrete instantiations of generic types
    #[allow(dead_code)]
    Instantiated(String, Vec<Type>), // Concrete instantiation like "Pair<string, bool>"
    Module(String), // Module namespace (e.g., "shapes" from "import shapes")
}

/// Encode a type into a deterministic, injective identifier for generated
/// symbols and specialization keys.
///
/// Each node carries a tag and length-framed fields. Names are encoded as
/// hexadecimal UTF-8 bytes, so source names cannot be confused with framing
/// characters or with the representation of another type node. This is an
/// internal name format; source-facing type rendering should remain separate.
pub fn mangle_type_name(type_: &Type) -> String {
    fn frame(tag: &str, fields: impl IntoIterator<Item = String>) -> String {
        let mut encoded = tag.to_string();
        for field in fields {
            encoded.push_str(&field.len().to_string());
            encoded.push('_');
            encoded.push_str(&field);
        }
        encoded
    }

    fn text(value: &str) -> String {
        value
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    match type_ {
        Type::Primitive(PrimitiveType::Int) => "I".to_string(),
        Type::Primitive(PrimitiveType::Float) => "F".to_string(),
        Type::Primitive(PrimitiveType::Bool) => "B".to_string(),
        Type::Primitive(PrimitiveType::Str) => "S".to_string(),
        Type::Primitive(PrimitiveType::Char) => "C".to_string(),
        Type::Primitive(PrimitiveType::Void) => "H".to_string(),
        Type::Void => "V".to_string(),
        Type::Primitive(PrimitiveType::Auto) => "A".to_string(),
        Type::List(inner) => frame("L", [mangle_type_name(inner)]),
        Type::Map(key, value) => frame("M", [mangle_type_name(key), mangle_type_name(value)]),
        Type::Set(inner) => frame("T", [mangle_type_name(inner)]),
        Type::Tuple(left, right) => frame("U", [mangle_type_name(left), mangle_type_name(right)]),
        Type::Optional(inner) => frame("O", [mangle_type_name(inner)]),
        Type::Result(ok, err) => frame("R", [mangle_type_name(ok), mangle_type_name(err)]),
        Type::Reference(inner) => frame("Q", [mangle_type_name(inner)]),
        Type::EmptyList => "l".to_string(),
        Type::EmptyMap => "m".to_string(),
        Type::EmptySet => "t".to_string(),
        Type::Function {
            params,
            returns,
            default_count,
        } => {
            let mut fields = vec![params.len().to_string()];
            fields.extend(params.iter().map(mangle_type_name));
            fields.push(mangle_type_name(returns));
            fields.push(default_count.to_string());
            frame("Y", fields)
        }
        Type::Named(name, args) => {
            let mut fields = vec![text(name), args.len().to_string()];
            fields.extend(args.iter().map(mangle_type_name));
            frame("N", fields)
        }
        Type::Instantiated(name, args) => {
            let mut fields = vec![text(name), args.len().to_string()];
            fields.extend(args.iter().map(mangle_type_name));
            frame("X", fields)
        }
        Type::Variable(name) => frame("W", [text(name)]),
        Type::Generic(name) => frame("G", [text(name)]),
        Type::Never => "Z".to_string(),
        Type::Module(name) => frame("D", [text(name)]),
    }
}

#[derive(Debug, Clone)]
pub struct GenericContext {
    pub type_params: HashMap<String, Type>, // T -> string, U -> bool
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    pub params: Vec<Type>,
    pub return_type: Type,
    pub is_static: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuiltInSig {
    pub params: Vec<Type>,
    pub return_type: Type,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn named(name: &str) -> Type {
        Type::Named(name.to_string(), Vec::new())
    }

    #[test]
    fn type_mangling_is_injective_across_type_shapes() {
        let types = vec![
            Type::Primitive(PrimitiveType::Int),
            Type::Primitive(PrimitiveType::Float),
            Type::Primitive(PrimitiveType::Bool),
            Type::Primitive(PrimitiveType::Char),
            Type::Primitive(PrimitiveType::Str),
            Type::Primitive(PrimitiveType::Void),
            Type::Primitive(PrimitiveType::Auto),
            Type::List(Box::new(named("Item"))),
            Type::Map(Box::new(named("Key")), Box::new(named("Value"))),
            Type::Set(Box::new(named("Item"))),
            Type::Tuple(Box::new(named("Left")), Box::new(named("Right"))),
            Type::Optional(Box::new(named("Item"))),
            Type::Result(Box::new(named("Ok")), Box::new(named("Err"))),
            Type::Reference(Box::new(named("Item"))),
            Type::Void,
            Type::Never,
            Type::EmptyList,
            Type::EmptyMap,
            Type::EmptySet,
            Type::Function {
                params: vec![named("Param")],
                returns: Box::new(named("Return")),
                default_count: 0,
            },
            named("Plain"),
            Type::Named("Generic".to_string(), vec![named("Arg")]),
            Type::Variable("T".to_string()),
            Type::Generic("T".to_string()),
            Type::Instantiated("Generic".to_string(), vec![named("Arg")]),
            Type::Module("module".to_string()),
        ];

        let mangled: HashSet<_> = types.iter().map(mangle_type_name).collect();
        assert_eq!(mangled.len(), types.len());
        assert!(mangled.iter().all(|name| {
            name.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        }));
    }

    #[test]
    fn nested_type_mangling_does_not_flatten_into_collisions() {
        let types = [
            Type::Named("Outer".to_string(), vec![named("A_B")]),
            Type::Named("Outer_A".to_string(), vec![named("B")]),
            Type::Named(
                "Outer".to_string(),
                vec![Type::List(Box::new(named("A_B")))],
            ),
            Type::Named("Outer".to_string(), vec![named("list_A_B")]),
            Type::Map(Box::new(named("A_B")), Box::new(named("C"))),
            Type::Map(Box::new(named("A")), Box::new(named("B_C"))),
        ];

        let mangled: HashSet<_> = types.iter().map(mangle_type_name).collect();
        assert_eq!(mangled.len(), types.len());
    }
}
