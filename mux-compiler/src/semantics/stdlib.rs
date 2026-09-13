//! Canonical stdlib registry
//!
//! This module centralizes the standard library (stdlib) description used by the
//! semantic analyzer and codegen. It provides:
//! - `StdlibItem`: canonical representation for stdlib functions and constants
//! - `lookup_stdlib_item` / `all_stdlib_items`: accessors for items
//! - `BUILT_IN_FUNCTIONS`: built-in function signatures used for name resolution
//! - `*_STDLIB_ITEMS` tables for per-module items (IO, MATH, DATETIME, SYNC)
//! - `net_module_class_symbols` / `sync_module_class_symbols`: class symbol builders
//! - `stdlib_item_to_symbol` / `register_stdlib_item_into`: helpers to convert and
//!   register stdlib items into the compiler's `SymbolTable`.
//!
//! Contributing
//! - Add new stdlib items by updating the appropriate table (`MATH_STDLIB_ITEMS`,
//!   `IO_STDLIB_ITEMS`, or `STDLIB_ITEMS` for PHF-backed descriptors).
//! - Prefer adding entries to the module-specific `HashMaps` rather than spreading
//!   duplicates across the codebase: these are the single source of truth.
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
fn json_duplicate_policy_type() -> Type {
    Type::Named("JsonDuplicatePolicy".to_string(), Vec::new())
}
fn json_token_reader_type() -> Type {
    Type::Named("JsonTokenReader".to_string(), Vec::new())
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
/// Owns its data to avoid `Box::leak`
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
static FLOAT_FLOAT_FLOAT_PARAMS: &[Type] = &[
    Type::Primitive(PrimitiveType::Float),
    Type::Primitive(PrimitiveType::Float),
    Type::Primitive(PrimitiveType::Float),
];
static INT_INT_PARAMS: &[Type] = &[
    Type::Primitive(PrimitiveType::Int),
    Type::Primitive(PrimitiveType::Int),
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
static STR_INT_INT_BOOL_BOOL_BOOL_PARAMS: &[Type] = &[
    Type::Primitive(PrimitiveType::Str),
    Type::Primitive(PrimitiveType::Int),
    Type::Primitive(PrimitiveType::Int),
    Type::Primitive(PrimitiveType::Bool),
    Type::Primitive(PrimitiveType::Bool),
    Type::Primitive(PrimitiveType::Bool),
];
static EMPTY_PARAMS: &[Type] = &[];

// Lazy static arrays for generic stdlib functions (to avoid Box::leak)
lazy_static! {
    static ref LIST_T_PARAM: Vec<Type> =
        vec![Type::List(Box::new(Type::Variable("T".to_string())))];
    static ref LIST_T_INT_PARAMS: Vec<Type> =
        vec![Type::List(Box::new(Type::Variable("T".to_string()))), int(),];
    static ref LIST_T_LIST_FLOAT_PARAMS: Vec<Type> = vec![
        Type::List(Box::new(Type::Variable("T".to_string()))),
        Type::List(Box::new(float())),
    ];
    static ref LIST_FLOAT_PARAMS: Vec<Type> = vec![Type::List(Box::new(float()))];
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

fn bytes_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(bytes_error_type()))
}

fn json_error_type() -> Type {
    Type::Named("JsonError".to_string(), Vec::new())
}

fn json_error_kind_type() -> Type {
    Type::Named("JsonErrorKind".to_string(), Vec::new())
}

fn csv_error_type() -> Type {
    Type::Named("CsvError".to_string(), Vec::new())
}

fn csv_error_kind_type() -> Type {
    Type::Named("CsvErrorKind".to_string(), Vec::new())
}

fn byte_error_type() -> Type {
    Type::Named("ByteError".to_string(), Vec::new())
}

fn byte_error_kind_type() -> Type {
    Type::Named("ByteErrorKind".to_string(), Vec::new())
}

fn byte_error_methods() -> HashMap<String, MethodSig> {
    HashMap::from([
        (
            "from_message".to_string(),
            MethodSig {
                params: vec![str_()],
                return_type: byte_error_type(),
                is_static: true,
            },
        ),
        (
            "message".to_string(),
            MethodSig {
                params: vec![],
                return_type: str_(),
                is_static: false,
            },
        ),
        (
            "to_string".to_string(),
            MethodSig {
                params: vec![],
                return_type: str_(),
                is_static: false,
            },
        ),
    ])
}

fn byte_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (byte_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

#[must_use]
pub fn byte_error_builtin_symbol(span: Span) -> Symbol {
    make_error_class_symbol_with_fields(
        "ByteError",
        byte_error_methods(),
        byte_error_fields(),
        span,
    )
}

fn bytes_error_type() -> Type {
    Type::Named("BytesError".to_string(), Vec::new())
}

fn bytes_error_kind_type() -> Type {
    Type::Named("BytesErrorKind".to_string(), Vec::new())
}

fn bytes_error_methods() -> HashMap<String, MethodSig> {
    HashMap::from([
        (
            "from_message".to_string(),
            MethodSig {
                params: vec![str_()],
                return_type: bytes_error_type(),
                is_static: true,
            },
        ),
        (
            "message".to_string(),
            MethodSig {
                params: vec![],
                return_type: str_(),
                is_static: false,
            },
        ),
        (
            "to_string".to_string(),
            MethodSig {
                params: vec![],
                return_type: str_(),
                is_static: false,
            },
        ),
    ])
}

fn bytes_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (bytes_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

#[must_use]
pub fn bytes_error_builtin_symbol(span: Span) -> Symbol {
    make_error_class_symbol_with_fields(
        "BytesError",
        bytes_error_methods(),
        bytes_error_fields(),
        span,
    )
}

fn json_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(json_error_type()))
}

pub(crate) fn json_error_methods() -> HashMap<String, MethodSig> {
    HashMap::from([
        (
            "from_message".to_string(),
            MethodSig {
                params: vec![str_()],
                return_type: json_error_type(),
                is_static: true,
            },
        ),
        (
            "message".to_string(),
            MethodSig {
                params: vec![],
                return_type: str_(),
                is_static: false,
            },
        ),
        (
            "to_string".to_string(),
            MethodSig {
                params: vec![],
                return_type: str_(),
                is_static: false,
            },
        ),
    ])
}

pub(crate) fn csv_error_methods() -> HashMap<String, MethodSig> {
    HashMap::from([
        (
            "from_message".to_string(),
            MethodSig {
                params: vec![str_()],
                return_type: csv_error_type(),
                is_static: true,
            },
        ),
        (
            "message".to_string(),
            MethodSig {
                params: vec![],
                return_type: str_(),
                is_static: false,
            },
        ),
        (
            "to_string".to_string(),
            MethodSig {
                params: vec![],
                return_type: str_(),
                is_static: false,
            },
        ),
    ])
}

pub(crate) fn json_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (json_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

pub(crate) fn csv_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (csv_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn csv_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(csv_error_type()))
}

fn io_error_type() -> Type {
    Type::Named("IoError".to_string(), Vec::new())
}

fn io_error_kind_type() -> Type {
    Type::Named("IoErrorKind".to_string(), Vec::new())
}

fn sync_error_type() -> Type {
    Type::Named("SyncError".to_string(), Vec::new())
}

fn sync_error_kind_type() -> Type {
    Type::Named("SyncErrorKind".to_string(), Vec::new())
}

fn sync_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(sync_error_type()))
}

fn stream_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(io_error_type()))
}

fn http_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(http_error_type()))
}

fn sql_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(sql_error_type()))
}

fn net_bytes_type() -> Type {
    Type::Primitive(PrimitiveType::Bytes)
}
fn tcp_stream_type() -> Type {
    Type::Named("TcpStream".to_string(), Vec::new())
}
fn udp_socket_type() -> Type {
    Type::Named("UdpSocket".to_string(), Vec::new())
}

fn udp_datagram_type() -> Type {
    Type::Named("UdpDatagram".to_string(), Vec::new())
}
fn tcp_listener_type() -> Type {
    Type::Named("TcpListener".to_string(), Vec::new())
}
fn local_stream_type() -> Type {
    Type::Named("LocalStream".to_string(), Vec::new())
}
fn local_listener_type() -> Type {
    Type::Named("LocalListener".to_string(), Vec::new())
}
fn ip_addr_type() -> Type {
    Type::Named("IpAddr".to_string(), Vec::new())
}
fn socket_addr_type() -> Type {
    Type::Named("SocketAddr".to_string(), Vec::new())
}
fn cidr_type() -> Type {
    Type::Named("Cidr".to_string(), Vec::new())
}
fn endpoint_type() -> Type {
    Type::Named("Endpoint".to_string(), Vec::new())
}
fn poller_type() -> Type {
    Type::Named("Poller".to_string(), Vec::new())
}
fn poll_event_type() -> Type {
    Type::Named("PollEvent".to_string(), Vec::new())
}
fn tls_stream_type() -> Type {
    Type::Named("TlsStream".to_string(), Vec::new())
}
fn tls_config_type() -> Type {
    Type::Named("TlsConfig".to_string(), Vec::new())
}

fn tls_error_type() -> Type {
    Type::Named("TlsError".to_string(), Vec::new())
}

fn tls_error_kind_type() -> Type {
    Type::Named("TlsErrorKind".to_string(), Vec::new())
}

fn tls_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(tls_error_type()))
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
fn sql_prepared_type() -> Type {
    Type::Named("PreparedStatement".to_string(), Vec::new())
}
fn sql_pool_type() -> Type {
    Type::Named("Pool".to_string(), Vec::new())
}

fn sql_cancellation_token_type() -> Type {
    Type::Named("CancellationToken".to_string(), Vec::new())
}

fn sql_migration_type() -> Type {
    Type::Named("Migration".to_string(), Vec::new())
}

fn sql_migrator_type() -> Type {
    Type::Named("Migrator".to_string(), Vec::new())
}
fn sql_row_type() -> Type {
    Type::Named("Row".to_string(), Vec::new())
}

fn sql_error_type() -> Type {
    Type::Named("SqlError".to_string(), Vec::new())
}

fn sql_error_kind_type() -> Type {
    Type::Named("SqlErrorKind".to_string(), Vec::new())
}
fn process_command_type() -> Type {
    Type::Named("Command".to_string(), Vec::new())
}
fn process_child_type() -> Type {
    Type::Named("Child".to_string(), Vec::new())
}
fn process_output_type() -> Type {
    Type::Named("Output".to_string(), Vec::new())
}

fn process_pool_type() -> Type {
    Type::Named("ProcessPool".to_string(), Vec::new())
}
fn process_error_type() -> Type {
    Type::Named("ProcessError".to_string(), Vec::new())
}

fn process_error_kind_type() -> Type {
    Type::Named("ProcessErrorKind".to_string(), Vec::new())
}

fn process_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(process_error_type()))
}
fn regex_type() -> Type {
    Type::Named("Regex".to_string(), Vec::new())
}
fn regex_match_type() -> Type {
    Type::Named("RegexMatch".to_string(), Vec::new())
}

fn uuid_type() -> Type {
    Type::Named("Uuid".to_string(), Vec::new())
}

fn url_type() -> Type {
    Type::Named("Url".to_string(), Vec::new())
}

fn http_headers_type() -> Type {
    Type::Named("Headers".to_string(), Vec::new())
}

fn http_request_type() -> Type {
    Type::Named("HttpRequest".to_string(), Vec::new())
}

fn http_response_type() -> Type {
    Type::Named("HttpResponse".to_string(), Vec::new())
}

fn http_server_config_type() -> Type {
    Type::Named("HttpServerConfig".to_string(), Vec::new())
}

fn http_router_type() -> Type {
    Type::Named("HttpRouter".to_string(), Vec::new())
}

fn oauth_client_type() -> Type {
    Type::Named("OAuthClient".to_string(), Vec::new())
}

fn oauth_session_type() -> Type {
    Type::Named("OAuthSession".to_string(), Vec::new())
}

fn http_next_type() -> Type {
    Type::Named("HttpNext".to_string(), Vec::new())
}

fn sse_event_type() -> Type {
    Type::Named("SseEvent".to_string(), Vec::new())
}

fn websocket_frame_type() -> Type {
    Type::Named("WebSocketFrame".to_string(), Vec::new())
}

fn websocket_handshake_type() -> Type {
    Type::Named("WebSocketHandshake".to_string(), Vec::new())
}

fn sse_stream_type() -> Type {
    Type::Named("SseStream".to_string(), Vec::new())
}

fn websocket_session_type() -> Type {
    Type::Named("WebSocketSession".to_string(), Vec::new())
}

fn http_handler_type() -> Type {
    Type::Function {
        params: vec![http_request_type()],
        returns: Box::new(http_result(http_response_type())),
        default_count: 0,
    }
}

fn http_middleware_type() -> Type {
    Type::Function {
        params: vec![http_request_type(), http_next_type()],
        returns: Box::new(http_result(http_response_type())),
        default_count: 0,
    }
}

fn http_error_type() -> Type {
    Type::Named("HttpError".to_string(), Vec::new())
}

fn http_error_kind_type() -> Type {
    Type::Named("HttpErrorKind".to_string(), Vec::new())
}

fn env_error_type() -> Type {
    Type::Named("EnvError".to_string(), Vec::new())
}

fn env_error_kind_type() -> Type {
    Type::Named("EnvErrorKind".to_string(), Vec::new())
}

fn fs_error_type() -> Type {
    Type::Named("FsError".to_string(), Vec::new())
}

fn fs_error_kind_type() -> Type {
    Type::Named("FsErrorKind".to_string(), Vec::new())
}

fn fs_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(fs_error_type()))
}

fn net_error_type() -> Type {
    Type::Named("NetError".to_string(), Vec::new())
}

fn net_error_kind_type() -> Type {
    Type::Named("NetErrorKind".to_string(), Vec::new())
}

fn net_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(net_error_type()))
}

fn url_error_type() -> Type {
    Type::Named("UrlError".to_string(), Vec::new())
}

fn url_error_kind_type() -> Type {
    Type::Named("UrlErrorKind".to_string(), Vec::new())
}

fn url_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(url_error_type()))
}

fn uuid_error_type() -> Type {
    Type::Named("UuidError".to_string(), Vec::new())
}

fn uuid_error_kind_type() -> Type {
    Type::Named("UuidErrorKind".to_string(), Vec::new())
}

fn uuid_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(uuid_error_type()))
}

fn log_error_type() -> Type {
    Type::Named("LogError".to_string(), Vec::new())
}

fn log_error_kind_type() -> Type {
    Type::Named("LogErrorKind".to_string(), Vec::new())
}

fn log_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(log_error_type()))
}

fn random_error_type() -> Type {
    Type::Named("RandomError".to_string(), Vec::new())
}

fn random_error_kind_type() -> Type {
    Type::Named("RandomErrorKind".to_string(), Vec::new())
}

fn random_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(random_error_type()))
}

fn crypto_error_type() -> Type {
    Type::Named("CryptoError".to_string(), Vec::new())
}

fn crypto_error_kind_type() -> Type {
    Type::Named("CryptoErrorKind".to_string(), Vec::new())
}

fn crypto_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(crypto_error_type()))
}

fn regex_error_type() -> Type {
    Type::Named("RegexError".to_string(), Vec::new())
}

fn regex_error_kind_type() -> Type {
    Type::Named("RegexErrorKind".to_string(), Vec::new())
}

fn regex_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(regex_error_type()))
}

fn math_error_type() -> Type {
    Type::Named("MathError".to_string(), Vec::new())
}

fn math_error_kind_type() -> Type {
    Type::Named("MathErrorKind".to_string(), Vec::new())
}

fn math_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(math_error_type()))
}

fn env_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(env_error_type()))
}

fn logger_type() -> Type {
    Type::Named("Logger".to_string(), Vec::new())
}

fn random_type() -> Type {
    Type::Named("Random".to_string(), Vec::new())
}

fn reader_type() -> Type {
    Type::Named("Reader".to_string(), Vec::new())
}

fn writer_type() -> Type {
    Type::Named("Writer".to_string(), Vec::new())
}

fn stream_type() -> Type {
    Type::Named("Stream".to_string(), Vec::new())
}

fn cli_parser_type() -> Type {
    Type::Named("CliParser".to_string(), Vec::new())
}

fn cli_matches_type() -> Type {
    Type::Named("CliMatches".to_string(), Vec::new())
}

fn cli_error_type() -> Type {
    Type::Named("CliError".to_string(), Vec::new())
}

fn cli_error_kind_type() -> Type {
    Type::Named("CliErrorKind".to_string(), Vec::new())
}

fn cli_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(cli_error_type()))
}

fn path_type() -> Type {
    Type::Named("Path".to_string(), Vec::new())
}

fn directory_type() -> Type {
    Type::Named("Directory".to_string(), Vec::new())
}

fn date_type() -> Type {
    Type::Named("Date".to_string(), Vec::new())
}

fn time_type() -> Type {
    Type::Named("Time".to_string(), Vec::new())
}

fn datetime_type() -> Type {
    Type::Named("DateTime".to_string(), Vec::new())
}

fn datetime_error_type() -> Type {
    Type::Named("DateTimeError".to_string(), Vec::new())
}

