//! Canonical stdlib registry
//!
//! This module centralizes the standard library (stdlib) description used by the
//! semantic analyzer and codegen. It provides:
//! - `StdlibItem` — canonical representation for stdlib functions and constants
//! - `lookup_stdlib_item` / `all_stdlib_items` — accessors for items
//! - `BUILT_IN_FUNCTIONS` — built-in function signatures used for name resolution
//! - `*_STDLIB_ITEMS` tables for per-module items (IO, MATH, DATETIME, ASSERT, SYNC)
//! - `net_module_class_symbols` / `sync_module_class_symbols` — class symbol builders
//! - `stdlib_item_to_symbol` / `register_stdlib_item_into` — helpers to convert and
//!   register stdlib items into the compiler's `SymbolTable`.
//!
//! Contributing
//! - Add new stdlib items by updating the appropriate table (`MATH_STDLIB_ITEMS`,
//!   `IO_STDLIB_ITEMS`, or `STDLIB_ITEMS` for PHF-backed descriptors).
//! - Prefer adding entries to the module-specific HashMaps rather than spreading
//!   duplicates across the codebase — these are the single source of truth.
//! - For class types (e.g., `net` / `sync`), update the corresponding `*_methods`
//!   helper and the `*_module_class_symbols` function so the analyzer can import
//!   class symbols.
//!
//! Rationale
//! - The previous code duplicated stdlib declarations in multiple places (symbol
//!   table, semantic analyzer). Consolidating them here prevents drift and keeps
//!   a clear mapping between stdlib names and their runtime/LLVM counterparts.

use crate::ast::PrimitiveType;
use crate::lexer::Span;
use crate::semantics::types::{BuiltInSig, MethodSig, Symbol, SymbolKind, Type};
use lazy_static::lazy_static;
use phf::phf_map;
use std::collections::HashMap;

fn float() -> Type {
    Type::Primitive(PrimitiveType::Float)
}
fn int() -> Type {
    Type::Primitive(PrimitiveType::Int)
}
fn str_() -> Type {
    Type::Primitive(PrimitiveType::Str)
}
fn bool_() -> Type {
    Type::Primitive(PrimitiveType::Bool)
}
fn json_type() -> Type {
    Type::Named("Json".to_string(), Vec::new())
}

fn sig(params: Vec<Type>, return_type: Type) -> BuiltInSig {
    BuiltInSig {
        params,
        return_type,
    }
}

