//! Settings from config files: the user config, then every `.ned.toml` from
//! the filesystem root down, the nearest winning (spec §1.1, §6.4).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::{env, fmt, fs, io};

use serde::Deserialize;
use toml::{Spanned, Value};

use crate::lang::Language;
use crate::lsp::Severity;

const CONFIG_FILE: &str = ".ned.toml";

/// The settings from every config file read so far.
#[derive(Debug)]
pub struct Config {
    user: Option<Layer>,
    /// Parsed `.ned.toml` files by directory; `None` where there is none.
    layers: HashMap<PathBuf, Option<Layer>>,
    /// New texts of config files by absolute path, read before the disk.
    written: HashMap<PathBuf, String>,
}

/// The settings from one config file.
#[derive(Debug)]
pub(crate) struct Layer {
    /// The directory holding the config file.
    pub(crate) dir: PathBuf,
    pub(crate) format: HashMap<Language, Entry>,
    pub(crate) lsp: HashMap<Language, Entry>,
    pub(crate) idle_timeout: Option<u64>,
    pub(crate) check_show: Option<Severity>,
    pub(crate) lsp_timeout: Option<u64>,

    /// `Some(None)` for `block = false`.
    pub(crate) check_block: Option<Option<Severity>>,
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
    #[serde(default)]
    check: RawCheck,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDaemon {
    idle_timeout: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawCheck {
    show: Option<Severity>,

    block: Option<RawBlock>,
}

/// `[check] block`: a level, or `false`.
#[derive(Deserialize)]
#[serde(
    untagged,
    expecting = "a level (\"error\", \"warning\", \"info\" or \"hint\") or false"
)]
enum RawBlock {
    Level(Severity),
    Off(bool),
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
            written: HashMap::new(),
        })
    }

    /// Reads config files from `written`, the new texts by absolute path,
    /// before the disk.
    pub(crate) fn overlay(&mut self, written: &HashMap<PathBuf, &str>) {
        for (path, text) in written {
            if path.file_name() == Some(CONFIG_FILE.as_ref()) {
                self.layers.remove(path.parent().unwrap_or(path));
                self.written.insert(path.clone(), text.to_string());
            }
        }
    }

    /// The config files that apply in `dir` (absolute), nearest first, then
    /// the user config.
    pub(crate) fn layers(&mut self, dir: &Path) -> Result<Vec<&Layer>, ConfigError> {
        for ancestor in dir.ancestors() {
            if !self.layers.contains_key(ancestor) {
                let path = ancestor.join(CONFIG_FILE);
                let layer = match self.written.get(&path) {
                    Some(text) => Some(parse(&path, text)?),
                    None => load(&path)?,
                };
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
        let dir = std::path::absolute(dir).map_err(|err| io_error(dir, &err))?;
        Ok(self
            .layers(&dir)?
            .into_iter()
            .find_map(|layer| layer.idle_timeout))
    }

    /// `[check] show` for `dir`.
    pub fn check_show(&mut self, dir: &Path) -> Result<Option<Severity>, ConfigError> {
        let dir = std::path::absolute(dir).map_err(|err| io_error(dir, &err))?;
        Ok(self
            .layers(&dir)?
            .into_iter()
            .find_map(|layer| layer.check_show))
    }

    /// `[lsp] timeout` for `dir`, in seconds.
    pub fn lsp_timeout(&mut self, dir: &Path) -> Result<Option<u64>, ConfigError> {
        let dir = std::path::absolute(dir).map_err(|err| io_error(dir, &err))?;
        Ok(self
            .layers(&dir)?
            .into_iter()
            .find_map(|layer| layer.lsp_timeout))
    }

    /// `[check] block` for `dir`: `Some(None)` if it's `false`.
    pub fn check_block(&mut self, dir: &Path) -> Result<Option<Option<Severity>>, ConfigError> {
        let dir = std::path::absolute(dir).map_err(|err| io_error(dir, &err))?;
        Ok(self
            .layers(&dir)?
            .into_iter()
            .find_map(|layer| layer.check_block))
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
    parse(path, &text).map(Some)
}

/// The config file at `path`, whose text is `text`.
fn parse(path: &Path, text: &str) -> Result<Layer, ConfigError> {
    let error = |span: Option<std::ops::Range<usize>>, message: String| {
        let mut location = display(path);
        if let Some(span) = span {
            let (line, col) = crate::script::error::location(text, span.start);
            location = format!("{location}:{line}:{col}");
        }
        ConfigError { location, message }
    };
    let raw: RawConfig =
        toml::from_str(text).map_err(|err| error(err.span(), err.message().trim().into()))?;
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
    let mut lsp = raw.lsp;
    let lsp_timeout = match lsp.remove("timeout") {
        None => None,
        Some(value) => match value.get_ref() {
            Value::Integer(secs) if *secs >= 0 => Some(*secs as u64),
            _ => {
                return Err(error(
                    Some(value.span()),
                    "`timeout` must be a number of seconds".into(),
                ));
            }
        },
    };
    Ok(Layer {
        dir: path.parent().unwrap_or(path).to_path_buf(),
        format: entries(raw.format)?,
        lsp: entries(lsp)?,
        lsp_timeout,

        idle_timeout: raw.daemon.idle_timeout,
        check_show: raw.check.show,

        check_block: match raw.check.block {
            None => None,
            Some(RawBlock::Level(level)) => Some(Some(level)),
            Some(RawBlock::Off(false)) => Some(None),
            Some(RawBlock::Off(true)) => {
                return Err(error(None, "`block` must be a level or false".into()));
            }
        },
    })
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

#[cfg(test)]
pub(crate) mod tests {
    use tempfile::TempDir;

    use super::*;

    pub(crate) fn tree(files: &[(&str, &str)]) -> TempDir {
        let root = TempDir::new().unwrap();
        for (path, text) in files {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        root
    }

    #[test]
    fn idle_timeout_nearest_wins() {
        let root = tree(&[
            ("config.toml", "[daemon]\nidle_timeout = 60\n"),
            ("ws/.ned.toml", "[daemon]\nidle_timeout = 1800\n"),
            ("ws/sub/.ned.toml", "[format]\nrust = false\n"),
        ]);
        let mut config = Config::new(Some(&root.path().join("config.toml"))).unwrap();
        assert_eq!(
            config.idle_timeout(&root.path().join("ws/sub")),
            Ok(Some(1800))
        );
        assert_eq!(config.idle_timeout(root.path()), Ok(Some(60)));
    }

    #[test]
    fn overlay_texts_replace_the_disk_even_once_read() {
        let root = tree(&[
            (".ned.toml", "[daemon]\nidle_timeout = 60\n"),
            ("ws/.ned.toml", "[daemon]\nidle_timeout = 1800\n"),
        ]);
        let (ws, new) = (root.path().join("ws"), root.path().join("ws/new"));
        let mut config = Config::new(None).unwrap();
        assert_eq!(config.idle_timeout(&new), Ok(Some(1800)));
        config.overlay(&HashMap::from([
            (ws.join(".ned.toml"), "[daemon]\nidle_timeout = 5\n"),
            (new.join(".ned.toml"), "[check]\nshow = \"error\"\n"),
        ]));
        assert_eq!(config.idle_timeout(&new), Ok(Some(5)));
        assert_eq!(config.check_show(&new), Ok(Some(Severity::Error)));
        assert_eq!(config.idle_timeout(root.path()), Ok(Some(60)));
    }

    #[test]
    fn overlay_errors_point_into_the_new_text() {
        let root = tree(&[]);
        let mut config = Config::new(None).unwrap();
        config.overlay(&HashMap::from([(
            root.path().join(".ned.toml"),
            "\n[lsp]\nrust = \"rust-analyzer\"\n",
        )]));
        let err = config.layers(root.path()).map(|_| ()).unwrap_err();
        assert!(err.location.ends_with(".ned.toml:3:8"), "{err}");
    }

    #[test]
    fn no_idle_timeout_without_a_setting() {
        let root = tree(&[(".ned.toml", "[daemon]\n")]);
        assert_eq!(
            Config::new(None).unwrap().idle_timeout(root.path()),
            Ok(None)
        );
    }

    #[test]
    fn unknown_daemon_settings_are_errors() {
        let root = tree(&[(".ned.toml", "[daemon]\nidle = 3\n")]);
        let err = Config::new(None)
            .unwrap()
            .idle_timeout(root.path())
            .unwrap_err();
        assert!(err.location.ends_with(".ned.toml:2:1"), "{err}");
        assert!(err.message.contains("unknown field `idle`"), "{err}");
    }

    #[test]
    fn lsp_values_must_be_commands() {
        let root = tree(&[(".ned.toml", "[lsp]\nrust = \"rust-analyzer\"\n")]);
        let err = Config::new(None)
            .unwrap()
            .layers(root.path())
            .map(|_| ())
            .unwrap_err();
        assert!(err.location.ends_with(".ned.toml:2:8"), "{err}");
        assert!(err.message.contains("must be a command"), "{err}");
    }

    #[test]
    fn check_settings_nearest_wins() {
        let root = tree(&[
            ("config.toml", "[check]\nshow = \"hint\"\n"),
            ("ws/.ned.toml", "[check]\nshow = \"error\"\n"),
        ]);
        let mut config = Config::new(Some(&root.path().join("config.toml"))).unwrap();
        let ws = root.path().join("ws");
        assert_eq!(config.check_show(&ws), Ok(Some(Severity::Error)));
        assert_eq!(config.check_show(root.path()), Ok(Some(Severity::Hint)));
        let mut none = Config::new(None).unwrap();
        assert_eq!(none.check_show(&ws), Ok(Some(Severity::Error)));
    }

    #[test]
    fn the_lsp_timeout_sits_beside_the_servers_and_nearest_wins() {
        let root = tree(&[
            ("config.toml", "[lsp]\ntimeout = 5\n"),
            ("ws/.ned.toml", "[lsp]\nrust = [\"ra\"]\ntimeout = 9\n"),
        ]);
        let mut config = Config::new(Some(&root.path().join("config.toml"))).unwrap();
        let ws = root.path().join("ws");
        assert_eq!(config.lsp_timeout(&ws), Ok(Some(9)));
        assert_eq!(config.lsp_timeout(root.path()), Ok(Some(5)));
        assert_eq!(
            config.layers(&ws).unwrap()[0].lsp.len(),
            1,
            "rust, not timeout"
        );
        assert_eq!(Config::new(None).unwrap().lsp_timeout(&ws), Ok(Some(9)));
    }

    #[test]
    fn bad_timeouts_are_errors() {
        let root = tree(&[(".ned.toml", "[check]\ntimeout = 5\n")]);
        let err = Config::new(None)
            .unwrap()
            .lsp_timeout(root.path())
            .unwrap_err();
        assert!(err.message.contains("unknown field `timeout`"), "{err}");
        let root = tree(&[(".ned.toml", "[lsp]\ntimeout = \"soon\"\n")]);
        let err = Config::new(None)
            .unwrap()
            .lsp_timeout(root.path())
            .unwrap_err();
        assert!(err.location.ends_with(".ned.toml:2:11"), "{err}");
        assert!(err.message.contains("seconds"), "{err}");
    }

    #[test]
    fn bad_check_levels_are_errors() {
        let root = tree(&[(".ned.toml", "[check]\nshow = \"warnings\"\n")]);
        let err = Config::new(None)
            .unwrap()
            .check_show(root.path())
            .unwrap_err();
        assert!(err.location.ends_with(".ned.toml:2:8"), "{err}");
        assert!(err.message.contains("warning"), "{err}");
    }

    #[test]
    fn block_is_a_level_or_false() {
        let root = tree(&[
            ("config.toml", "[check]\nblock = \"warning\"\n"),
            ("ws/.ned.toml", "[check]\nblock = false\n"),
        ]);
        let mut config = Config::new(Some(&root.path().join("config.toml"))).unwrap();
        assert_eq!(
            config.check_block(root.path()),
            Ok(Some(Some(Severity::Warning)))
        );
        assert_eq!(config.check_block(&root.path().join("ws")), Ok(Some(None)));
        assert_eq!(
            Config::new(None).unwrap().check_block(root.path()),
            Ok(None)
        );
        for bad in ["true", "\"errors\""] {
            let root = tree(&[(".ned.toml", &format!("[check]\nblock = {bad}\n"))]);
            let err = Config::new(None)
                .unwrap()
                .check_block(root.path())
                .unwrap_err();
            assert!(err.message.contains("level"), "{bad}: {err}");
        }
    }
}
