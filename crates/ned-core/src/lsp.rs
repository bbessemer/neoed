//! Language servers: which one serves each language (spec §1.1).

use std::path::Path;

use crate::config::{Config, ConfigError};
use crate::lang::Language;

impl Config {
    /// The server command for `lang` in the workspace at `root`, with its
    /// program resolved, or `None` if the language has no server.
    pub fn server(
        &mut self,
        root: &Path,
        lang: Language,
    ) -> Result<Option<Vec<String>>, ConfigError> {
        let _ = (root, lang);
        todo!()
    }
}

/// The LSP `languageId` of `lang`.
pub fn language_id(lang: Language) -> &'static str {
    let _ = lang;
    todo!()
}