fn register_batch(
    m: &mut HashMap<&'static str, BuiltInSig>,
    names: &[&'static str],
    sig: BuiltInSig,
) {
    for name in names {
        m.insert(name, sig.clone());
    }
}

/// Value representation for compile-time constants
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum ConstantValue {
    Float(f64),
    Int(i64),
    Bool(bool),
}

/// Types of items in stdlib modules
/// Owns its data to avoid Box::leak
#[derive(Debug, Clone, PartialEq)]
pub enum StdlibItem {
    Function {
        params: Vec<Type>,
        ret: Type,
        llvm_name: String,
    },
    Constant {
        ty: Type,
        value: ConstantValue,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeDesc {
    Float,
    Int,
    Bool,
    Void,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstDesc {
    Pi,
    E,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdlibItemDesc {
    Function {
        params: &'static [TypeDesc],
        ret: TypeDesc,
        llvm_name: &'static str,
    },
    Constant {
        ty: TypeDesc,
        value: ConstDesc,
    },
}

const EMPTY_PARAM_DESC: &[TypeDesc] = &[];
const INT_PARAM_DESC: &[TypeDesc] = &[TypeDesc::Int];
const INT_INT_PARAM_DESC: &[TypeDesc] = &[TypeDesc::Int, TypeDesc::Int];

fn materialize_type(desc: TypeDesc) -> Type {
    match desc {
        TypeDesc::Float => Type::Primitive(PrimitiveType::Float),
        TypeDesc::Int => Type::Primitive(PrimitiveType::Int),
        TypeDesc::Bool => Type::Primitive(PrimitiveType::Bool),
        TypeDesc::Void => Type::Void,
    }
}

fn materialize_const(desc: ConstDesc) -> ConstantValue {
    match desc {
        ConstDesc::Pi => ConstantValue::Float(std::f64::consts::PI),
        ConstDesc::E => ConstantValue::Float(std::f64::consts::E),
    }
}

fn materialize_stdlib_item(desc: &StdlibItemDesc) -> StdlibItem {
    match desc {
        StdlibItemDesc::Function {
            params,
            ret,
            llvm_name,
        } => StdlibItem::Function {
            params: params.iter().copied().map(materialize_type).collect(),
            ret: materialize_type(*ret),
            llvm_name: (*llvm_name).to_string(),
        },
        StdlibItemDesc::Constant { ty, value } => StdlibItem::Constant {
            ty: materialize_type(*ty),
            value: materialize_const(*value),
        },
    }
}

// Static arrays for function parameters (required for PHF const compatibility)
static FLOAT_PARAM: &[Type] = &[Type::Primitive(PrimitiveType::Float)];
static FLOAT_FLOAT_PARAMS: &[Type] = &[
    Type::Primitive(PrimitiveType::Float),
    Type::Primitive(PrimitiveType::Float),
];
static INT_PARAM: &[Type] = &[Type::Primitive(PrimitiveType::Int)];
static INT_STR_PARAMS: &[Type] = &[
    Type::Primitive(PrimitiveType::Int),
    Type::Primitive(PrimitiveType::Str),
];
static BOOL_PARAM: &[Type] = &[Type::Primitive(PrimitiveType::Bool)];
static BOOL_STR_PARAMS: &[Type] = &[
    Type::Primitive(PrimitiveType::Bool),
    Type::Primitive(PrimitiveType::Str),
];
static STR_PARAM: &[Type] = &[Type::Primitive(PrimitiveType::Str)];
static STR_STR_PARAMS: &[Type] = &[
    Type::Primitive(PrimitiveType::Str),
    Type::Primitive(PrimitiveType::Str),
];
static EMPTY_PARAMS: &[Type] = &[];

// Lazy static arrays for ASSERT functions (to avoid Box::leak)
lazy_static! {
    static ref T_PARAM: Vec<Type> = vec![Type::Variable("T".to_string())];
    static ref T_T_PARAMS: Vec<Type> = vec![
        Type::Variable("T".to_string()),
        Type::Variable("T".to_string())
    ];
    static ref OPTIONAL_T_PARAM: Vec<Type> =
        vec![Type::Optional(Box::new(Type::Variable("T".to_string())))];
    static ref RESULT_T_E_PARAMS: Vec<Type> = vec![Type::Result(
        Box::new(Type::Variable("T".to_string())),
        Box::new(Type::Variable("E".to_string()))
    )];
}

fn io_fn(name: &'static str, params: &'static [Type], ret: Type) -> StdlibItem {
    StdlibItem::Function {
        params: params.to_vec(),
        ret,
        llvm_name: name.to_string(),
    }
}

fn io_str_fn(name: &'static str, ret: Type) -> StdlibItem {
    io_fn(name, STR_PARAM, ret)
}
fn io_str_str_fn(name: &'static str, ret: Type) -> StdlibItem {
    io_fn(name, STR_STR_PARAMS, ret)
}

fn io_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(str_()))
}

fn net_bytes_type() -> Type {
    Type::List(Box::new(Type::Primitive(PrimitiveType::Int)))
}
fn net_tuple_bytes_addr_type() -> Type {
    Type::Tuple(Box::new(net_bytes_type()), Box::new(str_()))
}
fn tcp_stream_type() -> Type {
    Type::Named("TcpStream".to_string(), Vec::new())
}
fn udp_socket_type() -> Type {
    Type::Named("UdpSocket".to_string(), Vec::new())
}
fn tcp_listener_type() -> Type {
    Type::Named("TcpListener".to_string(), Vec::new())
}
fn sql_connection_type() -> Type {
    Type::Named("Connection".to_string(), Vec::new())
}
fn sql_transaction_type() -> Type {
    Type::Named("Transaction".to_string(), Vec::new())
}
fn sql_result_set_type() -> Type {
    Type::Named("ResultSet".to_string(), Vec::new())
}
fn sql_value_type() -> Type {
    Type::Named("SqlValue".to_string(), Vec::new())
}
fn sql_row_type() -> Type {
    Type::Map(Box::new(str_()), Box::new(sql_value_type()))
}

fn make_class_symbol(name: &str, methods: HashMap<String, MethodSig>, span: Span) -> Symbol {
    Symbol {
        kind: SymbolKind::Class,
        span,
        type_: Some(Type::Named(name.to_string(), Vec::new())),
        interfaces: HashMap::new(),
        methods,
        fields: HashMap::new(),
        type_params: Vec::new(),
        original_name: None,
        llvm_name: None,
        default_param_count: 0,
        variants: None,
    }
}

fn make_import_module_symbol(module_name: &str, span: Span) -> Symbol {
    Symbol {
        kind: SymbolKind::Import,
        span,
        type_: Some(Type::Module(module_name.to_string())),
        interfaces: HashMap::new(),
        methods: HashMap::new(),
        fields: HashMap::new(),
        type_params: Vec::new(),
        original_name: None,
        llvm_name: None,
        default_param_count: 0,
        variants: None,
    }
}

macro_rules! define_methods {
    (
        $($method_name:expr => {
            params: [$($param:expr),* $(,)?],
            return_type: $ret_type:expr,
            is_static: $is_static:expr $(,)?
        }),*
        $(,)?
    ) => {
        {
            let mut methods = HashMap::new();
            $(
                methods.insert(
                    $method_name.to_string(),
                    MethodSig {
                        params: vec![$($param),*],
                        return_type: $ret_type,
                        is_static: $is_static,
                    },
                );
            )*
            methods
        }
    };
}

macro_rules! insert_items {
    (
        $map:expr;
        $($name:expr => $item_expr:expr),*
        $(,)?
    ) => {
        $(
            $map.insert($name, $item_expr);
        )*
    };
}

fn tcp_stream_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "connect" => {
            params: [str_()],
            return_type: io_result(tcp_stream_type()),
            is_static: true
        },
        "read" => {
            params: [int()],
            return_type: io_result(net_bytes_type()),
            is_static: false
        },
        "write" => {
            params: [net_bytes_type()],
            return_type: io_result(int()),
            is_static: false
        }
    };
    insert_socket_common_methods(&mut methods);
    methods
}

fn udp_socket_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "bind" => {
            params: [str_()],
            return_type: io_result(udp_socket_type()),
            is_static: true
        },
        "send_to" => {
            params: [net_bytes_type(), str_()],
            return_type: io_result(int()),
            is_static: false
        },
        "recv_from" => {
            params: [int()],
            return_type: io_result(net_tuple_bytes_addr_type()),
            is_static: false
        }
    };
    insert_socket_common_methods(&mut methods);
    methods
}

