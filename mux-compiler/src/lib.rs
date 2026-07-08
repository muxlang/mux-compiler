pub mod ast;
pub mod codegen;
pub mod diagnostic;
mod embedded_std {
    include!(concat!(env!("OUT_DIR"), "/embedded_std.rs"));
}
pub mod lexer;
pub mod module_resolver;
pub mod parser;
pub mod semantics;
pub mod source;
pub mod spinner;
