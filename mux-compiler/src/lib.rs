pub mod analysis;
pub mod ast;
pub mod codegen;
pub mod diagnostic;
pub mod docgen;
mod embedded_std {
    include!(concat!(env!("OUT_DIR"), "/embedded_std.rs"));
}
pub mod formatter;
pub mod lexer;
pub mod module_resolver;
pub mod muxdoc;
pub mod parser;
pub mod semantics;
pub mod source;
pub mod spinner;
pub mod syntax;