fn insert_socket_common_methods(methods: &mut HashMap<String, MethodSig>) {
    methods.extend(define_methods! {
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        },
        "set_nonblocking" => {
            params: [bool_()],
            return_type: io_result(Type::Void),
            is_static: false
        },
        "peer_addr" => {
            params: [],
            return_type: io_result(str_()),
            is_static: false
        },
        "local_addr" => {
            params: [],
            return_type: io_result(str_()),
            is_static: false
        }
    });
}

fn tcp_listener_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "bind" => {
            params: [str_()],
            return_type: io_result(tcp_listener_type()),
            is_static: true
        },
        "accept" => {
            params: [],
            return_type: io_result(tcp_stream_type()),
            is_static: false
        },
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        },
        "set_nonblocking" => {
            params: [bool_()],
            return_type: io_result(Type::Void),
            is_static: false
        },
        "local_addr" => {
            params: [],
            return_type: io_result(str_()),
            is_static: false
        }
    }
}

#[must_use]
pub fn net_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "TcpStream".to_string(),
        make_class_symbol("TcpStream", tcp_stream_methods(), span),
    );
    classes.insert(
        "UdpSocket".to_string(),
        make_class_symbol("UdpSocket", udp_socket_methods(), span),
    );
    classes.insert(
        "TcpListener".to_string(),
        make_class_symbol("TcpListener", tcp_listener_methods(), span),
    );
    classes.insert(
        "http".to_string(),
        make_import_module_symbol("net.http", span),
    );
    classes
}

fn sql_connection_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        },
        "begin_transaction" => {
            params: [],
            return_type: io_result(sql_transaction_type()),
            is_static: false
        }
    };
    insert_sql_query_methods(&mut methods);
    methods
}

fn sql_transaction_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "commit" => {
            params: [],
            return_type: io_result(Type::Void),
            is_static: false
        },
        "rollback" => {
            params: [],
            return_type: io_result(Type::Void),
            is_static: false
        }
    };
    insert_sql_query_methods(&mut methods);
    methods
}

