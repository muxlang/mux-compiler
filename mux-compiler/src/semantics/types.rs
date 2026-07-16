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
