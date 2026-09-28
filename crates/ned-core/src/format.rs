//! External formatters and their configuration (command-language spec, §6.4).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::lang::Language;

/// A file's formatter, ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatter {
    /// The basename of its program, as shown in the output.
    pub name: String,
    /// Commands with placeholders and programs resolved, tried in order while
    /// the program isn't found.
    pub commands: Vec<Vec<String>>,
    /// The directory to run in: the file's.
    pub dir: PathBuf,
}

/// The formatter settings from every config file read so far.
#[derive(Debug)]
pub struct Formatters {
    user: Option<Layer>,
    /// Parsed `.ned.toml` files by directory; `None` where there is none.
    layers: HashMap<PathBuf, Option<Layer>>,
}

/// The settings from one config file.
#[derive(Debug)]
struct Layer {
    /// The directory holding the config file.
    dir: PathBuf,
    format: HashMap<Language, Entry>,
}

#[derive(Debug)]
enum Entry {
    Command(Vec<String>),
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// The config file, with the line and column when known.
    pub location: String,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: invalid config: {}", self.location, self.message)
    }
}

impl std::error::Error for ConfigError {}

/// The user config file: `$XDG_CONFIG_HOME/ned/config.toml`, else
/// `~/.config/ned/config.toml`.
pub fn user_config() -> Option<PathBuf> {
    None
}

impl Formatters {
    /// Reads the user config at `user`, if it exists.
    pub fn new(user: Option<&Path>) -> Result<Self, ConfigError> {
        let _ = user;
        Ok(Formatters {
            user: None,
            layers: HashMap::new(),
        })
    }

    /// The formatter for `path` in `lang`, or `None` if it has none.
    pub fn get(&mut self, path: &Path, lang: Language) -> Result<Option<Formatter>, ConfigError> {
        let _ = (path, lang, &self.user, &mut self.layers);
        Ok(None)
    }
}