fn datetime_error_kind_type() -> Type {
    Type::Named("DateTimeErrorKind".to_string(), Vec::new())
}

fn datetime_result(ok: Type) -> Type {
    Type::Result(Box::new(ok), Box::new(datetime_error_type()))
}

fn zoned_datetime_type() -> Type {
    Type::Named("ZonedDateTime".to_string(), Vec::new())
}

fn local_resolution_type() -> Type {
    Type::Named("LocalResolution".to_string(), Vec::new())
}

fn instant_type() -> Type {
    Type::Named("Instant".to_string(), Vec::new())
}

fn duration_type() -> Type {
    Type::Named("Duration".to_string(), Vec::new())
}

fn period_type() -> Type {
    Type::Named("Period".to_string(), Vec::new())
}

fn make_class_symbol(name: &str, methods: HashMap<String, MethodSig>, span: Span) -> Symbol {
    make_class_symbol_with_fields(name, methods, HashMap::new(), span)
}

fn make_interface_symbol(name: &str, methods: HashMap<String, MethodSig>, span: Span) -> Symbol {
    let mut interfaces = HashMap::new();
    interfaces.insert(name.to_string(), (Vec::new(), methods));
    Symbol {
        kind: SymbolKind::Interface,
        span,
        type_: None,
        interfaces,
        methods: HashMap::new(),
        fields: HashMap::new(),
        type_params: Vec::new(),
        original_name: None,
        llvm_name: None,
        default_param_count: 0,
        variants: None,
    }
}

/// The opt-in serialization contracts exposed by `std.data.json` and
/// `std.data.csv`. These are interfaces rather than compiler magic: a class
/// implements them only when it declares the matching method, and the method
/// body is always supplied by the program.
pub fn json_representable_interface_symbol(span: Span) -> Symbol {
    make_interface_symbol(
        "JsonRepresentable",
        HashMap::from([(
            "to_json".to_string(),
            MethodSig {
                params: Vec::new(),
                return_type: Type::Result(Box::new(json_type()), Box::new(json_error_type())),
                is_static: false,
            },
        )]),
        span,
    )
}

pub fn csv_representable_interface_symbol(span: Span) -> Symbol {
    make_interface_symbol(
        "CsvRepresentable",
        HashMap::from([(
            "to_csv".to_string(),
            MethodSig {
                params: Vec::new(),
                return_type: Type::Result(Box::new(str_()), Box::new(csv_error_type())),
                is_static: false,
            },
        )]),
        span,
    )
}

fn make_generic_class_symbol(
    name: &str,
    type_param: &str,
    methods: HashMap<String, MethodSig>,
    span: Span,
) -> Symbol {
    let mut symbol = make_class_symbol(name, methods, span);
    symbol.type_ = Some(Type::Named(
        name.to_string(),
        vec![Type::Variable(type_param.to_string())],
    ));
    symbol
        .type_params
        .push((type_param.to_string(), Vec::new()));
    symbol
}

fn make_class_symbol_with_fields(
    name: &str,
    methods: HashMap<String, MethodSig>,
    fields: HashMap<String, (Type, bool)>,
    span: Span,
) -> Symbol {
    Symbol {
        kind: SymbolKind::Class,
        span,
        type_: Some(Type::Named(name.to_string(), Vec::new())),
        interfaces: HashMap::new(),
        methods,
        fields,
        type_params: Vec::new(),
        original_name: None,
        llvm_name: None,
        default_param_count: 0,
        variants: None,
    }
}

/// Build a payload-less enum symbol for a runtime-backed stdlib type.
///
/// Runtime-backed modules do not have a Mux source AST from which the normal
/// declaration pass can collect enum variants, but they still need the same
/// semantic shape as a user-declared enum so callers can construct and match
/// the value. Codegen materializes the corresponding layout and constructors.
pub(crate) fn make_enum_symbol(name: &str, variants: &[&str], span: Span) -> Symbol {
    let return_type = Type::Named(name.to_string(), Vec::new());
    let methods = variants
        .iter()
        .map(|variant| {
            (
                (*variant).to_string(),
                MethodSig {
                    params: Vec::new(),
                    return_type: return_type.clone(),
                    is_static: true,
                },
            )
        })
        .collect();
    Symbol {
        kind: SymbolKind::Enum,
        span,
        type_: Some(return_type),
        interfaces: HashMap::new(),
        methods,
        fields: HashMap::new(),
        type_params: Vec::new(),
        original_name: None,
        llvm_name: None,
        default_param_count: 0,
        variants: Some(
            variants
                .iter()
                .map(|variant| (*variant).to_string())
                .collect(),
        ),
    }
}

pub(crate) fn make_error_class_symbol_with_fields(
    name: &str,
    methods: HashMap<String, MethodSig>,
    fields: HashMap<String, (Type, bool)>,
    span: Span,
) -> Symbol {
    let mut symbol = make_class_symbol_with_fields(name, methods, fields, span);
    symbol.interfaces.insert(
        "Error".to_string(),
        (
            Vec::new(),
            HashMap::from([(
                "message".to_string(),
                MethodSig {
                    params: Vec::new(),
                    return_type: str_(),
                    is_static: false,
                },
            )]),
        ),
    );
    symbol
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

fn bytes_cursor_methods() -> HashMap<String, MethodSig> {
    let mut methods = HashMap::new();
    let mut add = |name: &str, params: Vec<Type>, return_type: Type| {
        methods.insert(
            name.to_string(),
            MethodSig {
                params,
                return_type,
                is_static: false,
            },
        );
    };
    add("position", vec![], bytes_result(int()));
    add("remaining", vec![], bytes_result(int()));
    add(
        "read_bytes",
        vec![int()],
        bytes_result(Type::Primitive(PrimitiveType::Bytes)),
    );
    for name in ["read_uint_le", "read_uint_be"] {
        add(name, vec![int()], bytes_result(int()));
    }
    for name in ["write_uint_le", "write_uint_be"] {
        add(name, vec![int(), int()], bytes_result(Type::Void));
    }
    for name in ["read_float_le", "read_float_be"] {
        add(name, vec![], bytes_result(float()));
    }
    for name in ["write_float_le", "write_float_be"] {
        add(name, vec![float()], bytes_result(Type::Void));
    }
    add(
        "into_bytes",
        vec![],
        bytes_result(Type::Primitive(PrimitiveType::Bytes)),
    );
    methods
}

#[must_use]
pub fn bytes_cursor_builtin_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut symbols = HashMap::new();
    symbols.insert(
        "BytesCursor".to_string(),
        make_class_symbol("BytesCursor", bytes_cursor_methods(), span),
    );
    symbols
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
            return_type: net_result(tcp_stream_type()),
            is_static: true
        },
        "read" => {
            params: [int()],
            return_type: stream_result(net_bytes_type()),
            is_static: false
        },
        "write" => {
            params: [net_bytes_type()],
            return_type: stream_result(int()),
            is_static: false
        }
    };
    methods.extend(define_methods! {
        "shutdown_read" => {
            params: [],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "shutdown_write" => {
            params: [],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "set_nodelay" => {
            params: [bool_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "nodelay" => {
            params: [],
            return_type: net_result(bool_()),
            is_static: false
        },
        "set_keepalive" => {
            params: [bool_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "keepalive" => {
            params: [],
            return_type: net_result(bool_()),
            is_static: false
        }
    });
    insert_socket_common_methods(&mut methods);
    methods
}

fn udp_socket_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "bind" => {
            params: [str_()],
            return_type: net_result(udp_socket_type()),
            is_static: true
        },
        "send_to" => {
            params: [net_bytes_type(), str_()],
            return_type: net_result(int()),
            is_static: false
        },
        "recv_from" => {
            params: [int()],
            return_type: net_result(udp_datagram_type()),
            is_static: false
        },
        "set_broadcast" => {
            params: [bool_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "broadcast" => {
            params: [],
            return_type: net_result(bool_()),
            is_static: false
        },
        "set_multicast_loop_v4" => {
            params: [bool_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "multicast_loop_v4" => {
            params: [],
            return_type: net_result(bool_()),
            is_static: false
        },
        "set_multicast_ttl_v4" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "multicast_ttl_v4" => {
            params: [],
            return_type: net_result(int()),
            is_static: false
        },
        "join_multicast_v4" => {
            params: [str_(), str_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "leave_multicast_v4" => {
            params: [str_(), str_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "set_multicast_loop_v6" => {
            params: [bool_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "multicast_loop_v6" => {
            params: [],
            return_type: net_result(bool_()),
            is_static: false
        },
        "set_multicast_hops_v6" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "multicast_hops_v6" => {
            params: [],
            return_type: net_result(int()),
            is_static: false
        },
        "join_multicast_v6" => {
            params: [str_(), int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "leave_multicast_v6" => {
            params: [str_(), int()],
            return_type: net_result(Type::Void),
            is_static: false
        }
    };
    insert_socket_common_methods(&mut methods);
    methods
}

fn udp_datagram_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "bytes" => { params: [], return_type: net_result(net_bytes_type()), is_static: false },
        "address" => { params: [], return_type: net_result(str_()), is_static: false },
        "truncated" => { params: [], return_type: net_result(bool_()), is_static: false }
    }
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
            return_type: net_result(Type::Void),
            is_static: false
        },
        "set_read_timeout" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "set_write_timeout" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "set_nonblocking" => {
            params: [bool_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "set_ttl" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "ttl" => {
            params: [],
            return_type: net_result(int()),
            is_static: false
        },
        "set_recv_buffer_size" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "recv_buffer_size" => {
            params: [],
            return_type: net_result(int()),
            is_static: false
        },
        "set_send_buffer_size" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "send_buffer_size" => {
            params: [],
            return_type: net_result(int()),
            is_static: false
        },
        "peer_addr" => {
            params: [],
            return_type: net_result(str_()),
            is_static: false
        },
        "local_addr" => {
            params: [],
            return_type: net_result(str_()),
            is_static: false
        }
    });
}

fn tcp_listener_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "bind" => {
            params: [str_()],
            return_type: net_result(tcp_listener_type()),
            is_static: true
        },
        "accept" => {
            params: [],
            return_type: net_result(tcp_stream_type()),
            is_static: false
        },
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        },
        "set_nonblocking" => {
            params: [bool_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "local_addr" => {
            params: [],
            return_type: net_result(str_()),
            is_static: false
        }
    }
}

fn local_stream_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "connect" => {
            params: [str_()],
            return_type: net_result(local_stream_type()),
            is_static: true
        },
        "read" => {
            params: [int()],
            return_type: stream_result(net_bytes_type()),
            is_static: false
        },
        "write" => {
            params: [net_bytes_type()],
            return_type: stream_result(int()),
            is_static: false
        },
        "set_read_timeout" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "set_write_timeout" => {
            params: [int()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "shutdown_read" => {
            params: [],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "shutdown_write" => {
            params: [],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        }
    }
}

fn local_listener_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "bind" => {
            params: [str_()],
            return_type: net_result(local_listener_type()),
            is_static: true
        },
        "accept" => {
            params: [],
            return_type: net_result(local_stream_type()),
            is_static: false
        },
        "set_nonblocking" => {
            params: [bool_()],
            return_type: net_result(Type::Void),
            is_static: false
        },
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        }
    }
}

fn ip_addr_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "parse" => { params: [str_()], return_type: net_result(ip_addr_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "is_v4" => { params: [], return_type: bool_(), is_static: false },
        "is_v6" => { params: [], return_type: bool_(), is_static: false },
        "octets" => { params: [], return_type: Type::Primitive(PrimitiveType::Bytes), is_static: false }
    }
}

fn socket_addr_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "parse" => { params: [str_()], return_type: net_result(socket_addr_type()), is_static: true },
        "resolve" => { params: [str_(), int()], return_type: net_result(Type::List(Box::new(socket_addr_type()))), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "port" => { params: [], return_type: int(), is_static: false },
        "ip" => { params: [], return_type: ip_addr_type(), is_static: false },
        "with_port" => { params: [int()], return_type: net_result(socket_addr_type()), is_static: false }
    }
}

fn cidr_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "parse" => { params: [str_()], return_type: net_result(cidr_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "prefix_len" => { params: [], return_type: int(), is_static: false },
        "network" => { params: [], return_type: ip_addr_type(), is_static: false },
        "contains" => { params: [ip_addr_type()], return_type: bool_(), is_static: false }
    }
}

fn endpoint_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_host" => { params: [str_(), int()], return_type: net_result(endpoint_type()), is_static: true },
        "from_socket_addr" => { params: [socket_addr_type()], return_type: net_result(endpoint_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "host" => { params: [], return_type: str_(), is_static: false },
        "port" => { params: [], return_type: int(), is_static: false },
        "resolve" => { params: [], return_type: net_result(Type::List(Box::new(socket_addr_type()))), is_static: false }
    }
}

fn poller_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: net_result(poller_type()), is_static: true },
        "register_tcp" => { params: [tcp_stream_type(), bool_(), bool_()], return_type: net_result(int()), is_static: false },
        "register_listener" => { params: [tcp_listener_type()], return_type: net_result(int()), is_static: false },
        "register_udp" => { params: [udp_socket_type(), bool_(), bool_()], return_type: net_result(int()), is_static: false },
        "deregister" => { params: [int()], return_type: net_result(Type::Void), is_static: false },
        "poll" => { params: [int()], return_type: net_result(Type::List(Box::new(poll_event_type()))), is_static: false }
    }
}

fn poll_event_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "token" => { params: [], return_type: net_result(int()), is_static: false },
        "readable" => { params: [], return_type: net_result(bool_()), is_static: false },
        "writable" => { params: [], return_type: net_result(bool_()), is_static: false },
        "error" => { params: [], return_type: net_result(bool_()), is_static: false },
        "closed" => { params: [], return_type: net_result(bool_()), is_static: false }
    }
}

fn process_command_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: process_result(process_command_type()),
            is_static: true
        },
        "set_program" => {
            params: [str_()],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "shell" => {
            params: [str_()],
            return_type: process_result(process_command_type()),
            is_static: true
        },
        "arg" => {
            params: [str_()],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "env" => {
            params: [str_(), str_()],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "cwd" => {
            params: [str_()],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "stdin_piped" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "stdout_piped" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "stderr_piped" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "stdin_null" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "stdout_null" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "stderr_null" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "output" => {
            params: [],
            return_type: process_result(process_output_type()),
            is_static: false
        },
        "status" => {
            params: [],
            return_type: process_result(int()),
            is_static: false
        },
        "spawn" => {
            params: [],
            return_type: process_result(process_child_type()),
            is_static: false
        }
    }
}

fn process_child_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "wait" => {
            params: [],
            return_type: process_result(int()),
            is_static: false
        },
        "try_wait" => {
            params: [],
            return_type: process_result(Type::Optional(Box::new(int()))),
            is_static: false
        },
        "wait_timeout" => {
            params: [int()],
            return_type: process_result(Type::Optional(Box::new(int()))),
            is_static: false
        },
        "write_stdin" => {
            params: [Type::Primitive(PrimitiveType::Bytes)],
            return_type: process_result(int()),
            is_static: false
        },
        "close_stdin" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "read_stdout" => {
            params: [int()],
            return_type: process_result(Type::Primitive(PrimitiveType::Bytes)),
            is_static: false
        },
        "read_stderr" => {
            params: [int()],
            return_type: process_result(Type::Primitive(PrimitiveType::Bytes)),
            is_static: false
        },
        "kill" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        },
        "kill_group" => {
            params: [],
            return_type: process_result(Type::Void),
            is_static: false
        }
    }
}

fn process_output_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "status" => {
            params: [],
            return_type: process_result(int()),
            is_static: false
        },
        "stdout" => {
            params: [],
            return_type: process_result(Type::Primitive(PrimitiveType::Bytes)),
            is_static: false
        },
        "stderr" => {
            params: [],
            return_type: process_result(Type::Primitive(PrimitiveType::Bytes)),
            is_static: false
        }
    }
}

fn process_pool_methods() -> HashMap<String, MethodSig> {
    let result_output = process_result(process_output_type());
    let result_channel = process_result(Type::Named(
        "Channel".to_string(),
        vec![result_output.clone()],
    ));
    let optional_channel = process_result(Type::Optional(Box::new(Type::Named(
        "Channel".to_string(),
        vec![result_output],
    ))));
    define_methods! {
        "new" => { params: [], return_type: process_result(process_pool_type()), is_static: true },
        "with_config" => { params: [int(), int()], return_type: process_result(process_pool_type()), is_static: true },
        "submit" => { params: [process_command_type()], return_type: result_channel, is_static: false },
        "try_submit" => { params: [process_command_type()], return_type: optional_channel.clone(), is_static: false },
        "submit_timeout" => { params: [process_command_type(), int()], return_type: optional_channel, is_static: false },
        "cancel_pending" => { params: [], return_type: process_result(int()), is_static: false },
        "close" => { params: [], return_type: process_result(Type::Void), is_static: false }
    }
}

