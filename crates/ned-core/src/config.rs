//! Settings from config files: the user config, then every `.ned.toml` from
//! the filesystem root down, the nearest winning (spec §1.1, §6.4).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::{env, fmt, fs, io};

use serde::Deserialize;
use toml::{Spanned, Value};

use crate::lang::Language;

const CONFIG_FILE: &str = ".ned.toml";

/// The settings from every config file read so far.
#[derive(Debug)]
pub struct Config {
    user: Option<Layer>,
    /// Parsed `.ned.toml` files by directory; `None` where there is none.
    layers: HashMap<PathBuf, Option<Layer>>,
}

/// The settings from one config file.
#[derive(Debug)]
pub(crate) struct Layer {
    /// The directory holding the config file.
    pub(crate) dir: PathBuf,
    pub(crate) format: HashMap<Language, Entry>,
    pub(crate) lsp: HashMap<Language, Entry>,
    pub(crate) idle_timeout: Option<u64>,
}

/// A tool setting for one language.
#[derive(Debug)]
pub(crate) enum Entry {
    Command(Vec<String>),
    Off,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    format: BTreeMap<Spanned<String>, Spanned<Value>>,
    #[serde(default)]
    lsp: BTreeMap<Spanned<String>, Spanned<Value>>,
    #[serde(default)]
    daemon: RawDaemon,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDaemon {
    idle_timeout: Option<u64>,
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

impl Config {
    /// Reads the user config at `user`, if it exists.
    pub fn new(user: Option<&Path>) -> Result<Self, ConfigError> {
        Ok(Config {
            user: user.map(load).transpose()?.flatten(),
            layers: HashMap::new(),
        })
    }

    /// The config files that apply in `dir` (absolute), nearest first, then
    /// the user config.
    pub(crate) fn layers(&mut self, dir: &Path) -> Result<Vec<&Layer>, ConfigError> {
        for ancestor in dir.ancestors() {
            if !self.layers.contains_key(ancestor) {
                let layer = load(&ancestor.join(CONFIG_FILE))?;
                self.layers.insert(ancestor.to_path_buf(), layer);
            }
        }
        Ok(dir
            .ancestors()
            .filter_map(|a| self.layers[a].as_ref())
            .chain(&self.user)
            .collect())
    }

    /// `[daemon] idle_timeout` for a daemon in `dir`, in seconds.
    pub fn idle_timeout(&mut self, dir: &Path) -> Result<Option<u64>, ConfigError> {
        let _ = dir;
        todo!()
    }
}

/// The user config file: `$XDG_CONFIG_HOME/ned/config.toml`, else
/// `~/.config/ned/config.toml`.
pub fn user_config() -> Option<PathBuf> {
    let absolute = |var| {
        env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let config = absolute("XDG_CONFIG_HOME").or_else(|| Some(absolute("HOME")?.join(".config")))?;
    Some(config.join("ned/config.toml"))
}

/// The config file at `path`, or `None` if there is none.
fn load(path: &Path) -> Result<Option<Layer>, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(path, &err)),
    };
    let error = |span: Option<std::ops::Range<usize>>, message: String| {
        let mut location = display(path);
        if let Some(span) = span {
            let (line, col) = crate::script::error::location(&text, span.start);
            location = format!("{location}:{line}:{col}");
        }
        ConfigError { location, message }
    };
    let raw: RawConfig =
        toml::from_str(&text).map_err(|err| error(err.span(), err.message().trim().into()))?;
    let entries = |table: BTreeMap<Spanned<String>, Spanned<Value>>| {
        let mut entries = HashMap::new();
        for (key, value) in table {
            let lang: Language = key
                .get_ref()
                .parse()
                .map_err(|message| error(Some(key.span()), message))?;
            let entry = match value.get_ref() {
                Value::Boolean(false) => Entry::Off,
                Value::Array(words) if words.is_empty() => {
                    return Err(error(
                        Some(value.span()),
                        format!("`{lang}` has an empty command"),
                    ));
                }
                Value::Array(words) => {
                    match words.iter().map(|w| w.as_str().map(String::from)).collect() {
                        Some(command) => Entry::Command(command),
                        None => return Err(error(Some(value.span()), not_a_command(lang))),
                    }
                }
                _ => return Err(error(Some(value.span()), not_a_command(lang))),
            };
            entries.insert(lang, entry);
        }
        Ok(entries)
    };
    Ok(Some(Layer {
        dir: path.parent().unwrap_or(path).to_path_buf(),
        format: entries(raw.format)?,
        lsp: entries(raw.lsp)?,
        idle_timeout: raw.daemon.idle_timeout,
    }))
}

/// Where to run `program`: relative to `base`, the directory of the config
/// that named it, if it's a path; else from the nearest `node_modules/.bin`
/// above `dir`, or as is, for a `PATH` lookup.
pub(crate) fn program(program: &str, base: Option<&Path>, dir: &Path) -> String {
    let found = if program.contains('/') {
        base.map(|b| b.join(program))
    } else {
        dir.ancestors()
            .map(|a| a.join("node_modules/.bin").join(program))
            .find(|p| p.is_file())
    };
    found.map_or(program.into(), |p| p.to_string_lossy().into_owned())
}

fn not_a_command(lang: Language) -> String {
    format!("`{lang}` must be a command (an array of strings) or false")
}

pub(crate) fn io_error(path: &Path, err: &io::Error) -> ConfigError {
    ConfigError {
        location: display(path),
        message: err.to_string(),
    }
}

/// `path`, relative to the working directory if it's inside it.
fn display(path: &Path) -> String {
    let cwd = env::current_dir().unwrap_or_default();
    path.strip_prefix(&cwd)
        .unwrap_or(path)
        .display()
        .to_string()
}
