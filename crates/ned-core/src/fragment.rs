//! Parsing code fragments (command-language spec, §3.10). tree-sitter always
//! parses from a grammar's start rule, so a fragment that only parses inside
//! other code (a method, a match arm, a field) is also tried inside each of
//! its language's builders, `queries/<lang>/builders.scm`.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use tree_sitter::{Node, Tree};

use crate::lang::Language;
use crate::template::Template;

/// How to build a node of `kind` around a fragment: `(build KIND PART...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Builder {
    pub kind: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// A string: literal text.
    Text(String),
    /// A symbol other than `_`: one of the kind's fields.
    Field(String),
    /// `_`, where the fragment goes.
    Hole,
}

/// A malformed builders file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BuilderError {
    pub message: String,
    pub span: Range<usize>,
}

/// What a grammar's `node-types.json` says each named kind can hold.
#[derive(Debug, Default)]
pub struct NodeTypes {
    fields: HashMap<String, HashSet<String>>,
    contains: HashMap<String, HashSet<String>>,
}

/// One reading of a fragment: the program it parsed in, and where the
/// fragment and its holes are in it.
#[derive(Debug)]
pub struct Fragment {
    pub text: String,
    pub tree: Tree,
    /// The byte range of the fragment's root node, or of its run of root
    /// nodes.
    pub roots: Range<usize>,
    /// Each hole's range in `text`, or `None` where it's literal text (in a
    /// string or comment).
    pub holes: Vec<Option<Range<usize>>>,
    /// The index of the builder it parsed in, or `None` if it parsed alone.
    pub builder: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FragmentError {
    #[error("the pattern doesn't parse as {lang}; add the code around it, or use query{{}}")]
    NoParse {
        lang: Language,
        /// Where parsing failed, in the template's text.
        at: usize,
    },
    #[error("a placeholder must stand for a whole node; write `@@` for a literal `@`")]
    Fused {
        /// The placeholder, in the template's text.
        span: Range<usize>,
    },
}

impl Builder {
    /// Reads a builders file, checking its fields against `types`.
    pub fn read_all(_src: &str, _types: &NodeTypes) -> Result<Vec<Builder>, BuilderError> {
        unimplemented!()
    }
}

impl NodeTypes {
    pub fn read(_json: &str) -> NodeTypes {
        unimplemented!()
    }

    pub fn has_kind(&self, _kind: &str) -> bool {
        unimplemented!()
    }

    pub fn has_field(&self, _kind: &str, _field: &str) -> bool {
        unimplemented!()
    }

    /// Whether a `parent` node can have a named `child` node, as a field or
    /// child, through any supertype.
    pub fn can_contain(&self, _parent: &str, _child: &str) -> bool {
        unimplemented!()
    }
}

impl Fragment {
    /// The fragment's root node, or its run of sibling root nodes.
    pub fn roots(&self) -> Vec<Node<'_>> {
        unimplemented!()
    }

    /// Hole `i`'s node: the outermost node spanning exactly its placeholder,
    /// within the roots. `None` if the hole is literal.
    pub fn hole(&self, _i: usize) -> Option<Node<'_>> {
        unimplemented!()
    }
}

/// Every reading of `template` in `lang` that parses without errors: alone,
/// then inside each builder. Readings with the same root kinds count once.
pub fn parse(_lang: Language, _template: &Template) -> Result<Vec<Fragment>, FragmentError> {
    unimplemented!()
}