fn insert_sql_query_methods(methods: &mut HashMap<String, MethodSig>) {
    methods.extend(define_methods! {
        "execute" => {
            params: [str_()],
            return_type: io_result(int()),
            is_static: false
        },
        "execute_params" => {
            params: [str_(), Type::List(Box::new(sql_value_type()))],
            return_type: io_result(int()),
            is_static: false
        },
        "query" => {
            params: [str_()],
            return_type: io_result(sql_result_set_type()),
            is_static: false
        },
        "query_params" => {
            params: [str_(), Type::List(Box::new(sql_value_type()))],
            return_type: io_result(sql_result_set_type()),
            is_static: false
        }
    });
}

fn sql_result_set_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "rows" => {
            params: [],
            return_type: Type::List(Box::new(sql_row_type())),
            is_static: false
        },
        "next" => {
            params: [],
            return_type: Type::Optional(Box::new(sql_row_type())),
            is_static: false
        },
        "columns" => {
            params: [],
            return_type: Type::List(Box::new(str_())),
            is_static: false
        }
    }
}

fn sql_value_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "is_null" => {
            params: [],
            return_type: bool_(),
            is_static: false
        },
        // Result, so a caller is told what the column actually held rather
        // than just "not that kind". The conversion helpers already produce a
        // message; the accessors used to discard it with `.ok()` (#404).
        "as_bool" => {
            params: [],
            return_type: io_result(bool_()),
            is_static: false
        },
        "as_int" => {
            params: [],
            return_type: io_result(int()),
            is_static: false
        },
        "as_float" => {
            params: [],
            return_type: io_result(float()),
            is_static: false
        },
        "as_string" => {
            params: [],
            return_type: io_result(str_()),
            is_static: false
        },
        "as_bytes" => {
            params: [],
            return_type: io_result(Type::List(Box::new(int()))),
            is_static: false
        },
        "to_string" => {
            params: [],
            return_type: str_(),
            is_static: false
        }
    }
}

#[must_use]
pub fn sql_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "Connection".to_string(),
        make_class_symbol("Connection", sql_connection_methods(), span),
    );
    classes.insert(
        "Transaction".to_string(),
        make_class_symbol("Transaction", sql_transaction_methods(), span),
    );
    classes.insert(
        "ResultSet".to_string(),
        make_class_symbol("ResultSet", sql_result_set_methods(), span),
    );
    classes.insert(
        "SqlValue".to_string(),
        make_class_symbol("SqlValue", sql_value_methods(), span),
    );
    classes
}

fn thread_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "join" => {
            params: [],
            return_type: Type::Result(Box::new(Type::Void), Box::new(str_())),
            is_static: false
        },
        "detach" => {
            params: [],
            return_type: Type::Result(Box::new(Type::Void), Box::new(str_())),
            is_static: false
        }
    }
}

fn condvar_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: Type::Named("CondVar".to_string(), Vec::new()),
            is_static: true
        },
        "wait" => {
            params: [Type::Named("Mutex".to_string(), Vec::new())],
            return_type: Type::Result(Box::new(Type::Void), Box::new(str_())),
            is_static: false
        },
        "signal" => {
            params: [],
            return_type: Type::Result(Box::new(Type::Void), Box::new(str_())),
            is_static: false
        },
        "broadcast" => {
            params: [],
            return_type: Type::Result(Box::new(Type::Void), Box::new(str_())),
            is_static: false
        }
    }
}

fn rwlock_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "new" => {
            params: [],
            return_type: Type::Named("RwLock".to_string(), Vec::new()),
            is_static: true
        },
        "read_lock" => {
            params: [],
            return_type: io_result(Type::Void),
            is_static: false
        },
        "write_lock" => {
            params: [],
            return_type: io_result(Type::Void),
            is_static: false
        }
    };
    insert_sync_unlock_method(&mut methods);
    methods
}

fn mutex_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "new" => {
            params: [],
            return_type: Type::Named("Mutex".to_string(), Vec::new()),
            is_static: true
        },
        "lock" => {
            params: [],
            return_type: io_result(Type::Void),
            is_static: false
        }
    };
    insert_sync_unlock_method(&mut methods);
    methods
}

fn insert_sync_unlock_method(methods: &mut HashMap<String, MethodSig>) {
    methods.extend(define_methods! {
        "unlock" => {
            params: [],
            return_type: io_result(Type::Void),
            is_static: false
        }
    });
}

