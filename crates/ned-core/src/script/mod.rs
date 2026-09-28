//! The command language: lexing and parsing scripts (docs/command-language.md).

pub mod ast;
pub mod error;
pub mod lexer;

pub use error::{ParseError, ParseErrorKind};