#[must_use]
pub fn process_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "Command".to_string(),
        make_class_symbol("Command", process_command_methods(), span),
    );
    classes.insert(
        "Child".to_string(),
        make_class_symbol("Child", process_child_methods(), span),
    );
    classes.insert(
        "Output".to_string(),
        make_class_symbol("Output", process_output_methods(), span),
    );
    classes.insert(
        "ProcessPool".to_string(),
        make_class_symbol("ProcessPool", process_pool_methods(), span),
    );
    classes.insert(
        "ProcessError".to_string(),
        make_error_class_symbol_with_fields(
            "ProcessError",
            process_error_methods(),
            process_error_fields(),
            span,
        ),
    );
    classes.insert(
        "ProcessErrorKind".to_string(),
        make_enum_symbol(
            "ProcessErrorKind",
            &["Invalid", "Io", "Spawn", "Timeout", "State", "NotFound"],
            span,
        ),
    );
    classes
}

fn process_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: process_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn process_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (process_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn regex_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: regex_result(regex_type()),
            is_static: true
        },
        "from_pattern" => {
            params: [str_()],
            return_type: regex_result(regex_type()),
            is_static: true
        },
        "from_pattern_with_flags" => {
            params: [str_(), str_()],
            return_type: regex_result(regex_type()),
            is_static: true
        },
        "escape" => {
            params: [str_()],
            return_type: str_(),
            is_static: true
        },
        "is_match" => {
            params: [str_()],
            return_type: regex_result(bool_()),
            is_static: false
        },
        "full_match" => {
            params: [str_()],
            return_type: regex_result(bool_()),
            is_static: false
        },
        "find" => {
            params: [str_()],
            return_type: regex_result(Type::Optional(Box::new(regex_match_type()))),
            is_static: false
        },
        "find_all" => {
            params: [str_()],
            return_type: regex_result(Type::List(Box::new(regex_match_type()))),
            is_static: false
        },
        "replace" => {
            params: [str_(), str_()],
            return_type: regex_result(str_()),
            is_static: false
        },
        "replace_first" => {
            params: [str_(), str_()],
            return_type: regex_result(str_()),
            is_static: false
        },
        "replace_with" => {
            params: [str_(), Type::Function {
                params: vec![str_()],
                returns: Box::new(str_()),
                default_count: 0,
            }],
            return_type: regex_result(str_()),
            is_static: false
        },
        "split" => {
            params: [str_()],
            return_type: regex_result(Type::List(Box::new(str_()))),
            is_static: false
        },
        "group_count" => {
            params: [],
            return_type: regex_result(int()),
            is_static: false
        },
        "named_groups" => {
            params: [],
            return_type: regex_result(Type::List(Box::new(str_()))),
            is_static: false
        }
    }
}

fn regex_match_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "start" => {
            params: [],
            return_type: regex_result(int()),
            is_static: false
        },
        "end" => {
            params: [],
            return_type: regex_result(int()),
            is_static: false
        },
        "text" => {
            params: [],
            return_type: regex_result(str_()),
            is_static: false
        },
        "capture" => {
            params: [int()],
            return_type: regex_result(Type::Optional(Box::new(str_()))),
            is_static: false
        },
        "capture_named" => {
            params: [str_()],
            return_type: regex_result(Type::Optional(Box::new(str_()))),
            is_static: false
        },
        "captures" => {
            params: [],
            return_type: regex_result(Type::List(Box::new(Type::Optional(Box::new(str_()))))),
            is_static: false
        }
    }
}

#[must_use]
pub fn regex_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "RegexErrorKind".to_string(),
        make_enum_symbol(
            "RegexErrorKind",
            &["Invalid", "Parse", "Match", "Capture"],
            span,
        ),
    );
    classes.insert(
        "Regex".to_string(),
        make_class_symbol("Regex", regex_methods(), span),
    );
    classes.insert(
        "RegexMatch".to_string(),
        make_class_symbol("RegexMatch", regex_match_methods(), span),
    );
    classes.insert(
        "RegexError".to_string(),
        make_error_class_symbol_with_fields(
            "RegexError",
            regex_error_methods(),
            regex_error_fields(),
            span,
        ),
    );
    classes
}

fn regex_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: regex_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn regex_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (regex_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn logger_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: logger_type(), is_static: true },
        "default" => { params: [], return_type: logger_type(), is_static: true },
        "set_default" => { params: [logger_type()], return_type: log_result(Type::Void), is_static: true },
        "set_writer" => { params: [writer_type()], return_type: log_result(Type::Void), is_static: false },
        "set_level" => { params: [str_()], return_type: log_result(Type::Void), is_static: false },
        "set_name" => { params: [str_()], return_type: log_result(Type::Void), is_static: false },
        "field" => { params: [str_(), str_()], return_type: log_result(Type::Void), is_static: false },
        "trace" => { params: [str_()], return_type: log_result(Type::Void), is_static: false },
        "debug" => { params: [str_()], return_type: log_result(Type::Void), is_static: false },
        "info" => { params: [str_()], return_type: log_result(Type::Void), is_static: false },
        "warn" => { params: [str_()], return_type: log_result(Type::Void), is_static: false },
        "error" => { params: [str_()], return_type: log_result(Type::Void), is_static: false }
    }
}

fn log_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: log_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn log_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (log_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn random_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "seeded" => { params: [int()], return_type: random_type(), is_static: true },
        "system" => { params: [], return_type: random_type(), is_static: true },
        "next_int" => { params: [], return_type: int(), is_static: false },
        "next_range" => { params: [int(), int()], return_type: int(), is_static: false },
        "next_float" => { params: [], return_type: float(), is_static: false },
        "next_bool" => { params: [], return_type: bool_(), is_static: false },
        "bytes" => { params: [int()], return_type: random_result(Type::Primitive(PrimitiveType::Bytes)), is_static: false },
        "normal" => { params: [float(), float()], return_type: random_result(float()), is_static: false },
        "exponential" => { params: [float()], return_type: random_result(float()), is_static: false }
    }
}

#[must_use]
pub fn random_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    HashMap::from([
        (
            "RandomErrorKind".to_string(),
            make_enum_symbol(
                "RandomErrorKind",
                &["Invalid", "Range", "Unsupported", "Io"],
                span,
            ),
        ),
        (
            "Random".to_string(),
            make_class_symbol("Random", random_methods(), span),
        ),
        (
            "RandomError".to_string(),
            make_error_class_symbol_with_fields(
                "RandomError",
                random_error_methods(),
                random_error_fields(),
                span,
            ),
        ),
    ])
}

fn random_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: random_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn random_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (random_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn crypto_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: crypto_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn crypto_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (crypto_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn math_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: math_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn math_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (math_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

#[must_use]
pub fn math_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    HashMap::from([
        (
            "MathErrorKind".to_string(),
            make_enum_symbol(
                "MathErrorKind",
                &["Invalid", "Range", "Overflow", "Domain"],
                span,
            ),
        ),
        (
            "MathError".to_string(),
            make_error_class_symbol_with_fields(
                "MathError",
                math_error_methods(),
                math_error_fields(),
                span,
            ),
        ),
    ])
}

#[must_use]
pub fn crypto_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    HashMap::from([
        (
            "CryptoErrorKind".to_string(),
            make_enum_symbol(
                "CryptoErrorKind",
                &["Invalid", "Unsupported", "Authentication", "Io"],
                span,
            ),
        ),
        (
            "CryptoError".to_string(),
            make_error_class_symbol_with_fields(
                "CryptoError",
                crypto_error_methods(),
                crypto_error_fields(),
                span,
            ),
        ),
    ])
}

fn reader_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: reader_type(), is_static: true },
        "from_bytes" => { params: [Type::Primitive(PrimitiveType::Bytes)], return_type: reader_type(), is_static: true },
        "from_file" => { params: [str_()], return_type: stream_result(reader_type()), is_static: true },
        "from_tcp" => { params: [Type::Named("TcpStream".to_string(), Vec::new())], return_type: stream_result(reader_type()), is_static: true },
        "read" => { params: [int()], return_type: stream_result(Type::Primitive(PrimitiveType::Bytes)), is_static: false },
        "read_line" => { params: [], return_type: stream_result(Type::Optional(Box::new(str_()))), is_static: false },
        "limit" => { params: [int()], return_type: stream_result(Type::Void), is_static: false },
        "tee" => { params: [writer_type()], return_type: stream_result(Type::Void), is_static: false },
        "untee" => { params: [], return_type: stream_result(Type::Void), is_static: false },
        "read_exact" => { params: [int()], return_type: stream_result(Type::Primitive(PrimitiveType::Bytes)), is_static: false },
        "position" => { params: [], return_type: stream_result(int()), is_static: false },
        "remaining" => { params: [], return_type: stream_result(int()), is_static: false },
        "read_to_end" => { params: [int()], return_type: stream_result(Type::Primitive(PrimitiveType::Bytes)), is_static: false },
        "copy_to" => { params: [writer_type(), int()], return_type: stream_result(int()), is_static: false },
        "seek" => { params: [int()], return_type: stream_result(int()), is_static: false },
        "close" => { params: [], return_type: Type::Void, is_static: false }
    }
}

fn writer_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: writer_type(), is_static: true },
        "to_file" => { params: [str_()], return_type: stream_result(writer_type()), is_static: true },
        "append_file" => { params: [str_()], return_type: stream_result(writer_type()), is_static: true },
        "from_tcp" => { params: [Type::Named("TcpStream".to_string(), Vec::new())], return_type: stream_result(writer_type()), is_static: true },
        "write" => { params: [Type::Primitive(PrimitiveType::Bytes)], return_type: stream_result(int()), is_static: false },
        "write_all" => { params: [Type::Primitive(PrimitiveType::Bytes)], return_type: stream_result(Type::Void), is_static: false },
        "bytes" => { params: [], return_type: stream_result(Type::Primitive(PrimitiveType::Bytes)), is_static: false },
        "position" => { params: [], return_type: stream_result(int()), is_static: false },
        "seek" => { params: [int()], return_type: stream_result(int()), is_static: false },
        "flush" => { params: [], return_type: stream_result(Type::Void), is_static: false },
        "close" => { params: [], return_type: Type::Void, is_static: false }
    }
}

fn erased_stream_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_reader" => { params: [reader_type()], return_type: stream_result(stream_type()), is_static: true },
        "from_writer" => { params: [writer_type()], return_type: stream_result(stream_type()), is_static: true },
        "read" => { params: [int()], return_type: stream_result(Type::Primitive(PrimitiveType::Bytes)), is_static: false },
        "write" => { params: [Type::Primitive(PrimitiveType::Bytes)], return_type: stream_result(int()), is_static: false },
        "flush" => { params: [], return_type: stream_result(Type::Void), is_static: false },
        "seek" => { params: [int()], return_type: stream_result(int()), is_static: false },
        "close" => { params: [], return_type: Type::Void, is_static: false }
    }
}

fn readable_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "read" => {
            params: [int()],
            return_type: stream_result(Type::Primitive(PrimitiveType::Bytes)),
            is_static: false
        }
    }
}

fn writable_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "write" => {
            params: [Type::Primitive(PrimitiveType::Bytes)],
            return_type: stream_result(int()),
            is_static: false
        }
    }
}

fn seek_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "position" => {
            params: [],
            return_type: stream_result(int()),
            is_static: false
        },
        "seek" => {
            params: [int()],
            return_type: stream_result(int()),
            is_static: false
        }
    }
}

fn io_capability_symbols(span: Span) -> HashMap<String, Symbol> {
    let readable = readable_methods();
    let writable = writable_methods();
    let seek = seek_methods();
    HashMap::from([
        (
            "Readable".to_string(),
            make_interface_symbol("Readable", readable, span),
        ),
        (
            "Writable".to_string(),
            make_interface_symbol("Writable", writable, span),
        ),
        (
            "Seek".to_string(),
            make_interface_symbol("Seek", seek, span),
        ),
    ])
}

fn implement_io_capability(symbol: &mut Symbol, name: &str, methods: HashMap<String, MethodSig>) {
    symbol
        .interfaces
        .insert(name.to_string(), (Vec::new(), methods));
}

#[must_use]
pub fn io_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut reader = make_class_symbol("Reader", reader_methods(), span);
    implement_io_capability(&mut reader, "Readable", readable_methods());
    implement_io_capability(&mut reader, "Seek", seek_methods());

    let mut writer = make_class_symbol("Writer", writer_methods(), span);
    implement_io_capability(&mut writer, "Writable", writable_methods());
    implement_io_capability(&mut writer, "Seek", seek_methods());

    let mut symbols = io_capability_symbols(span);
    symbols.insert(
        "IoErrorKind".to_string(),
        make_enum_symbol(
            "IoErrorKind",
            &[
                "Invalid",
                "Io",
                "NotFound",
                "Permission",
                "NotUnicode",
                "Closed",
                "Os",
            ],
            span,
        ),
    );
    symbols.insert("Reader".to_string(), reader);
    symbols.insert("Writer".to_string(), writer);
    symbols.insert(
        "Stream".to_string(),
        make_class_symbol("Stream", erased_stream_methods(), span),
    );
    symbols.insert(
        "IoError".to_string(),
        make_error_class_symbol_with_fields("IoError", io_error_methods(), io_error_fields(), span),
    );
    symbols
}

fn io_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: io_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn io_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (io_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
        ("operation".to_string(), (str_(), false)),
    ])
}

fn path_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: path_type(), is_static: true },
        "from_string" => { params: [str_()], return_type: fs_result(path_type()), is_static: true },
        "to_string" => { params: [], return_type: fs_result(str_()), is_static: false },
        "display" => { params: [], return_type: fs_result(str_()), is_static: false },
        "is_absolute" => { params: [], return_type: fs_result(bool_()), is_static: false },
        "is_relative" => { params: [], return_type: fs_result(bool_()), is_static: false },
        "parent" => { params: [], return_type: fs_result(Type::Optional(Box::new(path_type()))), is_static: false },
        "file_name" => { params: [], return_type: fs_result(Type::Optional(Box::new(str_()))), is_static: false },
        "extension" => { params: [], return_type: fs_result(Type::Optional(Box::new(str_()))), is_static: false },
        "stem" => { params: [], return_type: fs_result(Type::Optional(Box::new(str_()))), is_static: false },
        "join" => { params: [str_()], return_type: fs_result(path_type()), is_static: false },
        "with_file_name" => { params: [str_()], return_type: fs_result(path_type()), is_static: false },
        "with_extension" => { params: [str_()], return_type: fs_result(path_type()), is_static: false }
    }
}

#[must_use]
pub fn fs_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut symbols = HashMap::from([
        (
            "FsErrorKind".to_string(),
            make_enum_symbol(
                "FsErrorKind",
                &[
                    "Invalid",
                    "Io",
                    "NotFound",
                    "Permission",
                    "NotUnicode",
                    "Os",
                ],
                span,
            ),
        ),
        (
            "Path".to_string(),
            make_class_symbol("Path", path_methods(), span),
        ),
        (
            "Directory".to_string(),
            make_class_symbol("Directory", directory_methods(), span),
        ),
    ]);
    symbols.insert(
        "FsError".to_string(),
        make_error_class_symbol_with_fields("FsError", fs_error_methods(), fs_error_fields(), span),
    );
    symbols
}

fn directory_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "open" => { params: [str_()], return_type: fs_result(directory_type()), is_static: true },
        "next" => { params: [], return_type: fs_result(Type::Optional(Box::new(str_()))), is_static: false },
        "close" => { params: [], return_type: Type::Void, is_static: false }
    }
}

#[must_use]
pub fn log_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    HashMap::from([
        (
            "LogErrorKind".to_string(),
            make_enum_symbol("LogErrorKind", &["Invalid", "Io", "Config", "State"], span),
        ),
        (
            "Logger".to_string(),
            make_class_symbol("Logger", logger_methods(), span),
        ),
        (
            "LogError".to_string(),
            make_error_class_symbol_with_fields(
                "LogError",
                log_error_methods(),
                log_error_fields(),
                span,
            ),
        ),
    ])
}