#[must_use]
pub fn sync_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "Thread".to_string(),
        make_class_symbol("Thread", thread_methods(), span),
    );
    classes.insert(
        "CondVar".to_string(),
        make_class_symbol("CondVar", condvar_methods(), span),
    );
    classes.insert(
        "RwLock".to_string(),
        make_class_symbol("RwLock", rwlock_methods(), span),
    );
    classes.insert(
        "Mutex".to_string(),
        make_class_symbol("Mutex", mutex_methods(), span),
    );
    classes
}

fn datetime_fn(name: &'static str, params: &'static [Type], ret: Type) -> StdlibItem {
    io_fn(name, params, ret)
}

static STDLIB_ITEMS: phf::Map<&str, StdlibItemDesc> = phf_map! {
   "math.pi" => StdlibItemDesc::Constant { ty: TypeDesc::Float, value: ConstDesc::Pi },
   "math.e" => StdlibItemDesc::Constant { ty: TypeDesc::Float, value: ConstDesc::E },
   "random.seed" => StdlibItemDesc::Function { params: INT_PARAM_DESC, ret: TypeDesc::Void, llvm_name: "mux_rand_init" },
   "random.next_int" => StdlibItemDesc::Function { params: EMPTY_PARAM_DESC, ret: TypeDesc::Int, llvm_name: "mux_rand_int" },
   "random.next_range" => StdlibItemDesc::Function { params: INT_INT_PARAM_DESC, ret: TypeDesc::Int, llvm_name: "mux_rand_range" },
   "random.next_float" => StdlibItemDesc::Function { params: EMPTY_PARAM_DESC, ret: TypeDesc::Float, llvm_name: "mux_rand_float" },
    "random.next_bool" => StdlibItemDesc::Function { params: EMPTY_PARAM_DESC, ret: TypeDesc::Bool, llvm_name: "mux_rand_bool" },
};

