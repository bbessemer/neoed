//! Resolving selectors to spans of files (command-language spec, §3).

use std::ops::Range;

use crate::buffer::Buffer;
use crate::exec::ExecError;
use crate::script::ast::Target;

/// A file in the file set, with its path as the user wrote it.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
    pub buffer: Buffer,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, text: String) -> Self {
        let buffer = Buffer::new(&text);
        SourceFile {
            path: path.into(),
            text,
            buffer,
        }
    }
}

/// A selected span of `files[file]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub file: usize,
    pub range: Range<usize>,
}

/// Resolves `target` against every file in `files`, enforcing the ambiguity
/// rules of §3.5. `src` is the script, for error messages.
pub fn resolve(target: &Target, files: &[SourceFile], src: &str) -> Result<Vec<Match>, ExecError> {
    let _ = (target, files, src);
    todo!()
}