fn cli_parser_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: cli_parser_type(), is_static: true },
        "set_program" => { params: [str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_about" => { params: [str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_version" => { params: [str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_response_files" => { params: [bool_()], return_type: cli_result(Type::Void), is_static: false },
        "add_option" => { params: [str_(), str_(), bool_(), bool_()], return_type: cli_result(Type::Void), is_static: false },
        "set_option_env" => { params: [str_(), str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_option_default" => { params: [str_(), str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_option_multiple" => { params: [str_(), bool_()], return_type: cli_result(Type::Void), is_static: false },
        "set_option_conflicts" => { params: [str_(), str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_option_requires" => { params: [str_(), str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_option_alias" => { params: [str_(), str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_option_group" => { params: [str_(), str_()], return_type: cli_result(Type::Void), is_static: false },
        "set_option_parser" => {
            params: [str_(), Type::Function {
                params: vec![str_()],
                returns: Box::new(io_result(str_())),
                default_count: 0,
            }],
            return_type: cli_result(Type::Void),
            is_static: false
        },
        "add_positional" => { params: [str_(), bool_()], return_type: cli_result(Type::Void), is_static: false },
        "add_subcommand" => { params: [str_(), str_()], return_type: cli_result(cli_parser_type()), is_static: false },
        "parse" => { params: [Type::List(Box::new(str_()))], return_type: cli_result(cli_matches_type()), is_static: false },
        "parse_process" => { params: [], return_type: cli_result(cli_matches_type()), is_static: false },
        "parse_or_exit" => { params: [], return_type: cli_matches_type(), is_static: false },
        "help" => { params: [], return_type: cli_result(str_()), is_static: false },
        "completion" => { params: [str_()], return_type: cli_result(str_()), is_static: false },
        "manpage" => { params: [], return_type: cli_result(str_()), is_static: false }
    }
}

fn cli_matches_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "has" => { params: [str_()], return_type: cli_result(bool_()), is_static: false },
        "get" => { params: [str_()], return_type: cli_result(Type::Optional(Box::new(str_()))), is_static: false },
        "get_int" => { params: [str_()], return_type: cli_result(Type::Optional(Box::new(int()))), is_static: false },
        "get_float" => { params: [str_()], return_type: cli_result(Type::Optional(Box::new(float()))), is_static: false },
        "get_bool" => { params: [str_()], return_type: cli_result(Type::Optional(Box::new(bool_()))), is_static: false },
        "values" => { params: [str_()], return_type: cli_result(Type::List(Box::new(str_()))), is_static: false },
        "positional" => { params: [int()], return_type: cli_result(Type::Optional(Box::new(str_()))), is_static: false },
        "subcommand" => { params: [], return_type: cli_result(Type::Optional(Box::new(str_()))), is_static: false },
        "subcommand_matches" => { params: [], return_type: cli_result(Type::Optional(Box::new(cli_matches_type()))), is_static: false },
        "help" => { params: [], return_type: cli_result(str_()), is_static: false }
    }
}

#[must_use]
pub fn cli_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    HashMap::from([
        (
            "CliErrorKind".to_string(),
            make_enum_symbol("CliErrorKind", &["Invalid", "Parse", "Io"], span),
        ),
        (
            "CliParser".to_string(),
            make_class_symbol("CliParser", cli_parser_methods(), span),
        ),
        (
            "CliMatches".to_string(),
            make_class_symbol("CliMatches", cli_matches_methods(), span),
        ),
        (
            "CliError".to_string(),
            make_error_class_symbol_with_fields(
                "CliError",
                cli_error_methods(),
                cli_error_fields(),
                span,
            ),
        ),
    ])
}

fn cli_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: cli_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn cli_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (cli_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn uuid_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_bytes" => { params: [Type::Primitive(PrimitiveType::Bytes)], return_type: uuid_result(uuid_type()), is_static: true },
        "nil" => { params: [], return_type: uuid_type(), is_static: true },
        "max" => { params: [], return_type: uuid_type(), is_static: true },
        "v1" => { params: [], return_type: uuid_type(), is_static: true },
        "v3" => { params: [uuid_type(), str_()], return_type: uuid_result(uuid_type()), is_static: true },
        "v4" => { params: [], return_type: uuid_type(), is_static: true },
        "v5" => { params: [uuid_type(), str_()], return_type: uuid_result(uuid_type()), is_static: true },
        "v6" => { params: [], return_type: uuid_type(), is_static: true },
        "v7" => { params: [], return_type: uuid_type(), is_static: true },
        "v8" => { params: [Type::Primitive(PrimitiveType::Bytes)], return_type: uuid_result(uuid_type()), is_static: true },
        "from_parts" => { params: [int(), int()], return_type: uuid_result(uuid_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "to_compact" => { params: [], return_type: str_(), is_static: false },
        "to_braced" => { params: [], return_type: str_(), is_static: false },
        "to_urn" => { params: [], return_type: str_(), is_static: false },
        "to_bytes" => { params: [], return_type: Type::Primitive(PrimitiveType::Bytes), is_static: false },
        "to_parts" => { params: [], return_type: Type::Tuple(Box::new(int()), Box::new(int())), is_static: false },
        "is_nil" => { params: [], return_type: bool_(), is_static: false },
        "is_max" => { params: [], return_type: bool_(), is_static: false },
        "version" => { params: [], return_type: Type::Optional(Box::new(int())), is_static: false },
        "variant" => { params: [], return_type: str_(), is_static: false }
    }
}

#[must_use]
pub fn uuid_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "UuidErrorKind".to_string(),
        make_enum_symbol("UuidErrorKind", &["Invalid", "Parse", "NotUnicode"], span),
    );
    classes.insert(
        "Uuid".to_string(),
        make_class_symbol("Uuid", uuid_methods(), span),
    );
    classes.insert(
        "UuidError".to_string(),
        make_error_class_symbol_with_fields(
            "UuidError",
            uuid_error_methods(),
            uuid_error_fields(),
            span,
        ),
    );
    classes
}

fn url_query_pairs_type() -> Type {
    Type::List(Box::new(Type::Tuple(Box::new(str_()), Box::new(str_()))))
}

fn url_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "parse" => { params: [str_()], return_type: url_result(url_type()), is_static: true },
        "from_file" => { params: [str_()], return_type: url_result(url_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "scheme" => { params: [], return_type: str_(), is_static: false },
        "username" => { params: [], return_type: str_(), is_static: false },
        "password" => { params: [], return_type: Type::Optional(Box::new(str_())), is_static: false },
        "host" => { params: [], return_type: Type::Optional(Box::new(str_())), is_static: false },
        "host_ascii" => { params: [], return_type: url_result(str_()), is_static: false },
        "host_unicode" => { params: [], return_type: url_result(str_()), is_static: false },
        "port" => { params: [], return_type: Type::Optional(Box::new(int())), is_static: false },
        "path" => { params: [], return_type: str_(), is_static: false },
        "query" => { params: [], return_type: Type::Optional(Box::new(str_())), is_static: false },
        "fragment" => { params: [], return_type: Type::Optional(Box::new(str_())), is_static: false },
        "origin" => { params: [], return_type: str_(), is_static: false },
        "redacted" => { params: [], return_type: str_(), is_static: false },
        "join" => { params: [str_()], return_type: url_result(url_type()), is_static: false },
        "with_path" => { params: [str_()], return_type: url_result(url_type()), is_static: false },
        "with_query" => { params: [str_()], return_type: url_result(url_type()), is_static: false },
        "with_fragment" => { params: [str_()], return_type: url_result(url_type()), is_static: false },
        "with_host" => { params: [str_()], return_type: url_result(url_type()), is_static: false },
        "with_port" => { params: [int()], return_type: url_result(url_type()), is_static: false },
        "with_scheme" => { params: [str_()], return_type: url_result(url_type()), is_static: false },
        "with_username" => { params: [str_()], return_type: url_result(url_type()), is_static: false },
        "with_password" => { params: [str_()], return_type: url_result(url_type()), is_static: false },
        "query_pairs" => { params: [], return_type: url_query_pairs_type(), is_static: false },
        "with_query_pairs" => { params: [url_query_pairs_type()], return_type: url_result(url_type()), is_static: false },
        "to_file_path" => { params: [], return_type: url_result(str_()), is_static: false },
        "is_http" => { params: [], return_type: bool_(), is_static: false },
        "is_https" => { params: [], return_type: bool_(), is_static: false }
    }
}

#[must_use]
pub fn url_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "UrlErrorKind".to_string(),
        make_enum_symbol("UrlErrorKind", &["Invalid", "Unsupported", "Parse"], span),
    );
    classes.insert(
        "Url".to_string(),
        make_class_symbol("Url", url_methods(), span),
    );
    classes.insert(
        "UrlError".to_string(),
        make_error_class_symbol_with_fields(
            "UrlError",
            url_error_methods(),
            url_error_fields(),
            span,
        ),
    );
    classes
}

#[must_use]
pub fn net_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "HttpErrorKind".to_string(),
        make_enum_symbol(
            "HttpErrorKind",
            &[
                "Invalid",
                "Transport",
                "Timeout",
                "Resolve",
                "Protocol",
                "Status",
            ],
            span,
        ),
    );
    classes.insert(
        "NetErrorKind".to_string(),
        make_enum_symbol(
            "NetErrorKind",
            &["Invalid", "Timeout", "Resolve", "Unsupported", "Io"],
            span,
        ),
    );
    let mut tcp_stream = make_class_symbol("TcpStream", tcp_stream_methods(), span);
    implement_io_capability(&mut tcp_stream, "Readable", readable_methods());
    implement_io_capability(&mut tcp_stream, "Writable", writable_methods());
    classes.insert("TcpStream".to_string(), tcp_stream);
    classes.insert(
        "UdpSocket".to_string(),
        make_class_symbol("UdpSocket", udp_socket_methods(), span),
    );
    classes.insert(
        "UdpDatagram".to_string(),
        make_class_symbol("UdpDatagram", udp_datagram_methods(), span),
    );
    classes.insert(
        "TcpListener".to_string(),
        make_class_symbol("TcpListener", tcp_listener_methods(), span),
    );
    let mut local_stream = make_class_symbol("LocalStream", local_stream_methods(), span);
    implement_io_capability(&mut local_stream, "Readable", readable_methods());
    implement_io_capability(&mut local_stream, "Writable", writable_methods());
    classes.insert("LocalStream".to_string(), local_stream);
    classes.insert(
        "LocalListener".to_string(),
        make_class_symbol("LocalListener", local_listener_methods(), span),
    );
    classes.insert(
        "IpAddr".to_string(),
        make_class_symbol("IpAddr", ip_addr_methods(), span),
    );
    classes.insert(
        "SocketAddr".to_string(),
        make_class_symbol("SocketAddr", socket_addr_methods(), span),
    );
    classes.insert(
        "Cidr".to_string(),
        make_class_symbol("Cidr", cidr_methods(), span),
    );
    classes.insert(
        "Endpoint".to_string(),
        make_class_symbol("Endpoint", endpoint_methods(), span),
    );
    classes.insert(
        "Poller".to_string(),
        make_class_symbol("Poller", poller_methods(), span),
    );
    classes.insert(
        "PollEvent".to_string(),
        make_class_symbol("PollEvent", poll_event_methods(), span),
    );
    classes.insert(
        "NetError".to_string(),
        make_error_class_symbol_with_fields(
            "NetError",
            net_error_methods(),
            net_error_fields(),
            span,
        ),
    );
    classes.extend(io_capability_symbols(span));
    classes.insert(
        "http".to_string(),
        make_import_module_symbol("net.http", span),
    );
    classes.insert(
        "websocket".to_string(),
        make_import_module_symbol("net.websocket", span),
    );
    classes.insert(
        "url".to_string(),
        make_import_module_symbol("net.url", span),
    );
    classes.insert(
        "Headers".to_string(),
        make_class_symbol("Headers", http_headers_methods(), span),
    );
    classes.insert(
        "HttpRequest".to_string(),
        make_class_symbol_with_fields(
            "HttpRequest",
            http_request_methods(),
            http_request_fields(),
            span,
        ),
    );
    classes.insert(
        "HttpResponse".to_string(),
        make_class_symbol_with_fields(
            "HttpResponse",
            http_response_methods(),
            http_response_fields(),
            span,
        ),
    );
    classes.insert(
        "HttpServer".to_string(),
        make_class_symbol("HttpServer", http_server_methods(), span),
    );
    classes.insert(
        "HttpServerConfig".to_string(),
        make_class_symbol_with_fields(
            "HttpServerConfig",
            http_server_config_methods(),
            http_server_config_fields(),
            span,
        ),
    );
    classes.insert(
        "HttpRouter".to_string(),
        make_class_symbol("HttpRouter", http_router_methods(), span),
    );
    classes.insert(
        "OAuthClient".to_string(),
        make_class_symbol("OAuthClient", oauth_client_methods(), span),
    );
    classes.insert(
        "OAuthSession".to_string(),
        make_class_symbol("OAuthSession", oauth_session_methods(), span),
    );
    classes.insert(
        "HttpNext".to_string(),
        make_class_symbol("HttpNext", http_next_methods(), span),
    );
    classes.insert(
        "SseEvent".to_string(),
        make_class_symbol_with_fields("SseEvent", sse_event_methods(), sse_event_fields(), span),
    );
    classes.insert(
        "WebSocketFrame".to_string(),
        make_class_symbol_with_fields(
            "WebSocketFrame",
            websocket_frame_methods(),
            websocket_frame_fields(),
            span,
        ),
    );
    classes.insert(
        "WebSocketHandshake".to_string(),
        make_class_symbol_with_fields(
            "WebSocketHandshake",
            websocket_handshake_methods(),
            websocket_handshake_fields(),
            span,
        ),
    );
    classes.insert(
        "SseStream".to_string(),
        make_class_symbol("SseStream", sse_stream_methods(), span),
    );
    classes.insert(
        "WebSocketSession".to_string(),
        make_class_symbol("WebSocketSession", websocket_session_methods(), span),
    );
    classes.insert(
        "HttpError".to_string(),
        make_error_class_symbol_with_fields(
            "HttpError",
            http_error_methods(),
            http_error_fields(),
            span,
        ),
    );
    classes
}

/// Symbols exposed by the nested `std.net.http` module. Keep the HTTP
/// namespace focused on request/response construction rather than making a
/// direct nested import also pull in unrelated socket types.
pub fn http_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let all = net_module_class_symbols(span);
    [
        "Headers",
        "HttpRequest",
        "HttpResponse",
        "HttpServer",
        "HttpServerConfig",
        "HttpRouter",
        "OAuthClient",
        "OAuthSession",
        "HttpNext",
        "SseEvent",
        "SseStream",
        "WebSocketFrame",
        "WebSocketHandshake",
        "HttpError",
        "HttpErrorKind",
    ]
    .into_iter()
    .filter_map(|name| {
        all.get(name)
            .cloned()
            .map(|symbol| (name.to_string(), symbol))
    })
    .collect()
}

/// Symbols exposed by `std.net.websocket`.
pub fn websocket_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let all = net_module_class_symbols(span);
    [
        "Headers",
        "WebSocketFrame",
        "WebSocketHandshake",
        "WebSocketSession",
        "HttpError",
        "HttpErrorKind",
    ]
    .into_iter()
    .filter_map(|name| {
        all.get(name)
            .cloned()
            .map(|symbol| (name.to_string(), symbol))
    })
    .collect()
}

/// Symbols exposed by the `std.env` module's structured error type.
pub fn env_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    HashMap::from([
        (
            "EnvErrorKind".to_string(),
            make_enum_symbol("EnvErrorKind", &["Invalid", "NotUnicode", "Os"], span),
        ),
        (
            "EnvError".to_string(),
            make_error_class_symbol_with_fields(
                "EnvError",
                env_error_methods(),
                env_error_fields(),
                span,
            ),
        ),
    ])
}

fn tls_stream_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "connect" => { params: [str_(), str_()], return_type: tls_result(tls_stream_type()), is_static: true },
        "connect_with_roots" => { params: [str_(), str_(), Type::List(Box::new(net_bytes_type()))], return_type: tls_result(tls_stream_type()), is_static: true },
        "connect_with_config" => { params: [str_(), str_(), tls_config_type()], return_type: tls_result(tls_stream_type()), is_static: true },
        "connect_with_roots_config" => { params: [str_(), str_(), Type::List(Box::new(net_bytes_type())), tls_config_type()], return_type: tls_result(tls_stream_type()), is_static: true },
        "connect_with_client_cert" => { params: [str_(), str_(), Type::List(Box::new(net_bytes_type())), Type::List(Box::new(net_bytes_type())), net_bytes_type()], return_type: tls_result(tls_stream_type()), is_static: true },
        "connect_with_client_cert_config" => { params: [str_(), str_(), Type::List(Box::new(net_bytes_type())), Type::List(Box::new(net_bytes_type())), net_bytes_type(), tls_config_type()], return_type: tls_result(tls_stream_type()), is_static: true },
        "accept" => { params: [tcp_stream_type(), Type::List(Box::new(net_bytes_type())), net_bytes_type()], return_type: tls_result(tls_stream_type()), is_static: true },
        "accept_with_config" => { params: [tcp_stream_type(), Type::List(Box::new(net_bytes_type())), net_bytes_type(), tls_config_type()], return_type: tls_result(tls_stream_type()), is_static: true },
        "read" => { params: [int()], return_type: stream_result(net_bytes_type()), is_static: false },
        "write" => { params: [net_bytes_type()], return_type: stream_result(int()), is_static: false },
        "flush" => { params: [], return_type: tls_result(Type::Void), is_static: false },
        "shutdown" => { params: [], return_type: tls_result(Type::Void), is_static: false },
        "peer_certificates" => { params: [], return_type: tls_result(Type::List(Box::new(net_bytes_type()))), is_static: false },
        "protocol_version" => { params: [], return_type: tls_result(str_()), is_static: false },
        "cipher_suite" => { params: [], return_type: tls_result(str_()), is_static: false },
        "alpn_protocol" => { params: [], return_type: tls_result(net_bytes_type()), is_static: false }
    }
}

fn tls_config_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: tls_result(tls_config_type()), is_static: true },
        "set_protocols" => { params: [str_(), str_()], return_type: tls_result(Type::Void), is_static: false },
        "set_cipher_suites" => { params: [Type::List(Box::new(str_()))], return_type: tls_result(Type::Void), is_static: false },
        "set_alpn_protocols" => { params: [Type::List(Box::new(net_bytes_type()))], return_type: tls_result(Type::Void), is_static: false }
    }
}

