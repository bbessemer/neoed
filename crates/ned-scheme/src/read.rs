//! The reader: source text to [`Syntax`].

use crate::{ReadError, Syntax};

/// Reads every datum in `src`.
pub fn read_all(_src: &str) -> Result<Vec<Syntax>, ReadError> {
    unimplemented!()
}