lazy_static! {
    pub static ref IO_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        insert_items! {
            m;
            "io.read_file" => io_str_fn("mux_io_read_file", io_result(str_())),
            "io.write_file" => io_str_str_fn("mux_io_write_file", io_result(Type::Void)),
            "io.exists" => io_str_fn("mux_io_exists", io_result(bool_())),
            "io.remove" => io_str_fn("mux_io_remove", io_result(Type::Void)),
            "io.is_file" => io_str_fn("mux_io_is_file", io_result(bool_())),
            "io.is_dir" => io_str_fn("mux_io_is_dir", io_result(bool_())),
            "io.mkdir" => io_str_fn("mux_io_mkdir", io_result(Type::Void)),
            "io.listdir" => io_str_fn("mux_io_listdir", io_result(Type::List(Box::new(Type::Primitive(PrimitiveType::Str))))),
            "io.join" => io_str_str_fn("mux_io_join", io_result(str_())),
            "io.basename" => io_str_fn("mux_io_basename", io_result(str_())),
            "io.dirname" => io_str_fn("mux_io_dirname", io_result(str_()))
        }
        m
    };
    pub static ref MATH_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        fn make_math_fn(llvm_name: &'static str, params: &'static [Type]) -> StdlibItem {
            StdlibItem::Function {
                params: params.to_vec(),
                ret: Type::Primitive(PrimitiveType::Float),
                llvm_name: llvm_name.to_string(),
            }
        }
        let mut m = HashMap::new();
        insert_items! {
            m;
            "math.sqrt" => make_math_fn("mux_math_sqrt", FLOAT_PARAM),
            "math.sin" => make_math_fn("mux_math_sin", FLOAT_PARAM),
            "math.cos" => make_math_fn("mux_math_cos", FLOAT_PARAM),
            "math.tan" => make_math_fn("mux_math_tan", FLOAT_PARAM),
            "math.asin" => make_math_fn("mux_math_asin", FLOAT_PARAM),
            "math.acos" => make_math_fn("mux_math_acos", FLOAT_PARAM),
            "math.atan" => make_math_fn("mux_math_atan", FLOAT_PARAM),
            "math.ln" => make_math_fn("mux_math_ln", FLOAT_PARAM),
            "math.log2" => make_math_fn("mux_math_log2", FLOAT_PARAM),
            "math.log10" => make_math_fn("mux_math_log10", FLOAT_PARAM),
            "math.exp" => make_math_fn("mux_math_exp", FLOAT_PARAM),
            "math.abs" => make_math_fn("mux_math_abs", FLOAT_PARAM),
            "math.floor" => make_math_fn("mux_math_floor", FLOAT_PARAM),
            "math.ceil" => make_math_fn("mux_math_ceil", FLOAT_PARAM),
            "math.round" => make_math_fn("mux_math_round", FLOAT_PARAM),
            "math.atan2" => make_math_fn("mux_math_atan2", FLOAT_FLOAT_PARAMS),
            "math.log" => make_math_fn("mux_math_log", FLOAT_FLOAT_PARAMS),
            "math.min" => make_math_fn("mux_math_min", FLOAT_FLOAT_PARAMS),
            "math.max" => make_math_fn("mux_math_max", FLOAT_FLOAT_PARAMS),
            "math.hypot" => make_math_fn("mux_math_hypot", FLOAT_FLOAT_PARAMS),
            "math.pow" => make_math_fn("mux_math_pow", FLOAT_FLOAT_PARAMS)
        }
        m
    };
    pub static ref DATETIME_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        insert_items! {
            m;
            "datetime.now" => datetime_fn("mux_datetime_now", EMPTY_PARAMS, io_result(int())),
            "datetime.now_millis" => datetime_fn("mux_datetime_now_millis", EMPTY_PARAMS, io_result(int())),
            "datetime.year" => datetime_fn("mux_datetime_year", INT_PARAM, io_result(int())),
            "datetime.month" => datetime_fn("mux_datetime_month", INT_PARAM, io_result(int())),
            "datetime.day" => datetime_fn("mux_datetime_day", INT_PARAM, io_result(int())),
            "datetime.hour" => datetime_fn("mux_datetime_hour", INT_PARAM, io_result(int())),
            "datetime.minute" => datetime_fn("mux_datetime_minute", INT_PARAM, io_result(int())),
            "datetime.second" => datetime_fn("mux_datetime_second", INT_PARAM, io_result(int())),
            "datetime.weekday" => datetime_fn("mux_datetime_weekday", INT_PARAM, io_result(int())),
            "datetime.format" => datetime_fn("mux_datetime_format", INT_STR_PARAMS, io_result(str_())),
            "datetime.format_local" => datetime_fn("mux_datetime_format_local", INT_STR_PARAMS, io_result(str_())),
            "datetime.sleep" => datetime_fn("mux_datetime_sleep", INT_PARAM, io_result(Type::Void)),
            "datetime.sleep_millis" => datetime_fn("mux_datetime_sleep_millis", INT_PARAM, io_result(Type::Void))
        }
        m
    };
    pub static ref ASSERT_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        fn make_item(llvm_name: &'static str, params: &[Type]) -> StdlibItem {
            StdlibItem::Function {
                params: params.to_vec(),
                ret: Type::Void,
                llvm_name: llvm_name.to_string(),
            }
        }
        let mut m = HashMap::new();
        insert_items! {
            m;
            "assert.assert" => make_item("mux_assert_assert", BOOL_STR_PARAMS),
            "assert.assert_eq" => make_item("mux_assert_eq", &T_T_PARAMS),
            "assert.assert_ne" => make_item("mux_assert_ne", &T_T_PARAMS),
            "assert.assert_true" => make_item("mux_assert_true", BOOL_PARAM),
            "assert.assert_false" => make_item("mux_assert_false", BOOL_PARAM),
            "assert.assert_some" => make_item("mux_assert_some", &OPTIONAL_T_PARAM),
            "assert.assert_none" => make_item("mux_assert_none", &OPTIONAL_T_PARAM),
            "assert.assert_ok" => make_item("mux_assert_ok", &RESULT_T_E_PARAMS),
            "assert.assert_err" => make_item("mux_assert_err", &RESULT_T_E_PARAMS)
        }
        m
    };
    pub static ref SYNC_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        fn spawn_fn() -> StdlibItem {
            StdlibItem::Function {
                params: vec![Type::Function {
                    params: Vec::new(),
                    returns: Box::new(Type::Void),
                    default_count: 0,
                }],
                ret: Type::Result(
                    Box::new(Type::Named("Thread".to_string(), Vec::new())),
                    Box::new(str_()),
                ),
                llvm_name: "mux_sync_spawn".to_string(),
            }
        }
        fn sleep_fn() -> StdlibItem {
            StdlibItem::Function {
                params: INT_PARAM.to_vec(),
                ret: Type::Void,
                llvm_name: "mux_sync_sleep".to_string(),
            }
        }
        let mut m = HashMap::new();
        insert_items! {
            m;
            "sync.spawn" => spawn_fn(),
            "sync.sleep" => sleep_fn()
        }
        m
    };
    pub static ref ENV_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        // env.get :: Str -> Optional(Str)
        m.insert(
            "env.get",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: Type::Optional(Box::new(str_())),
                llvm_name: "mux_env_get".to_string(),
            },
        );
        m
    };
    pub static ref NET_HTTP_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        m.insert(
            "net.http.request",
            StdlibItem::Function {
                params: vec![json_type()],
                ret: io_result(json_type()),
                llvm_name: "mux_net_http_request".to_string(),
            },
        );
        m.insert(
            "net.http.read_request",
            StdlibItem::Function {
                params: vec![tcp_stream_type()],
                ret: io_result(json_type()),
                llvm_name: "mux_net_http_read_request".to_string(),
            },
        );
        m.insert(
            "net.http.write_response",
            StdlibItem::Function {
                params: vec![tcp_stream_type(), json_type()],
                ret: io_result(Type::Void),
                llvm_name: "mux_net_http_write_response".to_string(),
            },
        );
        m
    };
    pub static ref DATA_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        // json.parse :: Str -> Result(Json, Str)
        m.insert(
            "json.parse",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: Type::Result(Box::new(Type::Named("Json".to_string(), Vec::new())), Box::new(str_())),
                llvm_name: "mux_json_parse".to_string(),
            },
        );
        // json.from_map :: Map(Str, T) -> Result(Json, Str)
        m.insert(
            "json.from_map",
            StdlibItem::Function {
                params: vec![Type::Map(Box::new(str_()), Box::new(Type::Variable("T".to_string())))],
                ret: Type::Result(Box::new(Type::Named("Json".to_string(), Vec::new())), Box::new(str_())),
                llvm_name: "mux_json_from_map".to_string(),
            },
        );
        // json.to_map :: Json -> Result(Map(Str, Json), Str)
        m.insert(
            "json.to_map",
            StdlibItem::Function {
                params: vec![Type::Named("Json".to_string(), Vec::new())],
                ret: Type::Result(
                    Box::new(Type::Map(Box::new(str_()), Box::new(Type::Named("Json".to_string(), Vec::new())))),
                    Box::new(str_()),
                ),
                llvm_name: "mux_json_to_map".to_string(),
            },
        );
        // csv.parse :: Str -> Result(Csv, Str)
        m.insert(
            "csv.parse",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: Type::Result(Box::new(Type::Named("Csv".to_string(), Vec::new())), Box::new(str_())),
                llvm_name: "mux_csv_parse".to_string(),
            },
        );
        // csv.parse_with_headers :: Str -> Result(Csv, Str)
        m.insert(
            "csv.parse_with_headers",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: Type::Result(Box::new(Type::Named("Csv".to_string(), Vec::new())), Box::new(str_())),
                llvm_name: "mux_csv_parse_with_headers".to_string(),
            },
        );
        m
    };
    pub static ref SQL_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        m.insert(
            "sql.connect",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: io_result(sql_connection_type()),
                llvm_name: "mux_sql_connect".to_string(),
            },
        );
        m.insert(
            "sql.int",
            StdlibItem::Function {
                params: INT_PARAM.to_vec(),
                ret: sql_value_type(),
                llvm_name: "mux_sql_value_int".to_string(),
            },
        );
        m.insert(
            "sql.float",
            StdlibItem::Function {
                params: FLOAT_PARAM.to_vec(),
                ret: sql_value_type(),
                llvm_name: "mux_sql_value_float".to_string(),
            },
        );
        m.insert(
            "sql.bool",
            StdlibItem::Function {
                params: BOOL_PARAM.to_vec(),
                ret: sql_value_type(),
                llvm_name: "mux_sql_value_bool".to_string(),
            },
        );
        m.insert(
            "sql.string",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: sql_value_type(),
                llvm_name: "mux_sql_value_string".to_string(),
            },
        );
        m.insert(
            "sql.bytes",
            StdlibItem::Function {
                params: vec![Type::List(Box::new(int()))],
                ret: sql_value_type(),
                llvm_name: "mux_sql_value_bytes".to_string(),
            },
        );
        m.insert(
            "sql.null",
            StdlibItem::Function {
                params: EMPTY_PARAMS.to_vec(),
                ret: sql_value_type(),
                llvm_name: "mux_sql_value_null".to_string(),
            },
        );
        m
    };
    pub static ref BUILT_IN_FUNCTIONS: HashMap<&'static str, BuiltInSig> = {
        let mut m = HashMap::new();
        m.insert("int_to_string", sig(vec![int()], str_()));
        m.insert("int_to_float", sig(vec![int()], float()));
        register_batch(
            &mut m,
            &["int_add", "int_sub", "int_mul", "int_div", "int_rem"],
            sig(vec![int(), int()], int()),
        );
        register_batch(
            &mut m,
            &["int_eq", "int_lt"],
            sig(vec![int(), int()], bool_()),
        );
        m.insert("float_to_string", sig(vec![float()], str_()));
        m.insert("float_to_int", sig(vec![float()], int()));
        m.insert("float_add", sig(vec![float(), float()], float()));
        m.insert("string_to_int", sig(vec![str_()], int()));
        m.insert("string_to_float", sig(vec![str_()], float()));
        m.insert("string_concat", sig(vec![str_(), str_()], str_()));
        m.insert("string_length", sig(vec![str_()], int()));
        m.insert("bool_to_string", sig(vec![bool_()], str_()));
        m.insert("bool_to_int", sig(vec![bool_()], int()));
        m.insert("print", sig(vec![str_()], Type::Void));
        m.insert("read_line", sig(vec![], str_()));
        m.insert(
            "range",
            sig(vec![int(), int()], Type::List(Box::new(int()))),
        );
        m.insert(
            "some",
            sig(
                vec![Type::Variable("T".to_string())],
                Type::Optional(Box::new(Type::Variable("T".to_string()))),
            ),
        );
        m.insert("none", sig(vec![], Type::Optional(Box::new(Type::Void))));
        m.insert(
            "ok",
            sig(
                vec![Type::Variable("T".to_string())],
                Type::Result(
                    Box::new(Type::Variable("T".to_string())),
                    Box::new(Type::Variable("E".to_string())),
                ),
            ),
        );
        m.insert(
            "err",
            sig(
                vec![Type::Variable("E".to_string())],
                Type::Result(
                    Box::new(Type::Variable("T".to_string())),
                    Box::new(Type::Variable("E".to_string())),
                ),
            ),
        );
        m
    };
}

