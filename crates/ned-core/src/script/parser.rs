//! Parser from script text to [`Script`] (command-language spec, §2.2).

use super::ast::Script;
use super::error::ParseError;

pub fn parse(src: &str) -> Result<Script, ParseError> {
    todo!()
}