#[must_use]
pub fn tls_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut tls_stream = make_class_symbol("TlsStream", tls_stream_methods(), span);
    implement_io_capability(&mut tls_stream, "Readable", readable_methods());
    implement_io_capability(&mut tls_stream, "Writable", writable_methods());
    let mut symbols = io_capability_symbols(span);
    symbols.insert("TlsStream".to_string(), tls_stream);
    symbols.insert(
        "TlsConfig".to_string(),
        make_class_symbol("TlsConfig", tls_config_methods(), span),
    );
    symbols.insert(
        "TlsError".to_string(),
        make_error_class_symbol_with_fields(
            "TlsError",
            tls_error_methods(),
            tls_error_fields(),
            span,
        ),
    );
    symbols.insert(
        "TlsErrorKind".to_string(),
        make_enum_symbol(
            "TlsErrorKind",
            &[
                "Invalid",
                "Io",
                "Timeout",
                "Handshake",
                "Certificate",
                "Unsupported",
                "Protocol",
            ],
            span,
        ),
    );
    symbols
}

fn tls_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: tls_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn tls_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (tls_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn http_headers_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: http_headers_type(),
            is_static: true
        },
        "set" => {
            params: [str_(), str_()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "append" => {
            params: [str_(), str_()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "get" => {
            params: [str_()],
            return_type: http_result(Type::Optional(Box::new(str_()))),
            is_static: false
        },
        "values" => {
            params: [str_()],
            return_type: http_result(Type::List(Box::new(str_()))),
            is_static: false
        },
        "remove" => {
            params: [str_()],
            return_type: http_result(Type::Void),
            is_static: false
        }
    }
}

fn http_request_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: http_request_type(),
            is_static: true
        },
        "from_config" => {
            params: [str_(), str_(), http_headers_type(), net_bytes_type()],
            return_type: http_result(http_request_type()),
            is_static: true
        },
        "read" => {
            params: [tcp_stream_type()],
            return_type: http_result(http_request_type()),
            is_static: true
        },
        "send" => {
            params: [],
            return_type: http_result(http_response_type()),
            is_static: false
        },
        "path_param" => {
            params: [str_()],
            return_type: Type::Optional(Box::new(str_())),
            is_static: false
        },
        "set_body_reader" => {
            params: [reader_type()],
            return_type: http_result(Type::Void),
            is_static: false
        }
    }
}

fn http_server_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "serve_once" => {
            params: [tcp_listener_type(), http_server_config_type(), http_handler_type()],
            return_type: http_result(Type::Void),
            is_static: true
        },
        "serve" => {
            params: [tcp_listener_type(), http_server_config_type(), http_handler_type(), int()],
            return_type: http_result(Type::Void),
            is_static: true
        },
        "serve_until_cancelled" => {
            params: [tcp_listener_type(), http_server_config_type(), http_handler_type(), Type::Named("CancellationToken".to_string(), Vec::new())],
            return_type: http_result(Type::Void),
            is_static: true
        }
    }
}

fn http_server_config_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: http_server_config_type(),
            is_static: true
        }
    }
}

fn http_router_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: http_router_type(),
            is_static: true
        },
        "route" => {
            params: [str_(), str_(), http_handler_type()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "middleware" => {
            params: [http_middleware_type()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "basic_auth" => {
            params: [str_(), str_()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "bearer_auth" => {
            params: [str_()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "oauth_oidc" => {
            params: [str_(), str_(), str_()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "handle" => {
            params: [http_request_type()],
            return_type: http_result(http_response_type()),
            is_static: false
        }
    }
}

fn http_next_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "handle" => {
            params: [http_request_type()],
            return_type: http_result(http_response_type()),
            is_static: false
        }
    }
}

fn oauth_client_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: oauth_client_type(),
            is_static: true
        },
        "from_config" => {
            params: [str_(), str_(), str_(), str_()],
            return_type: http_result(oauth_client_type()),
            is_static: true
        },
        "discover" => {
            params: [],
            return_type: http_result(json_type()),
            is_static: false
        },
        "authorization_url" => {
            params: [str_(), str_(), str_()],
            return_type: http_result(str_()),
            is_static: false
        },
        "exchange_code" => {
            params: [str_(), str_()],
            return_type: http_result(json_type()),
            is_static: false
        },
        "refresh" => {
            params: [str_()],
            return_type: http_result(json_type()),
            is_static: false
        },
        "revoke" => {
            params: [str_(), str_()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "introspect" => {
            params: [str_()],
            return_type: http_result(json_type()),
            is_static: false
        }
    }
}

fn oauth_session_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_token_response" => {
            params: [json_type()],
            return_type: http_result(oauth_session_type()),
            is_static: true
        },
        "access_token" => {
            params: [],
            return_type: http_result(str_()),
            is_static: false
        },
        "refresh_token" => {
            params: [],
            return_type: http_result(Type::Optional(Box::new(str_()))),
            is_static: false
        },
        "id_token" => {
            params: [],
            return_type: http_result(Type::Optional(Box::new(str_()))),
            is_static: false
        },
        "token_type" => {
            params: [],
            return_type: http_result(str_()),
            is_static: false
        },
        "is_expired" => {
            params: [],
            return_type: http_result(bool_()),
            is_static: false
        },
        "refresh" => {
            params: [oauth_client_type()],
            return_type: http_result(oauth_session_type()),
            is_static: false
        },
        "revoke" => {
            params: [oauth_client_type(), str_()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "introspect" => {
            params: [oauth_client_type()],
            return_type: http_result(json_type()),
            is_static: false
        },
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        }
    }
}

fn sse_event_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: sse_event_type(),
            is_static: true
        },
        "from_config" => {
            params: [str_(), str_(), int(), str_()],
            return_type: http_result(sse_event_type()),
            is_static: true
        },
        "encode" => {
            params: [],
            return_type: http_result(net_bytes_type()),
            is_static: false
        }
    }
}

fn sse_event_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("event".to_string(), (str_(), false)),
        ("id".to_string(), (str_(), false)),
        ("retry_ms".to_string(), (int(), false)),
        ("data".to_string(), (str_(), false)),
    ])
}

fn sse_stream_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_tcp" => {
            params: [tcp_stream_type()],
            return_type: http_result(sse_stream_type()),
            is_static: true
        },
        "send" => {
            params: [sse_event_type()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "flush" => {
            params: [],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        }
    }
}

fn websocket_frame_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: websocket_frame_type(),
            is_static: true
        },
        "from_config" => {
            params: [bool_(), int(), net_bytes_type(), bool_()],
            return_type: http_result(websocket_frame_type()),
            is_static: true
        },
        "encode" => {
            params: [],
            return_type: http_result(net_bytes_type()),
            is_static: false
        },
        "decode" => {
            params: [net_bytes_type()],
            return_type: http_result(websocket_frame_type()),
            is_static: true
        },
        "reassemble" => {
            params: [Type::List(Box::new(websocket_frame_type()))],
            return_type: http_result(websocket_frame_type()),
            is_static: true
        }
    }
}

fn websocket_frame_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("fin".to_string(), (bool_(), false)),
        ("opcode".to_string(), (int(), false)),
        ("payload".to_string(), (net_bytes_type(), false)),
        ("masked".to_string(), (bool_(), false)),
    ])
}

fn websocket_handshake_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: websocket_handshake_type(),
            is_static: true
        },
        "from_config" => {
            params: [str_(), str_()],
            return_type: http_result(websocket_handshake_type()),
            is_static: true
        },
        "request_key" => {
            params: [],
            return_type: http_result(str_()),
            is_static: true
        },
        "accept_key" => {
            params: [],
            return_type: http_result(str_()),
            is_static: false
        },
        "response_headers" => {
            params: [],
            return_type: http_result(http_headers_type()),
            is_static: false
        }
    }
}

fn websocket_handshake_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("key".to_string(), (str_(), false)),
        ("protocol".to_string(), (str_(), false)),
    ])
}

fn websocket_session_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_tcp" => {
            params: [tcp_stream_type()],
            return_type: http_result(websocket_session_type()),
            is_static: true
        },
        "receive" => {
            params: [],
            return_type: http_result(websocket_frame_type()),
            is_static: false
        },
        "send" => {
            params: [websocket_frame_type()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        }
    }
}

fn http_server_config_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("max_header_bytes".to_string(), (int(), false)),
        ("max_body_bytes".to_string(), (int(), false)),
        ("max_headers".to_string(), (int(), false)),
        ("read_timeout_ms".to_string(), (int(), false)),
        ("access_log".to_string(), (bool_(), false)),
        (
            "cors_origins".to_string(),
            (Type::List(Box::new(str_())), false),
        ),
        ("cors_allow_credentials".to_string(), (bool_(), false)),
        ("static_root".to_string(), (str_(), false)),
        ("worker_count".to_string(), (int(), false)),
        ("heartbeat_interval_ms".to_string(), (int(), false)),
    ])
}

fn http_request_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("method".to_string(), (str_(), false)),
        ("url".to_string(), (str_(), false)),
        ("request_id".to_string(), (str_(), false)),
        ("proxy".to_string(), (str_(), false)),
        ("headers".to_string(), (http_headers_type(), false)),
        ("body".to_string(), (net_bytes_type(), false)),
        ("connect_timeout_ms".to_string(), (int(), false)),
        ("timeout_ms".to_string(), (int(), false)),
        ("max_redirects".to_string(), (int(), false)),
        ("retries".to_string(), (int(), false)),
        ("retry_backoff_ms".to_string(), (int(), false)),
    ])
}

fn http_response_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => {
            params: [],
            return_type: http_response_type(),
            is_static: true
        },
        "from_config" => {
            params: [int(), http_headers_type(), net_bytes_type()],
            return_type: http_result(http_response_type()),
            is_static: true
        },
        "write" => {
            params: [tcp_stream_type()],
            return_type: http_result(Type::Void),
            is_static: false
        },
        "error_for_status" => {
            params: [],
            return_type: http_result(http_response_type()),
            is_static: false
        },
        "read_bytes" => {
            params: [int()],
            return_type: http_result(net_bytes_type()),
            is_static: false
        },
        "reader" => {
            params: [int()],
            return_type: http_result(reader_type()),
            is_static: false
        },
        "read_text" => {
            params: [int()],
            return_type: http_result(str_()),
            is_static: false
        },
        "read_json" => {
            params: [int()],
            return_type: http_result(json_type()),
            is_static: false
        },
        "save" => {
            params: [str_()],
            return_type: http_result(Type::Void),
            is_static: false
        }
    }
}

fn http_response_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("status".to_string(), (int(), false)),
        ("headers".to_string(), (http_headers_type(), false)),
        ("body".to_string(), (net_bytes_type(), false)),
    ])
}

fn http_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: http_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn http_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (http_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
        ("status".to_string(), (int(), false)),
        ("method".to_string(), (str_(), false)),
        ("url".to_string(), (str_(), false)),
    ])
}

fn env_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: env_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn env_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (env_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
        ("key".to_string(), (str_(), false)),
    ])
}

fn fs_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: fs_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn fs_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (fs_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
        ("path".to_string(), (str_(), false)),
    ])
}

fn net_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: net_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn net_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (net_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
        ("address".to_string(), (str_(), false)),
    ])
}

fn url_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: url_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn url_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (url_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
        ("url".to_string(), (str_(), false)),
    ])
}

fn uuid_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: uuid_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn uuid_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (uuid_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
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
            return_type: sql_result(sql_transaction_type()),
            is_static: false
        },
        "begin_transaction_with_options" => {
            params: [str_(), bool_(), bool_()],
            return_type: sql_result(sql_transaction_type()),
            is_static: false
        },
        "prepare" => {
            params: [str_()],
            return_type: sql_result(sql_prepared_type()),
            is_static: false
        }
    };
    insert_sql_query_methods(&mut methods);
    methods
}

fn sql_pool_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "from_config" => { params: [str_(), int(), int()], return_type: sql_result(sql_pool_type()), is_static: true },
        "close" => { params: [], return_type: sql_result(Type::Void), is_static: false },
        "metrics" => { params: [], return_type: sql_result(Type::Map(Box::new(str_()), Box::new(Type::Primitive(PrimitiveType::Int)))), is_static: false }
    };
    insert_sql_query_methods(&mut methods);
    methods
}

fn sql_migration_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_config" => {
            params: [int(), str_(), str_(), str_()],
            return_type: sql_result(sql_migration_type()),
            is_static: true
        }
    }
}

fn sql_migrator_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_migrations" => {
            params: [sql_connection_type(), Type::List(Box::new(sql_migration_type()))],
            return_type: sql_result(sql_migrator_type()),
            is_static: true
        },
        "from_directory" => {
            params: [sql_connection_type(), str_()],
            return_type: sql_result(sql_migrator_type()),
            is_static: true
        },
        "up" => { params: [], return_type: sql_result(int()), is_static: false },
        "up_to" => { params: [int()], return_type: sql_result(int()), is_static: false },
        "down" => { params: [], return_type: sql_result(int()), is_static: false },
        "down_to" => { params: [int()], return_type: sql_result(int()), is_static: false },
        "status" => {
            params: [],
            return_type: sql_result(Type::List(Box::new(Type::Map(Box::new(str_()), Box::new(str_()))))),
            is_static: false
        },
        "validate" => { params: [], return_type: sql_result(Type::Void), is_static: false },
        "dry_run" => {
            params: [],
            return_type: sql_result(Type::List(Box::new(str_()))),
            is_static: false
        }
    }
}

fn sql_transaction_methods() -> HashMap<String, MethodSig> {
    let mut methods = define_methods! {
        "begin_transaction" => {
            params: [],
            return_type: sql_result(sql_transaction_type()),
            is_static: false
        },
        "savepoint" => {
            params: [str_()],
            return_type: sql_result(Type::Void),
            is_static: false
        },
        "rollback_to" => {
            params: [str_()],
            return_type: sql_result(Type::Void),
            is_static: false
        },
        "release_savepoint" => {
            params: [str_()],
            return_type: sql_result(Type::Void),
            is_static: false
        },
        "commit" => {
            params: [],
            return_type: sql_result(Type::Void),
            is_static: false
        },
        "rollback" => {
            params: [],
            return_type: sql_result(Type::Void),
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
            return_type: sql_result(int()),
            is_static: false
        },
        "execute_batch" => {
            params: [str_()],
            return_type: sql_result(Type::Void),
            is_static: false
        },
        "execute_many" => {
            params: [str_(), Type::List(Box::new(Type::List(Box::new(sql_value_type()))))],
            return_type: sql_result(int()),
            is_static: false
        },
        "execute_params" => {
            params: [str_(), Type::List(Box::new(sql_value_type()))],
            return_type: sql_result(int()),
            is_static: false
        },
        "execute_named" => {
            params: [str_(), Type::Map(Box::new(str_()), Box::new(sql_value_type()))],
            return_type: sql_result(int()),
            is_static: false
        },
        "query" => {
            params: [str_()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_params" => {
            params: [str_(), Type::List(Box::new(sql_value_type()))],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_named" => {
            params: [str_(), Type::Map(Box::new(str_()), Box::new(sql_value_type()))],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_with_timeout" => {
            params: [str_(), int()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_params_with_timeout" => {
            params: [str_(), Type::List(Box::new(sql_value_type())), int()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_named_with_timeout" => {
            params: [str_(), Type::Map(Box::new(str_()), Box::new(sql_value_type())), int()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_with_cancellation" => {
            params: [str_(), sql_cancellation_token_type()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_params_with_cancellation" => {
            params: [str_(), Type::List(Box::new(sql_value_type())), sql_cancellation_token_type()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_named_with_cancellation" => {
            params: [str_(), Type::Map(Box::new(str_()), Box::new(sql_value_type())), sql_cancellation_token_type()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        }
    });
}

fn sql_result_set_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "close" => {
            params: [],
            return_type: sql_result(Type::Void),
            is_static: false
        },
        "rows" => {
            params: [],
            return_type: sql_result(Type::List(Box::new(sql_row_type()))),
            is_static: false
        },
        "next" => {
            params: [],
            return_type: sql_result(Type::Optional(Box::new(sql_row_type()))),
            is_static: false
        },
        "next_batch" => {
            params: [int()],
            return_type: sql_result(Type::List(Box::new(sql_row_type()))),
            is_static: false
        },
        "columns" => {
            params: [],
            return_type: Type::List(Box::new(str_())),
            is_static: false
        }
    }
}

fn sql_row_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "columns" => { params: [], return_type: Type::List(Box::new(str_())), is_static: false },
        "values" => { params: [], return_type: Type::List(Box::new(sql_value_type())), is_static: false },
        "at" => { params: [int()], return_type: sql_result(sql_value_type()), is_static: false },
        "get" => { params: [str_()], return_type: sql_result(sql_value_type()), is_static: false }
    }
}

fn sql_prepared_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "close" => {
            params: [],
            return_type: Type::Void,
            is_static: false
        },
        "execute" => {
            params: [Type::List(Box::new(sql_value_type()))],
            return_type: sql_result(int()),
            is_static: false
        },
        "execute_named" => {
            params: [Type::Map(Box::new(str_()), Box::new(sql_value_type()))],
            return_type: sql_result(int()),
            is_static: false
        },
        "query" => {
            params: [Type::List(Box::new(sql_value_type()))],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_named" => {
            params: [Type::Map(Box::new(str_()), Box::new(sql_value_type()))],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_with_timeout" => {
            params: [Type::List(Box::new(sql_value_type())), int()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_named_with_timeout" => {
            params: [Type::Map(Box::new(str_()), Box::new(sql_value_type())), int()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_with_cancellation" => {
            params: [Type::List(Box::new(sql_value_type())), sql_cancellation_token_type()],
            return_type: sql_result(sql_result_set_type()),
            is_static: false
        },
        "query_named_with_cancellation" => {
            params: [Type::Map(Box::new(str_()), Box::new(sql_value_type())), sql_cancellation_token_type()],
            return_type: sql_result(sql_result_set_type()),
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
            return_type: sql_result(bool_()),
            is_static: false
        },
        "as_int" => {
            params: [],
            return_type: sql_result(int()),
            is_static: false
        },
        "as_float" => {
            params: [],
            return_type: sql_result(float()),
            is_static: false
        },
        "as_string" => {
            params: [],
            return_type: sql_result(str_()),
            is_static: false
        },
        "as_bytes" => {
            params: [],
            return_type: sql_result(Type::Primitive(PrimitiveType::Bytes)),
            is_static: false
        },
        "as_json" => {
            params: [],
            return_type: sql_result(json_type()),
            is_static: false
        },
        "as_datetime" => {
            params: [],
            return_type: sql_result(datetime_type()),
            is_static: false
        },
        "as_uuid" => {
            params: [],
            return_type: sql_result(uuid_type()),
            is_static: false
        },
        "to_string" => {
            params: [],
            return_type: str_(),
            is_static: false
        }
    }
}

fn sql_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: sql_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn sql_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (sql_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
        ("provider".to_string(), (str_(), false)),
        ("code".to_string(), (str_(), false)),
        ("constraint".to_string(), (str_(), false)),
        ("operation".to_string(), (str_(), false)),
    ])
}

