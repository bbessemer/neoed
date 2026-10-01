//! Filters, `[...]` after a step (command-language spec, §3.9), lexed as one
//! token.

use super::Lexer;
use crate::script::ast::Filter;
use crate::script::error::ParseError;

impl Lexer<'_> {
    /// The filter at `[`, through its `]`.
    pub(super) fn filter(&mut self) -> Result<Filter, ParseError> {
        todo!()
    }
}