pub fn all_stdlib_items() -> impl Iterator<Item = (String, StdlibItem)> {
    STDLIB_ITEMS
        .entries()
        .map(|(key, item)| (key.to_string(), materialize_stdlib_item(item)))
        .chain(
            IO_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            MATH_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            DATETIME_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            ASSERT_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            SYNC_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            ENV_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            NET_HTTP_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            DATA_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            SQL_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
}

#[must_use]
pub fn lookup_stdlib_item(name: &str) -> Option<StdlibItem> {
    if let Some(item) = STDLIB_ITEMS.get(name) {
        return Some(materialize_stdlib_item(item));
    }
    IO_STDLIB_ITEMS
        .get(name)
        .cloned()
        .or_else(|| MATH_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| DATETIME_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| ASSERT_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| SYNC_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| ENV_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| NET_HTTP_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| DATA_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| SQL_STDLIB_ITEMS.get(name).cloned())
}

/// Convert a canonical `StdlibItem` into a `Symbol` suitable for registration in a SymbolTable.
#[must_use]
pub fn stdlib_item_to_symbol(item: &StdlibItem, span: Span) -> Symbol {
    match item {
        StdlibItem::Function {
            params,
            ret,
            llvm_name,
        } => Symbol {
            kind: SymbolKind::Function,
            span,
            type_: Some(Type::Function {
                params: params.to_vec(),
                returns: Box::new(ret.clone()),
                default_count: 0,
            }),
            interfaces: std::collections::HashMap::new(),
            methods: std::collections::HashMap::new(),
            fields: std::collections::HashMap::new(),
            type_params: Vec::new(),
            original_name: None,
            llvm_name: Some(llvm_name.to_string()),
            default_param_count: 0,
            variants: None,
        },
        StdlibItem::Constant { ty, .. } => Symbol {
            kind: SymbolKind::Constant,
            span,
            type_: Some(ty.clone()),
            interfaces: std::collections::HashMap::new(),
            methods: std::collections::HashMap::new(),
            fields: std::collections::HashMap::new(),
            type_params: Vec::new(),
            original_name: None,
            llvm_name: None,
            default_param_count: 0,
            variants: None,
        },
    }
}