#[must_use]
pub fn sql_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "SqlErrorKind".to_string(),
        make_enum_symbol(
            "SqlErrorKind",
            &[
                "Constraint",
                "Timeout",
                "Unsupported",
                "Invalid",
                "Database",
            ],
            span,
        ),
    );
    classes.insert(
        "Connection".to_string(),
        make_class_symbol("Connection", sql_connection_methods(), span),
    );
    classes.insert(
        "Pool".to_string(),
        make_class_symbol("Pool", sql_pool_methods(), span),
    );
    classes.insert(
        "Migration".to_string(),
        make_class_symbol("Migration", sql_migration_methods(), span),
    );
    classes.insert(
        "Migrator".to_string(),
        make_class_symbol("Migrator", sql_migrator_methods(), span),
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
        "Row".to_string(),
        make_class_symbol("Row", sql_row_methods(), span),
    );
    classes.insert(
        "SqlValue".to_string(),
        make_class_symbol("SqlValue", sql_value_methods(), span),
    );
    classes.insert(
        "SqlError".to_string(),
        make_error_class_symbol_with_fields(
            "SqlError",
            sql_error_methods(),
            sql_error_fields(),
            span,
        ),
    );
    classes.insert(
        "PreparedStatement".to_string(),
        make_class_symbol("PreparedStatement", sql_prepared_methods(), span),
    );
    classes
}

fn thread_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "join" => {
            params: [],
            return_type: sync_result(Type::Variable("T".to_string())),
            is_static: false
        },
        "detach" => {
            params: [],
            return_type: sync_result(Type::Void),
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
            params: [Type::Named(
                "Mutex".to_string(),
                vec![Type::Variable("T".to_string())],
            )],
            return_type: sync_result(Type::Void),
            is_static: false
        },
        "wait_timeout" => {
            params: [
                Type::Named(
                    "Mutex".to_string(),
                    vec![Type::Variable("T".to_string())],
                ),
                int(),
            ],
            return_type: sync_result(bool_()),
            is_static: false
        },
        "signal" => {
            params: [],
            return_type: sync_result(Type::Void),
            is_static: false
        },
        "broadcast" => {
            params: [],
            return_type: sync_result(Type::Void),
            is_static: false
        }
    }
}

fn atomic_int_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: Type::Named("AtomicInt".to_string(), Vec::new()), is_static: true },
        "with_value" => { params: [int()], return_type: Type::Named("AtomicInt".to_string(), Vec::new()), is_static: true },
        "load" => { params: [], return_type: sync_result(int()), is_static: false },
        "store" => { params: [int()], return_type: sync_result(Type::Void), is_static: false },
        "add" => { params: [int()], return_type: sync_result(int()), is_static: false },
        "swap" => { params: [int()], return_type: sync_result(int()), is_static: false },
        "compare_exchange" => { params: [int(), int()], return_type: sync_result(int()), is_static: false }
    }
}

fn atomic_bool_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: Type::Named("AtomicBool".to_string(), Vec::new()), is_static: true },
        "with_value" => { params: [bool_()], return_type: Type::Named("AtomicBool".to_string(), Vec::new()), is_static: true },
        "load" => { params: [], return_type: sync_result(bool_()), is_static: false },
        "store" => { params: [bool_()], return_type: sync_result(Type::Void), is_static: false },
        "swap" => { params: [bool_()], return_type: sync_result(bool_()), is_static: false },
        "compare_exchange" => { params: [bool_(), bool_()], return_type: sync_result(bool_()), is_static: false }
    }
}

fn cancellation_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: Type::Named("CancellationToken".to_string(), Vec::new()), is_static: true },
        "cancel" => { params: [], return_type: sync_result(Type::Void), is_static: false },
        "is_cancelled" => { params: [], return_type: sync_result(bool_()), is_static: false }
    }
}

fn semaphore_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "with_permits" => { params: [int()], return_type: sync_result(Type::Named("Semaphore".to_string(), Vec::new())), is_static: true },
        "acquire" => { params: [], return_type: sync_result(Type::Void), is_static: false },
        "try_acquire" => { params: [], return_type: sync_result(bool_()), is_static: false },
        "acquire_timeout" => { params: [int()], return_type: sync_result(bool_()), is_static: false },
        "release" => { params: [], return_type: sync_result(Type::Void), is_static: false }
    }
}

fn barrier_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "with_size" => { params: [int()], return_type: sync_result(Type::Named("Barrier".to_string(), Vec::new())), is_static: true },
        "wait" => { params: [], return_type: sync_result(bool_()), is_static: false }
    }
}

fn channel_methods() -> HashMap<String, MethodSig> {
    let channel_type = || Type::Named("Channel".to_string(), vec![Type::Variable("T".to_string())]);
    define_methods! {
        "select" => { params: [Type::List(Box::new(channel_type())), int()], return_type: sync_result(Type::Optional(Box::new(Type::Tuple(Box::new(int()), Box::new(Type::Variable("T".to_string())))))), is_static: true },
        "new" => { params: [], return_type: channel_type(), is_static: true },
        "new_unbounded" => { params: [], return_type: channel_type(), is_static: true },
        "new_bounded" => { params: [int()], return_type: sync_result(channel_type()), is_static: true },
        "send" => { params: [Type::Variable("T".to_string())], return_type: sync_result(Type::Void), is_static: false },
        "try_send" => { params: [Type::Variable("T".to_string())], return_type: sync_result(bool_()), is_static: false },
        "send_timeout" => { params: [Type::Variable("T".to_string()), int()], return_type: sync_result(bool_()), is_static: false },
        "send_cancelled" => { params: [Type::Variable("T".to_string()), Type::Named("CancellationToken".to_string(), Vec::new())], return_type: sync_result(bool_()), is_static: false },
        "recv" => { params: [], return_type: sync_result(Type::Optional(Box::new(Type::Variable("T".to_string())))), is_static: false },
        "try_recv" => { params: [], return_type: sync_result(Type::Optional(Box::new(Type::Variable("T".to_string())))), is_static: false },
        "recv_timeout" => { params: [int()], return_type: sync_result(Type::Optional(Box::new(Type::Variable("T".to_string())))), is_static: false },
        "recv_cancelled" => { params: [Type::Named("CancellationToken".to_string(), Vec::new())], return_type: sync_result(Type::Optional(Box::new(Type::Variable("T".to_string())))), is_static: false },
        "close" => { params: [], return_type: sync_result(Type::Void), is_static: false },
        "is_closed" => { params: [], return_type: sync_result(bool_()), is_static: false },
        "capacity" => { params: [], return_type: sync_result(Type::Optional(Box::new(int()))), is_static: false }
    }
}

fn worker_pool_methods() -> HashMap<String, MethodSig> {
    let pool_type = || {
        Type::Named(
            "WorkerPool".to_string(),
            vec![Type::Variable("T".to_string())],
        )
    };
    let channel_type = || Type::Named("Channel".to_string(), vec![Type::Variable("T".to_string())]);
    let callback = || Type::Function {
        params: Vec::new(),
        returns: Box::new(Type::Variable("T".to_string())),
        default_count: 0,
    };
    define_methods! {
        "new" => { params: [], return_type: pool_type(), is_static: true },
        "with_size" => { params: [int()], return_type: sync_result(pool_type()), is_static: true },
        "with_config" => { params: [int(), int()], return_type: sync_result(pool_type()), is_static: true },
        "submit" => { params: [callback()], return_type: sync_result(channel_type()), is_static: false },
        "map" => {
            params: [Type::List(Box::new(Type::Variable("U".to_string()))), Type::Function {
                params: vec![Type::Variable("U".to_string())],
                returns: Box::new(Type::Variable("T".to_string())),
                default_count: 0,
            }],
            return_type: sync_result(Type::List(Box::new(Type::Variable("T".to_string())))),
            is_static: false
        },
        "try_submit" => { params: [callback()], return_type: sync_result(Type::Optional(Box::new(channel_type()))), is_static: false },
        "submit_timeout" => { params: [callback(), int()], return_type: sync_result(Type::Optional(Box::new(channel_type()))), is_static: false },
        "cancel_pending" => { params: [], return_type: sync_result(int()), is_static: false },
        "close" => { params: [], return_type: sync_result(Type::Void), is_static: false }
    }
}

fn once_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: Type::Named("Once".to_string(), Vec::new()), is_static: true },
        "call" => {
            params: [Type::Function {
                params: Vec::new(),
                returns: Box::new(Type::Void),
                default_count: 0,
            }],
            return_type: sync_result(Type::Void),
            is_static: false
        }
    }
}

fn lock_callback() -> Type {
    Type::Function {
        params: vec![Type::Reference(Box::new(Type::Variable("T".to_string())))],
        returns: Box::new(Type::Void),
        default_count: 0,
    }
}

fn rwlock_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "with_value" => {
            params: [Type::Variable("T".to_string())],
            return_type: Type::Named("RwLock".to_string(), vec![Type::Variable("T".to_string())]),
            is_static: true
        },
        "with_read" => {
            params: [lock_callback()],
            return_type: sync_result(Type::Void),
            is_static: false
        },
        "with_write" => {
            params: [lock_callback()],
            return_type: sync_result(Type::Void),
            is_static: false
        }
    }
}

fn mutex_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "with_value" => {
            params: [Type::Variable("T".to_string())],
            return_type: Type::Named("Mutex".to_string(), vec![Type::Variable("T".to_string())]),
            is_static: true
        },
        "with_lock" => {
            params: [lock_callback()],
            return_type: sync_result(Type::Void),
            is_static: false
        }
    }
}

#[must_use]
pub fn sync_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    let mut classes = HashMap::new();
    classes.insert(
        "Thread".to_string(),
        make_generic_class_symbol("Thread", "T", thread_methods(), span),
    );
    classes.insert(
        "CondVar".to_string(),
        make_class_symbol("CondVar", condvar_methods(), span),
    );
    classes.insert(
        "RwLock".to_string(),
        make_generic_class_symbol("RwLock", "T", rwlock_methods(), span),
    );
    classes.insert(
        "Mutex".to_string(),
        make_generic_class_symbol("Mutex", "T", mutex_methods(), span),
    );
    classes.insert(
        "AtomicInt".to_string(),
        make_class_symbol("AtomicInt", atomic_int_methods(), span),
    );
    classes.insert(
        "AtomicBool".to_string(),
        make_class_symbol("AtomicBool", atomic_bool_methods(), span),
    );
    classes.insert(
        "CancellationToken".to_string(),
        make_class_symbol("CancellationToken", cancellation_methods(), span),
    );
    classes.insert(
        "Semaphore".to_string(),
        make_class_symbol("Semaphore", semaphore_methods(), span),
    );
    classes.insert(
        "Barrier".to_string(),
        make_class_symbol("Barrier", barrier_methods(), span),
    );
    classes.insert(
        "Channel".to_string(),
        make_generic_class_symbol("Channel", "T", channel_methods(), span),
    );
    classes.insert(
        "WorkerPool".to_string(),
        make_generic_class_symbol("WorkerPool", "T", worker_pool_methods(), span),
    );
    classes.insert(
        "Once".to_string(),
        make_class_symbol("Once", once_methods(), span),
    );
    classes.insert(
        "SyncError".to_string(),
        make_error_class_symbol_with_fields(
            "SyncError",
            sync_error_methods(),
            sync_error_fields(),
            span,
        ),
    );
    classes.insert(
        "SyncErrorKind".to_string(),
        make_enum_symbol(
            "SyncErrorKind",
            &[
                "Invalid", "State", "Timeout", "Closed", "Callback", "Spawn", "Io",
            ],
            span,
        ),
    );
    classes
}

fn sync_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: sync_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn sync_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (sync_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
}

fn datetime_fn(name: &'static str, params: &'static [Type], ret: Type) -> StdlibItem {
    io_fn(name, params, ret)
}

fn date_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: date_type(), is_static: true },
        "from_parts" => { params: [int(), int(), int()], return_type: datetime_result(date_type()), is_static: true },
        "parse" => { params: [str_()], return_type: datetime_result(date_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "year" => { params: [], return_type: int(), is_static: false },
        "month" => { params: [], return_type: int(), is_static: false },
        "day" => { params: [], return_type: int(), is_static: false },
        "weekday" => { params: [], return_type: int(), is_static: false },
        "format" => { params: [str_()], return_type: datetime_result(str_()), is_static: false },
        "add_days" => { params: [int()], return_type: datetime_result(date_type()), is_static: false }
    }
}

fn time_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: time_type(), is_static: true },
        "from_parts" => { params: [int(), int(), int(), int()], return_type: datetime_result(time_type()), is_static: true },
        "parse" => { params: [str_()], return_type: datetime_result(time_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "hour" => { params: [], return_type: int(), is_static: false },
        "minute" => { params: [], return_type: int(), is_static: false },
        "second" => { params: [], return_type: int(), is_static: false },
        "nanosecond" => { params: [], return_type: int(), is_static: false },
        "format" => { params: [str_()], return_type: datetime_result(str_()), is_static: false }
    }
}

fn datetime_value_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: datetime_type(), is_static: true },
        "now" => { params: [], return_type: datetime_type(), is_static: true },
        "from_timestamp" => { params: [int(), int()], return_type: datetime_result(datetime_type()), is_static: true },
        "from_date_time" => { params: [date_type(), time_type()], return_type: datetime_result(datetime_type()), is_static: true },
        "parse_pattern" => { params: [str_(), str_()], return_type: datetime_result(datetime_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "format" => { params: [str_()], return_type: datetime_result(str_()), is_static: false },
        "unix_seconds" => { params: [], return_type: datetime_result(int()), is_static: false },
        "unix_nanos" => { params: [], return_type: datetime_result(int()), is_static: false },
        "date" => { params: [], return_type: date_type(), is_static: false },
        "time" => { params: [], return_type: time_type(), is_static: false },
        "add_duration" => { params: [duration_type()], return_type: datetime_result(datetime_type()), is_static: false }
    }
}

fn zoned_datetime_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: zoned_datetime_type(), is_static: true },
        "from_local" => { params: [date_type(), time_type(), str_()], return_type: datetime_result(zoned_datetime_type()), is_static: true },
        "resolve_local" => { params: [date_type(), time_type(), str_()], return_type: datetime_result(local_resolution_type()), is_static: true },
        "from_instant" => { params: [instant_type(), str_()], return_type: datetime_result(zoned_datetime_type()), is_static: true },
        "parse" => { params: [str_()], return_type: datetime_result(zoned_datetime_type()), is_static: true },
        "to_string" => { params: [], return_type: str_(), is_static: false },
        "zone" => { params: [], return_type: str_(), is_static: false },
        "offset_seconds" => { params: [], return_type: datetime_result(int()), is_static: false },
        "date" => { params: [], return_type: date_type(), is_static: false },
        "time" => { params: [], return_type: time_type(), is_static: false },
        "instant" => { params: [], return_type: datetime_result(instant_type()), is_static: false },
        "add_duration" => { params: [duration_type()], return_type: datetime_result(zoned_datetime_type()), is_static: false }
    }
}

