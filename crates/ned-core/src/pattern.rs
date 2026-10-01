//! Matching syntax patterns against syntax trees (command-language spec,
//! §3.10): node by node, skipping comments on both sides and tokens the
//! pattern leaves out.

use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::Tree;

use crate::fragment::Fragment;
use crate::lang::Language;

/// A pattern compiled for one language: each of its readings.
#[derive(Debug)]
pub struct Pattern {
    readings: Vec<Reading>,
}

#[derive(Debug)]
struct Reading {
    fragment: Fragment,
    /// The holes' nodes, by node id: the hole's name (`None` for `@_`), and
    /// whether it's a run.
    holes: HashMap<usize, (Option<String>, bool)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMatch {
    pub range: Range<usize>,
    /// Each named placeholder's capture, in the pattern's order.
    pub captures: Vec<(String, Range<usize>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    #[error("doesn't parse as {lang} at `{at}`; add the code around it, or use query{{}}")]
    NoParse { lang: Language, at: String },
    #[error("has `{placeholder}` where a whole node must be; write `@@` for a literal `@`")]
    Fused { placeholder: String },
}

impl Pattern {
    /// Compiles the pattern `src` for `lang`, ignoring the common indentation
    /// of its lines.
    pub fn compile(_lang: Language, _src: &str) -> Result<Pattern, PatternError> {
        unimplemented!()
    }

    /// The matches in `tree`, the tree of `text`, that lie within `range`, in
    /// source order. A match inside an earlier one is skipped.
    pub fn find(&self, _tree: &Tree, _text: &str, _range: Range<usize>) -> Vec<PatternMatch> {
        unimplemented!()
    }
}