fn local_resolution_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "kind" => { params: [], return_type: str_(), is_static: false },
        "earlier" => { params: [], return_type: Type::Optional(Box::new(zoned_datetime_type())), is_static: false },
        "later" => { params: [], return_type: Type::Optional(Box::new(zoned_datetime_type())), is_static: false }
    }
}

fn instant_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: instant_type(), is_static: true },
        "now" => { params: [], return_type: instant_type(), is_static: true },
        "from_unix_nanos" => { params: [int()], return_type: datetime_result(instant_type()), is_static: true },
        "unix_nanos" => { params: [], return_type: datetime_result(int()), is_static: false },
        "add_duration" => { params: [duration_type()], return_type: datetime_result(instant_type()), is_static: false },
        "duration_since" => { params: [instant_type()], return_type: datetime_result(duration_type()), is_static: false }
    }
}

fn duration_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: duration_type(), is_static: true },
        "from_parts" => { params: [int(), int()], return_type: datetime_result(duration_type()), is_static: true },
        "from_seconds" => { params: [int()], return_type: datetime_result(duration_type()), is_static: true },
        "from_millis" => { params: [int()], return_type: datetime_result(duration_type()), is_static: true },
        "from_micros" => { params: [int()], return_type: datetime_result(duration_type()), is_static: true },
        "from_nanos" => { params: [int()], return_type: datetime_result(duration_type()), is_static: true },
        "to_nanos" => { params: [], return_type: datetime_result(int()), is_static: false },
        "add" => { params: [duration_type()], return_type: datetime_result(duration_type()), is_static: false },
        "sub" => { params: [duration_type()], return_type: datetime_result(duration_type()), is_static: false }
    }
}

fn period_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "new" => { params: [], return_type: period_type(), is_static: true },
        "from_parts" => { params: [int(), int(), int()], return_type: datetime_result(period_type()), is_static: true },
        "years" => { params: [], return_type: int(), is_static: false },
        "months" => { params: [], return_type: int(), is_static: false },
        "days" => { params: [], return_type: int(), is_static: false },
        "add_to_date" => { params: [date_type()], return_type: datetime_result(date_type()), is_static: false }
    }
}

#[must_use]
pub fn datetime_module_class_symbols(span: Span) -> HashMap<String, Symbol> {
    HashMap::from([
        (
            "Date".to_string(),
            make_class_symbol("Date", date_methods(), span),
        ),
        (
            "Time".to_string(),
            make_class_symbol("Time", time_methods(), span),
        ),
        (
            "DateTime".to_string(),
            make_class_symbol("DateTime", datetime_value_methods(), span),
        ),
        (
            "ZonedDateTime".to_string(),
            make_class_symbol("ZonedDateTime", zoned_datetime_methods(), span),
        ),
        (
            "LocalResolution".to_string(),
            make_class_symbol("LocalResolution", local_resolution_methods(), span),
        ),
        (
            "Instant".to_string(),
            make_class_symbol("Instant", instant_methods(), span),
        ),
        (
            "Duration".to_string(),
            make_class_symbol("Duration", duration_methods(), span),
        ),
        (
            "Period".to_string(),
            make_class_symbol("Period", period_methods(), span),
        ),
        (
            "DateTimeError".to_string(),
            make_error_class_symbol_with_fields(
                "DateTimeError",
                datetime_error_methods(),
                datetime_error_fields(),
                span,
            ),
        ),
        (
            "DateTimeErrorKind".to_string(),
            make_enum_symbol(
                "DateTimeErrorKind",
                &["Invalid", "Parse", "Range", "Format", "System"],
                span,
            ),
        ),
    ])
}

fn datetime_error_methods() -> HashMap<String, MethodSig> {
    define_methods! {
        "from_message" => { params: [str_()], return_type: datetime_error_type(), is_static: true },
        "message" => { params: [], return_type: str_(), is_static: false },
        "to_string" => { params: [], return_type: str_(), is_static: false }
    }
}

fn datetime_error_fields() -> HashMap<String, (Type, bool)> {
    HashMap::from([
        ("kind".to_string(), (datetime_error_kind_type(), false)),
        ("detail".to_string(), (str_(), false)),
    ])
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
    pub static ref RANDOM_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        m.insert(
            "random.bytes",
            StdlibItem::Function {
                params: INT_PARAM.to_vec(),
                ret: random_result(Type::Primitive(PrimitiveType::Bytes)),
                llvm_name: "mux_rand_bytes".to_string(),
            },
        );
        m.insert(
            "random.choose",
            StdlibItem::Function {
                params: LIST_T_PARAM.clone(),
                ret: Type::Optional(Box::new(Type::Variable("T".to_string()))),
                llvm_name: "mux_random_choose".to_string(),
            },
        );
        m.insert(
            "random.shuffle",
            StdlibItem::Function {
                params: LIST_T_PARAM.clone(),
                ret: Type::Void,
                llvm_name: "mux_random_shuffle".to_string(),
            },
        );
        m.insert(
            "random.sample",
            StdlibItem::Function {
                params: LIST_T_INT_PARAMS.clone(),
                ret: random_result(Type::List(Box::new(Type::Variable("T".to_string())))),
                llvm_name: "mux_random_sample".to_string(),
            },
        );
        m.insert(
            "random.weighted_choice",
            StdlibItem::Function {
                params: LIST_T_LIST_FLOAT_PARAMS.clone(),
                ret: random_result(Type::Variable("T".to_string())),
                llvm_name: "mux_random_weighted_choice".to_string(),
            },
        );
        for (name, params, llvm_name) in [
            ("random.normal", vec![float(), float()], "mux_rand_normal"),
            ("random.exponential", vec![float()], "mux_rand_exponential"),
        ] {
            m.insert(
                name,
                StdlibItem::Function {
                    params,
                    ret: random_result(float()),
                    llvm_name: llvm_name.to_string(),
                },
            );
        }
        m
    };
    pub static ref IO_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        insert_items! {
            m;
            "io.stdin" => StdlibItem::Function {
                params: Vec::new(),
                ret: reader_type(),
                llvm_name: "mux_io_stdin".to_string(),
            },
            "io.stdout" => StdlibItem::Function {
                params: Vec::new(),
                ret: writer_type(),
                llvm_name: "mux_io_stdout".to_string(),
            },
            "io.stderr" => StdlibItem::Function {
                params: Vec::new(),
                ret: writer_type(),
                llvm_name: "mux_io_stderr".to_string(),
            }
        }
        m
    };
    /// Filesystem-focused operations. The runtime ABI is shared with the
    /// low-level implementation, but the public package boundary is `std.fs`.
    pub static ref FS_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        insert_items! {
            m;
            "fs.read_file" => io_str_fn("mux_io_read_file", fs_result(str_())),
            "fs.write_file" => io_str_str_fn("mux_io_write_file", fs_result(Type::Void)),
            "fs.read_bytes" => StdlibItem::Function {
                params: vec![str_()],
                ret: fs_result(Type::Primitive(PrimitiveType::Bytes)),
                llvm_name: "mux_io_read_bytes".to_string(),
            },
            "fs.write_bytes" => StdlibItem::Function {
                params: vec![str_(), Type::Primitive(PrimitiveType::Bytes)],
                ret: fs_result(Type::Void),
                llvm_name: "mux_io_write_bytes".to_string(),
            },
            "fs.exists" => io_str_fn("mux_io_exists", fs_result(bool_())),
            "fs.remove" => io_str_fn("mux_io_remove", fs_result(Type::Void)),
            "fs.remove_dir_all" => io_str_fn("mux_io_remove_dir_all", fs_result(Type::Void)),
            "fs.is_file" => io_str_fn("mux_io_is_file", fs_result(bool_())),
            "fs.is_dir" => io_str_fn("mux_io_is_dir", fs_result(bool_())),
            "fs.mkdir" => io_str_fn("mux_io_mkdir", fs_result(Type::Void)),
            "fs.listdir" => io_str_fn("mux_io_listdir", fs_result(Type::List(Box::new(Type::Primitive(PrimitiveType::Str))))),
            "fs.join" => io_str_str_fn("mux_io_join", fs_result(str_())),
            "fs.basename" => io_str_fn("mux_io_basename", fs_result(str_())),
            "fs.dirname" => io_str_fn("mux_io_dirname", fs_result(str_())),
            "fs.cwd" => StdlibItem::Function {
                params: Vec::new(),
                ret: fs_result(str_()),
                llvm_name: "mux_io_cwd".to_string(),
            },
            "fs.absolute" => io_str_fn("mux_io_absolute", fs_result(str_())),
            "fs.canonical" => io_str_fn("mux_io_canonical", fs_result(str_())),
            "fs.copy" => io_str_str_fn("mux_io_copy", fs_result(Type::Void)),
            "fs.rename" => io_str_str_fn("mux_io_rename", fs_result(Type::Void)),
            "fs.replace_atomic" => io_str_str_fn("mux_io_replace_atomic", fs_result(Type::Void)),
            "fs.file_size" => io_str_fn("mux_io_file_size", fs_result(int())),
            "fs.is_symlink" => io_str_fn("mux_io_is_symlink", fs_result(bool_())),
        }
        m.insert(
            "fs.temp_file",
            StdlibItem::Function {
                params: Vec::new(),
                ret: fs_result(str_()),
                llvm_name: "mux_fs_temp_file".to_string(),
            },
        );
        m.insert(
            "fs.temp_dir",
            StdlibItem::Function {
                params: Vec::new(),
                ret: fs_result(str_()),
                llvm_name: "mux_fs_temp_dir".to_string(),
            },
        );
        m.insert(
            "fs.is_readonly",
            StdlibItem::Function {
                params: vec![str_()],
                ret: fs_result(bool_()),
                llvm_name: "mux_fs_is_readonly".to_string(),
            },
        );
        m.insert(
            "fs.set_readonly",
            StdlibItem::Function {
                params: vec![str_(), bool_()],
                ret: fs_result(Type::Void),
                llvm_name: "mux_fs_set_readonly".to_string(),
            },
        );
        m.insert(
            "fs.read_link",
            StdlibItem::Function {
                params: vec![str_()],
                ret: fs_result(str_()),
                llvm_name: "mux_fs_read_link".to_string(),
            },
        );
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
        fn make_int_fn(llvm_name: &'static str, params: &'static [Type]) -> StdlibItem {
            StdlibItem::Function {
                params: params.to_vec(),
                ret: Type::Primitive(PrimitiveType::Int),
                llvm_name: llvm_name.to_string(),
            }
        }
        fn make_result_fn(llvm_name: &'static str, params: &'static [Type], ok: Type) -> StdlibItem {
            StdlibItem::Function {
                params: params.to_vec(),
                ret: math_result(ok),
                llvm_name: llvm_name.to_string(),
            }
        }
        fn make_bool_fn(llvm_name: &'static str) -> StdlibItem {
            StdlibItem::Function {
                params: FLOAT_PARAM.to_vec(),
                ret: Type::Primitive(PrimitiveType::Bool),
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
            "math.trunc" => make_math_fn("mux_math_trunc", FLOAT_PARAM),
            "math.fract" => make_math_fn("mux_math_fract", FLOAT_PARAM),
            "math.sinh" => make_math_fn("mux_math_sinh", FLOAT_PARAM),
            "math.cosh" => make_math_fn("mux_math_cosh", FLOAT_PARAM),
            "math.tanh" => make_math_fn("mux_math_tanh", FLOAT_PARAM),
            "math.asinh" => make_math_fn("mux_math_asinh", FLOAT_PARAM),
            "math.acosh" => make_math_fn("mux_math_acosh", FLOAT_PARAM),
            "math.atanh" => make_math_fn("mux_math_atanh", FLOAT_PARAM),
            "math.to_radians" => make_math_fn("mux_math_to_radians", FLOAT_PARAM),
            "math.to_degrees" => make_math_fn("mux_math_to_degrees", FLOAT_PARAM),
            "math.exp2" => make_math_fn("mux_math_exp2", FLOAT_PARAM),
            "math.exp_m1" => make_math_fn("mux_math_exp_m1", FLOAT_PARAM),
            "math.ln_1p" => make_math_fn("mux_math_ln_1p", FLOAT_PARAM),
            "math.cbrt" => make_math_fn("mux_math_cbrt", FLOAT_PARAM),
            "math.signum" => make_math_fn("mux_math_signum", FLOAT_PARAM),
            "math.erf" => make_math_fn("mux_math_erf", FLOAT_PARAM),
            "math.gamma" => make_math_fn("mux_math_gamma", FLOAT_PARAM),
            "math.sum" => make_result_fn("mux_math_sum", &LIST_FLOAT_PARAMS, float()),
            "math.product" => make_result_fn("mux_math_product", &LIST_FLOAT_PARAMS, float()),
            "math.atan2" => make_math_fn("mux_math_atan2", FLOAT_FLOAT_PARAMS),
            "math.log" => make_math_fn("mux_math_log", FLOAT_FLOAT_PARAMS),
            "math.min" => make_math_fn("mux_math_min", FLOAT_FLOAT_PARAMS),
            "math.max" => make_math_fn("mux_math_max", FLOAT_FLOAT_PARAMS),
            "math.hypot" => make_math_fn("mux_math_hypot", FLOAT_FLOAT_PARAMS),
            "math.pow" => make_math_fn("mux_math_pow", FLOAT_FLOAT_PARAMS),
            "math.clamp" => make_math_fn("mux_math_clamp", FLOAT_FLOAT_FLOAT_PARAMS),
            "math.lerp" => make_math_fn("mux_math_lerp", FLOAT_FLOAT_FLOAT_PARAMS),
            "math.clamp_checked" => make_result_fn("mux_math_clamp_checked", FLOAT_FLOAT_FLOAT_PARAMS, float()),
            "math.inverse_lerp" => make_result_fn("mux_math_inverse_lerp", FLOAT_FLOAT_FLOAT_PARAMS, float()),
            "math.smoothstep" => make_result_fn("mux_math_smoothstep", FLOAT_FLOAT_FLOAT_PARAMS, float()),
            "math.is_nan" => make_bool_fn("mux_math_is_nan"),
            "math.is_infinite" => make_bool_fn("mux_math_is_infinite"),
            "math.is_finite" => make_bool_fn("mux_math_is_finite"),
            "math.gcd" => make_int_fn("mux_math_gcd", INT_INT_PARAMS),
            "math.lcm" => make_int_fn("mux_math_lcm", INT_INT_PARAMS),
            "math.isqrt" => make_int_fn("mux_math_isqrt", INT_PARAM),
            "math.factorial" => make_result_fn("mux_math_factorial", INT_PARAM, int()),
            "math.combinations" => make_result_fn("mux_math_combinations", INT_INT_PARAMS, int()),
            "math.permutations" => make_result_fn("mux_math_permutations", INT_INT_PARAMS, int())
        }
        m
    };
    pub static ref DATETIME_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        insert_items! {
            m;
            "datetime.now" => datetime_fn("mux_datetime_now", EMPTY_PARAMS, datetime_result(int())),
            "datetime.now_millis" => datetime_fn("mux_datetime_now_millis", EMPTY_PARAMS, datetime_result(int())),
            "datetime.now_micros" => datetime_fn("mux_datetime_now_micros", EMPTY_PARAMS, datetime_result(int())),
            "datetime.now_nanos" => datetime_fn("mux_datetime_now_nanos", EMPTY_PARAMS, datetime_result(int())),
            "datetime.year" => datetime_fn("mux_datetime_year", INT_PARAM, datetime_result(int())),
            "datetime.month" => datetime_fn("mux_datetime_month", INT_PARAM, datetime_result(int())),
            "datetime.day" => datetime_fn("mux_datetime_day", INT_PARAM, datetime_result(int())),
            "datetime.hour" => datetime_fn("mux_datetime_hour", INT_PARAM, datetime_result(int())),
            "datetime.minute" => datetime_fn("mux_datetime_minute", INT_PARAM, datetime_result(int())),
            "datetime.second" => datetime_fn("mux_datetime_second", INT_PARAM, datetime_result(int())),
            "datetime.weekday" => datetime_fn("mux_datetime_weekday", INT_PARAM, datetime_result(int())),
            "datetime.format" => datetime_fn("mux_datetime_format", INT_STR_PARAMS, datetime_result(str_())),
            "datetime.format_local" => datetime_fn("mux_datetime_format_local", INT_STR_PARAMS, datetime_result(str_())),
            "datetime.parse_timestamp" => datetime_fn("mux_datetime_parse_timestamp", STR_PARAM, datetime_result(int())),
            "datetime.parse_datetime" => datetime_fn("mux_datetime_parse_datetime", STR_PARAM, datetime_result(datetime_type())),
            "datetime.format_timestamp" => datetime_fn("mux_datetime_format_timestamp", INT_PARAM, datetime_result(str_())),
            "datetime.parse_http_date" => datetime_fn("mux_datetime_parse_http_date", STR_PARAM, datetime_result(int())),
            "datetime.format_http_date" => datetime_fn("mux_datetime_format_http_date", INT_PARAM, datetime_result(str_())),
            "datetime.sleep" => datetime_fn("mux_datetime_sleep", INT_PARAM, datetime_result(Type::Void)),
            "datetime.sleep_millis" => datetime_fn("mux_datetime_sleep_millis", INT_PARAM, datetime_result(Type::Void))
        }
        m
    };
    pub static ref UUID_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        m.insert(
            "uuid.parse_uuid",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: uuid_result(uuid_type()),
                llvm_name: "mux_uuid_parse_uuid".to_string(),
            },
        );
        m
    };
    pub static ref SYNC_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        fn spawn_fn() -> StdlibItem {
            StdlibItem::Function {
                params: vec![Type::Function {
                    params: Vec::new(),
                    returns: Box::new(Type::Variable("T".to_string())),
                    default_count: 0,
                }],
                ret: Type::Result(
                    Box::new(Type::Named(
                        "Thread".to_string(),
                        vec![Type::Variable("T".to_string())],
                    )),
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
    pub static ref PROCESS_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        m.insert(
            "process.args",
            StdlibItem::Function {
                params: EMPTY_PARAMS.to_vec(),
                ret: Type::List(Box::new(str_())),
                llvm_name: "mux_process_args".to_string(),
            },
        );
        m.insert(
            "process.id",
            StdlibItem::Function {
                params: EMPTY_PARAMS.to_vec(),
                ret: int(),
                llvm_name: "mux_process_id".to_string(),
            },
        );
        m.insert(
            "process.parent_id",
            StdlibItem::Function {
                params: EMPTY_PARAMS.to_vec(),
                ret: Type::Optional(Box::new(int())),
                llvm_name: "mux_process_parent_id".to_string(),
            },
        );
        m.insert(
            "process.executable",
            StdlibItem::Function {
                params: EMPTY_PARAMS.to_vec(),
                ret: process_result(str_()),
                llvm_name: "mux_process_executable".to_string(),
            },
        );
        m
    };
    pub static ref ENV_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        // env.get :: Str -> Result(Optional(Str), EnvError)
        m.insert(
            "env.get",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: env_result(Type::Optional(Box::new(str_()))),
                llvm_name: "mux_env_get".to_string(),
            },
        );
        m.insert(
            "env.set",
            StdlibItem::Function {
                params: STR_STR_PARAMS.to_vec(),
                ret: env_result(Type::Void),
                llvm_name: "mux_env_set".to_string(),
            },
        );
        m.insert(
            "env.remove",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: env_result(Type::Void),
                llvm_name: "mux_env_remove".to_string(),
            },
        );
        m.insert(
            "env.contains",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: env_result(bool_()),
                llvm_name: "mux_env_contains".to_string(),
            },
        );
        m.insert(
            "env.entries",
            StdlibItem::Function {
                params: Vec::new(),
                ret: env_result(Type::List(Box::new(Type::Tuple(
                    Box::new(str_()),
                    Box::new(str_()),
                )))),
                llvm_name: "mux_env_entries".to_string(),
            },
        );
        m
    };
    pub static ref LOG_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        insert_items! {
            m;
            "log.trace" => StdlibItem::Function { params: STR_PARAM.to_vec(), ret: log_result(Type::Void), llvm_name: "mux_log_trace".to_string() },
            "log.debug" => StdlibItem::Function { params: STR_PARAM.to_vec(), ret: log_result(Type::Void), llvm_name: "mux_log_debug".to_string() },
            "log.info" => StdlibItem::Function { params: STR_PARAM.to_vec(), ret: log_result(Type::Void), llvm_name: "mux_log_info".to_string() },
            "log.warn" => StdlibItem::Function { params: STR_PARAM.to_vec(), ret: log_result(Type::Void), llvm_name: "mux_log_warn".to_string() },
            "log.error" => StdlibItem::Function { params: STR_PARAM.to_vec(), ret: log_result(Type::Void), llvm_name: "mux_log_error".to_string() }
        }
        m
    };
    pub static ref CRYPTO_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        for (name, llvm_name) in [
            ("crypto.sha256", "mux_crypto_sha256"),
            ("crypto.sha512", "mux_crypto_sha512"),
            ("crypto.sha3_256", "mux_crypto_sha3_256"),
            ("crypto.sha3_512", "mux_crypto_sha3_512"),
            ("crypto.blake3", "mux_crypto_blake3"),
        ] {
            m.insert(
                name,
                StdlibItem::Function {
                    params: vec![net_bytes_type()],
                    ret: net_bytes_type(),
                    llvm_name: llvm_name.to_string(),
                },
            );
        }
        for (name, llvm_name) in [
            ("crypto.hmac_sha256", "mux_crypto_hmac_sha256"),
            ("crypto.hmac_sha512", "mux_crypto_hmac_sha512"),
        ] {
            m.insert(
                name,
                StdlibItem::Function {
                    params: vec![net_bytes_type(), net_bytes_type()],
                    ret: crypto_result(net_bytes_type()),
                    llvm_name: llvm_name.to_string(),
                },
            );
        }
        m.insert(
            "crypto.random_bytes",
            StdlibItem::Function {
                params: vec![int()],
                ret: crypto_result(net_bytes_type()),
                llvm_name: "mux_crypto_random_bytes".to_string(),
            },
        );
        m.insert(
            "crypto.random_token",
            StdlibItem::Function {
                params: vec![int()],
                ret: crypto_result(str_()),
                llvm_name: "mux_crypto_random_token".to_string(),
            },
        );
        m.insert(
            "crypto.generate_key",
            StdlibItem::Function {
                params: Vec::new(),
                ret: crypto_result(net_bytes_type()),
                llvm_name: "mux_crypto_generate_key".to_string(),
            },
        );
        for (name, llvm_name) in [
            ("crypto.seal_aes256_gcm", "mux_crypto_seal_aes256_gcm"),
            (
                "crypto.seal_chacha20_poly1305",
                "mux_crypto_seal_chacha20_poly1305",
            ),
            ("crypto.open", "mux_crypto_open"),
        ] {
            m.insert(
                name,
                StdlibItem::Function {
                    params: vec![net_bytes_type(), net_bytes_type(), net_bytes_type()],
                    ret: crypto_result(net_bytes_type()),
                    llvm_name: llvm_name.to_string(),
                },
            );
        }
        for (name, llvm_name) in [
            ("crypto.seal_file", "mux_crypto_seal_file"),
            ("crypto.open_file", "mux_crypto_open_file"),
        ] {
            m.insert(
                name,
                StdlibItem::Function {
                    params: vec![net_bytes_type(), str_(), str_(), net_bytes_type()],
                    ret: crypto_result(Type::Void),
                    llvm_name: llvm_name.to_string(),
                },
            );
        }
        m
    };
    pub static ref DATA_STDLIB_ITEMS: HashMap<&'static str, StdlibItem> = {
        let mut m = HashMap::new();
        // json.parse :: Str -> Result(Json, Str)
        m.insert(
            "json.parse",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: json_result(Type::Named("Json".to_string(), Vec::new())),
                llvm_name: "mux_json_parse".to_string(),
            },
        );
        m.insert(
            "json.parse_with",
            StdlibItem::Function {
                params: vec![str_(), json_duplicate_policy_type()],
                ret: json_result(json_type()),
                llvm_name: "mux_json_parse_with_policy".to_string(),
            },
        );
        m.insert(
            "json.parse_lines",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: json_result(Type::List(Box::new(json_type()))),
                llvm_name: "mux_json_parse_lines".to_string(),
            },
        );
        m.insert(
            "json.parse_lines_with",
            StdlibItem::Function {
                params: vec![str_(), json_duplicate_policy_type()],
                ret: json_result(Type::List(Box::new(json_type()))),
                llvm_name: "mux_json_parse_lines_with_policy".to_string(),
            },
        );
        m.insert(
            "json.parse_reader",
            StdlibItem::Function {
                params: vec![reader_type(), int()],
                ret: json_result(json_type()),
                llvm_name: "mux_json_parse_reader".to_string(),
            },
        );
        m.insert(
            "json.parse_reader_with",
            StdlibItem::Function {
                params: vec![reader_type(), int(), json_duplicate_policy_type()],
                ret: json_result(json_type()),
                llvm_name: "mux_json_parse_reader_with_policy".to_string(),
            },
        );
        m.insert(
            "json.stringify_lines",
            StdlibItem::Function {
                params: vec![Type::List(Box::new(json_type()))],
                ret: json_result(str_()),
                llvm_name: "mux_json_stringify_lines".to_string(),
            },
        );
        m.insert(
            "json.stringify_to",
            StdlibItem::Function {
                params: vec![json_type(), writer_type(), Type::Optional(Box::new(int()))],
                ret: json_result(Type::Void),
                llvm_name: "mux_json_stringify_to".to_string(),
            },
        );
        // json.from_map :: Map(Str, T) -> Result(Json, Str)
        m.insert(
            "json.from_map",
            StdlibItem::Function {
                params: vec![Type::Map(Box::new(str_()), Box::new(Type::Variable("T".to_string())))],
                ret: json_result(Type::Named("Json".to_string(), Vec::new())),
                llvm_name: "mux_json_from_map".to_string(),
            },
        );
        // json.to_map :: Json -> Result(Map(Str, Json), Str)
        m.insert(
            "json.to_map",
            StdlibItem::Function {
                params: vec![Type::Named("Json".to_string(), Vec::new())],
                ret: json_result(Type::Map(Box::new(str_()), Box::new(Type::Named("Json".to_string(), Vec::new())))),
                llvm_name: "mux_json_to_map".to_string(),
            },
        );
        m.insert(
            "json.token_reader",
            StdlibItem::Function {
                params: vec![reader_type(), int()],
                ret: json_result(json_token_reader_type()),
                llvm_name: "mux_json_token_reader_from_reader".to_string(),
            },
        );
        // csv.parse :: Str -> Result(Csv, Str)
        m.insert(
            "csv.parse",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: csv_result(Type::Named("Csv".to_string(), Vec::new())),
                llvm_name: "mux_csv_parse".to_string(),
            },
        );
        // csv.parse_with_headers :: Str -> Result(Csv, Str)
        m.insert(
            "csv.parse_with_headers",
            StdlibItem::Function {
                params: STR_PARAM.to_vec(),
                ret: csv_result(Type::Named("Csv".to_string(), Vec::new())),
                llvm_name: "mux_csv_parse_with_headers".to_string(),
            },
        );
        m.insert(
            "csv.parse_with_options",
            StdlibItem::Function {
                params: STR_INT_INT_BOOL_BOOL_BOOL_PARAMS.to_vec(),
                ret: csv_result(Type::Named("Csv".to_string(), Vec::new())),
                llvm_name: "mux_csv_parse_with_options".to_string(),
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
                ret: sql_result(sql_connection_type()),
                llvm_name: "mux_sql_connect".to_string(),
            },
        );
        m.insert(
            "sql.sqlite_memory",
            StdlibItem::Function {
                params: Vec::new(),
                ret: sql_result(sql_connection_type()),
                llvm_name: "mux_sql_sqlite_memory".to_string(),
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
                params: vec![Type::Primitive(PrimitiveType::Bytes)],
                ret: sql_value_type(),
                llvm_name: "mux_sql_value_bytes".to_string(),
            },
        );
        m.insert(
            "sql.json",
            StdlibItem::Function {
                params: vec![json_type()],
                ret: sql_result(sql_value_type()),
                llvm_name: "mux_sql_value_json".to_string(),
            },
        );
        m.insert(
            "sql.datetime",
            StdlibItem::Function {
                params: vec![datetime_type()],
                ret: sql_result(sql_value_type()),
                llvm_name: "mux_sql_value_datetime".to_string(),
            },
        );
        m.insert(
            "sql.uuid",
            StdlibItem::Function {
                params: vec![uuid_type()],
                ret: sql_result(sql_value_type()),
                llvm_name: "mux_sql_value_uuid".to_string(),
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
        // Assertions are a language primitive rather than a stdlib module.
        // Both arguments are required so every failure has an intentional
        // diagnostic message.
        m.insert("assert", sig(BOOL_STR_PARAMS.to_vec(), Type::Void));
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
            RANDOM_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            IO_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            FS_STDLIB_ITEMS
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
            UUID_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            SYNC_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            PROCESS_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            ENV_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            LOG_STDLIB_ITEMS
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone())),
        )
        .chain(
            CRYPTO_STDLIB_ITEMS
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
        .or_else(|| RANDOM_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| FS_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| MATH_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| DATETIME_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| UUID_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| SYNC_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| PROCESS_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| ENV_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| LOG_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| CRYPTO_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| DATA_STDLIB_ITEMS.get(name).cloned())
        .or_else(|| SQL_STDLIB_ITEMS.get(name).cloned())
}

/// Convert a canonical `StdlibItem` into a `Symbol` suitable for registration in a `SymbolTable`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Type {
        Type::Named(name.to_string(), Vec::new())
    }

    fn method<'a>(symbol: &'a Symbol, interface: &str, name: &str) -> &'a MethodSig {
        symbol
            .interfaces
            .get(interface)
            .and_then(|(_, methods)| methods.get(name))
            .unwrap_or_else(|| panic!("missing {interface}.{name} contract"))
    }

    #[test]
    fn representable_interfaces_pin_the_explicit_method_contracts() {
        let span = Span::new(1, 1);
        let json = json_representable_interface_symbol(span);
        let csv = csv_representable_interface_symbol(span);

        assert_eq!(json.kind, SymbolKind::Interface);
        assert_eq!(csv.kind, SymbolKind::Interface);

        let json_method = method(&json, "JsonRepresentable", "to_json");
        assert!(json_method.params.is_empty());
        assert_eq!(json_method.return_type, json_result(json_type()));
        assert!(!json_method.is_static);

        let csv_method = method(&csv, "CsvRepresentable", "to_csv");
        assert!(csv_method.params.is_empty());
        assert_eq!(csv_method.return_type, csv_result(str_()));
        assert!(!csv_method.is_static);

        assert_eq!(json.interfaces.len(), 1);
        assert_eq!(csv.interfaces.len(), 1);
        assert!(
            json.interfaces["JsonRepresentable"]
                .1
                .contains_key("to_json")
        );
        assert!(csv.interfaces["CsvRepresentable"].1.contains_key("to_csv"));
    }

    #[test]
    fn http_router_oauth_oidc_pins_the_runtime_call_contract() {
        let span = Span::new(1, 1);
        for symbols in [
            net_module_class_symbols(span),
            http_module_class_symbols(span),
        ] {
            let router = symbols
                .get("HttpRouter")
                .expect("HTTP modules must expose HttpRouter");
            let oauth = router
                .methods
                .get("oauth_oidc")
                .expect("HttpRouter must expose oauth_oidc");

            assert_eq!(oauth.params, vec![str_(), str_(), str_()]);
            assert_eq!(oauth.return_type, http_result(Type::Void));
            assert!(!oauth.is_static);
        }

        assert_eq!(named("HttpRouter"), http_router_type());
    }

    #[test]
    fn oauth_client_pins_explicit_discovery_and_token_methods() {
        let symbols = http_module_class_symbols(Span::new(1, 1));
        let client = symbols
            .get("OAuthClient")
            .expect("HTTP modules must expose OAuthClient");
        assert_eq!(client.methods["new"].params, Vec::<Type>::new());
        assert!(client.methods["new"].is_static);
        assert_eq!(
            client.methods["from_config"].params,
            vec![str_(), str_(), str_(), str_()]
        );
        assert_eq!(
            client.methods["authorization_url"].return_type,
            http_result(str_())
        );
        assert_eq!(
            client.methods["exchange_code"].return_type,
            http_result(json_type())
        );
        assert_eq!(
            client.methods["revoke"].return_type,
            http_result(Type::Void)
        );

        let session = symbols
            .get("OAuthSession")
            .expect("HTTP modules must expose OAuthSession");
        assert_eq!(
            session.methods["from_token_response"].params,
            vec![json_type()]
        );
        assert_eq!(session.methods["refresh"].params, vec![oauth_client_type()]);
        assert_eq!(
            session.methods["refresh_token"].return_type,
            http_result(Type::Optional(Box::new(str_())))
        );

        let config = symbols
            .get("HttpServerConfig")
            .expect("HTTP modules must expose HttpServerConfig");
        assert_eq!(config.fields["worker_count"], (int(), false));
        assert_eq!(config.fields["heartbeat_interval_ms"], (int(), false));
    }

    #[test]
    fn result_set_operations_return_typed_sql_errors() {
        let symbols = sql_module_class_symbols(Span::new(1, 1));
        let result_set = symbols
            .get("ResultSet")
            .expect("SQL module must expose ResultSet");

        assert_eq!(
            result_set.methods["close"].return_type,
            sql_result(Type::Void)
        );
        assert_eq!(
            result_set.methods["rows"].return_type,
            sql_result(Type::List(Box::new(sql_row_type())))
        );
        assert_eq!(
            result_set.methods["next"].return_type,
            sql_result(Type::Optional(Box::new(sql_row_type())))
        );
        assert_eq!(
            result_set.methods["next_batch"].return_type,
            sql_result(Type::List(Box::new(sql_row_type())))
        );
    }
}
