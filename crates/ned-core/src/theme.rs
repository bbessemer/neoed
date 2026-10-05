//! Themes: a colour for each highlight capture and output role on terminals
//! of more than 16 colours (command-language spec, §6.6).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use toml::Spanned;

use crate::color::{BadColor, Rgb};
use crate::config::{ConfigError, error_at, io_error};
use crate::highlight::prefixes;
use crate::style::Role;

/// The themes built into `ned`, by name.
const BUILTINS: &[(&str, &str)] = &[
    ("default-dark", include_str!("../themes/default-dark.toml")),
    (
        "default-light",
        include_str!("../themes/default-light.toml"),
    ),
];

/// The `[theme.ui]` keys.
const UI_KEYS: &[&str] = &[
    "header",
    "line-number",
    "kind",
    "dim",
    "hunk-header",
    "error",
    "warning",
    "info",
    "note",
];

/// How a piece of output looks: a colour and attributes, any of them unset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Look {
    pub color: Option<Rgb>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Whether the theme is for a dark background.
    pub dark: bool,
    pub added: Rgb,
    pub removed: Rgb,
    syntax: HashMap<String, Look>,
    ui: HashMap<&'static str, Look>,
}

impl Theme {
    /// The look of code highlighted as `capture`, from its longest dotted
    /// prefix the theme sets.
    pub fn syntax(&self, capture: &str) -> Option<&Look> {
        prefixes(capture).find_map(|p| self.syntax.get(p))
    }

    /// The look the theme gives `role`, if it sets one.
    pub fn ui(&self, role: Role) -> Option<&Look> {
        let key = match role {
            Role::Header => "header",
            Role::LineNumber => "line-number",
            Role::Kind => "kind",
            Role::Dim => "dim",
            Role::HunkHeader => "hunk-header",
            Role::Error => "error",
            Role::Warning => "warning",
            Role::Info => "info",
            Role::Note => "note",
            Role::Added | Role::Removed | Role::Code(_) => return None,
        };
        self.ui.get(key)
    }
}

/// A `[theme]` table, or a theme file's keys, as written.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawTheme {
    from: Option<Spanned<String>>,
    dark: Option<bool>,
    added: Option<Spanned<String>>,
    removed: Option<Spanned<String>>,
    #[serde(default)]
    syntax: BTreeMap<String, Spanned<String>>,
    #[serde(default)]
    ui: BTreeMap<Spanned<String>, Spanned<String>>,
}

/// The user config's `theme`: a theme's name or path, or a `[theme]` table.
pub(crate) enum Setting {
    Name(String),
    Table(RawTheme),
}

impl<'de> Deserialize<'de> for Setting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Expected;
        impl<'de> Visitor<'de> for Expected {
            type Value = Setting;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a theme's name or path, or a table")
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<Setting, E> {
                Ok(Setting::Name(name.into()))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Setting, A::Error> {
                RawTheme::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(Setting::Table)
            }
        }
        deserializer.deserialize_any(Expected)
    }
}

/// The theme `setting` gives, read from the config file `file`, whose text
/// is `text`. Names not built in are looked up as `NAME.toml` in `themes`.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the user config from themes/config on")
)]
pub(crate) fn resolve(
    setting: Spanned<Setting>,
    file: &Path,
    text: &str,
    themes: &Path,
) -> Result<Theme, ConfigError> {
    let span = setting.span();
    let raw = match setting.into_inner() {
        Setting::Name(name) => RawTheme {
            from: Some(Spanned::new(span.clone(), name)),
            ..RawTheme::default()
        },
        Setting::Table(raw) => raw,
    };
    let theme = layer(raw, file, text, themes, &mut Vec::new())?;
    let missing = |key: &str| {
        let message = format!("the theme has no `{key}`; set it, or `from` a theme that does");
        error_at(file, text, Some(span.clone()), message)
    };
    Ok(Theme {
        dark: theme.dark.ok_or_else(|| missing("dark"))?,
        added: theme.added.ok_or_else(|| missing("added"))?,
        removed: theme.removed.ok_or_else(|| missing("removed"))?,
        syntax: theme.syntax,
        ui: theme.ui,
    })
}

/// A theme as far as one file and those its `from` names give it.
#[derive(Default)]
struct Partial {
    dark: Option<bool>,
    added: Option<Rgb>,
    removed: Option<Rgb>,
    syntax: HashMap<String, Look>,
    ui: HashMap<&'static str, Look>,
}

/// `raw`, read from `path`, whose text is `text`, over the theme its `from`
/// names. `chain` holds the theme files read so far, to catch a cycle.
fn layer(
    raw: RawTheme,
    path: &Path,
    text: &str,
    themes: &Path,
    chain: &mut Vec<PathBuf>,
) -> Result<Partial, ConfigError> {
    let error = |span, message| error_at(path, text, Some(span), message);
    let mut theme = match &raw.from {
        None => Partial::default(),
        Some(from) => {
            let name = from.get_ref();
            let is_path = name.contains('/') || name.ends_with(".toml");
            let builtin = BUILTINS.iter().find(|(n, _)| !is_path && n == name);
            let (file, source) = match builtin {
                Some((_, source)) => (PathBuf::from(format!("<{name}>")), source.to_string()),
                None => {
                    let file = match is_path {
                        true => path.parent().unwrap_or(path).join(name),
                        false => themes.join(format!("{name}.toml")),
                    };
                    match fs::read_to_string(&file) {
                        Ok(source) => (file, source),
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            let message = match is_path {
                                true => format!(
                                    "no theme file {}; fix the path, which is relative to this file",
                                    file.display()
                                ),
                                false => format!(
                                    "no theme `{name}`: it isn't built in (default-dark, default-light) and there is no {}; write it, or use a built-in",
                                    file.display()
                                ),
                            };
                            return Err(error(from.span(), message));
                        }
                        Err(err) => return Err(io_error(&file, &err)),
                    }
                }
            };
            let id = fs::canonicalize(&file).unwrap_or(file.clone());
            if chain.contains(&id) {
                let message = format!("theme `{name}` inherits from itself; remove a `from`");
                return Err(error(from.span(), message));
            }
            chain.push(id);
            let raw: RawTheme = toml::from_str(&source)
                .map_err(|e| error_at(&file, &source, e.span(), e.message().trim().into()))?;
            layer(raw, &file, &source, themes, chain)?
        }
    };
    let color = |value: &Spanned<String>| {
        value
            .get_ref()
            .parse::<Rgb>()
            .map_err(|e| error(value.span(), e.to_string()))
    };
    let look = |value: &Spanned<String>| look(value.get_ref()).map_err(|m| error(value.span(), m));
    theme.dark = raw.dark.or(theme.dark);
    if let Some(added) = &raw.added {
        theme.added = Some(color(added)?);
    }
    if let Some(removed) = &raw.removed {
        theme.removed = Some(color(removed)?);
    }
    for (capture, value) in &raw.syntax {
        theme.syntax.insert(capture.clone(), look(value)?);
    }
    for (key, value) in &raw.ui {
        let Some(key) = UI_KEYS.iter().find(|k| *k == key.get_ref()) else {
            let message = format!(
                "unknown ui key `{}`; use {}",
                key.get_ref(),
                UI_KEYS.join(", ")
            );
            return Err(error(key.span(), message));
        };
        theme.ui.insert(key, look(value)?);
    }
    Ok(theme)
}

/// The look a `[theme]` value writes: a colour and attributes.
fn look(value: &str) -> Result<Look, String> {
    const FIX: &str = "use `#rrggbb`, bold, dim, italic or underline";
    let mut look = Look::default();
    if value.trim().is_empty() {
        return Err(format!("empty look; {FIX}"));
    }
    for word in value.split_whitespace() {
        match word {
            "bold" => look.bold = true,
            "dim" => look.dim = true,
            "italic" => look.italic = true,
            "underline" => look.underline = true,
            _ if word.starts_with('#') => {
                if look.color.is_some() {
                    return Err(format!("two colours in `{value}`; keep one"));
                }
                look.color = Some(word.parse().map_err(|e: BadColor| e.to_string())?);
            }
            _ => return Err(format!("`{word}` is not a colour or attribute; {FIX}")),
        }
    }
    Ok(look)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::config::error_at;
    use crate::config::tests::tree;

    /// The theme the user config `config.toml` among `files` sets.
    fn theme(files: &[(&str, &str)]) -> Result<Theme, ConfigError> {
        let root = tree(files);
        let file = root.path().join("config.toml");
        let text = fs::read_to_string(&file).unwrap();
        #[derive(Deserialize)]
        struct Config {
            theme: Spanned<Setting>,
        }
        let config: Config = toml::from_str(&text)
            .map_err(|e| error_at(&file, &text, e.span(), e.message().into()))?;
        resolve(config.theme, &file, &text, &root.path().join("themes"))
    }

    fn config(text: &str) -> Result<Theme, ConfigError> {
        theme(&[("config.toml", text)])
    }

    fn rgb(s: &str) -> Rgb {
        s.parse().unwrap()
    }

    fn colored(s: &str) -> Look {
        Look {
            color: Some(rgb(s)),
            ..Look::default()
        }
    }

    #[track_caller]
    fn assert_error(result: Result<Theme, ConfigError>, location: &str, message: &str) {
        let err = result.unwrap_err();
        assert!(err.location.ends_with(location), "{err}");
        assert!(err.message.contains(message), "{err}");
    }

    const INLINE: &str = r##"
[theme]
dark = true
added = "#00ff00"
removed = "#ff0000"

[theme.syntax]
function = "#61afef"
"function.method" = "#56b6c2 bold"

[theme.ui]
hunk-header = "#56b6c2"
"##;

    #[test]
    fn the_builtins_are_whole_themes() {
        for (name, dark) in [("default-dark", true), ("default-light", false)] {
            let t = config(&format!("theme = \"{name}\"\n")).unwrap();
            assert_eq!(t.dark, dark, "{name}");
            assert!(
                t.syntax("keyword").and_then(|l| l.color).is_some(),
                "{name}"
            );
            assert!(t.ui(Role::HunkHeader).is_some(), "{name}");
        }
    }

    #[test]
    fn a_name_is_a_table_that_sets_only_from() {
        let short = config("theme = \"default-light\"\n").unwrap();
        let table = config("[theme]\nfrom = \"default-light\"\n").unwrap();
        assert_eq!(short, table);
    }

    #[test]
    fn a_table_writes_a_theme() {
        let t = config(INLINE).unwrap();
        assert!(t.dark);
        assert_eq!((t.added, t.removed), (rgb("#00ff00"), rgb("#ff0000")));
        assert_eq!(t.syntax("function"), Some(&colored("#61afef")));
        let method = Look {
            bold: true,
            ..colored("#56b6c2")
        };
        assert_eq!(t.syntax("function.method"), Some(&method));
        assert_eq!(t.ui(Role::HunkHeader), Some(&colored("#56b6c2")));
    }

    #[test]
    fn a_capture_takes_its_longest_prefixs_look() {
        let t = config(INLINE).unwrap();
        assert_eq!(t.syntax("function.builtin"), Some(&colored("#61afef")));
        assert_eq!(
            t.syntax("function.method.call"),
            t.syntax("function.method")
        );
        assert_eq!(t.syntax("variable"), None);
        assert_eq!(t.syntax("func"), None);
    }

    #[test]
    fn only_ui_roles_the_theme_sets_have_a_look() {
        let t = config(INLINE).unwrap();
        assert_eq!(t.ui(Role::Header), None);
        assert_eq!(t.ui(Role::Added), None);
        assert_eq!(t.ui(Role::Code("function")), None);
    }

    #[test]
    fn keys_beside_from_replace_its_own() {
        let t = config(
            "[theme]\nfrom = \"default-dark\"\nadded = \"#000001\"\n[theme.syntax]\nkeyword = \"#010101\"\n[theme.ui]\nheader = \"underline\"\n",
        )
        .unwrap();
        let base = config("theme = \"default-dark\"\n").unwrap();
        assert!(t.dark);
        assert_eq!(t.added, rgb("#000001"));
        assert_eq!(t.removed, base.removed);
        assert_eq!(t.syntax("keyword"), Some(&colored("#010101")));
        assert_eq!(t.syntax("function"), base.syntax("function"));
        let underline = Look {
            underline: true,
            ..Look::default()
        };
        assert_eq!(t.ui(Role::Header), Some(&underline));
        assert_eq!(t.ui(Role::HunkHeader), base.ui(Role::HunkHeader));
    }

    #[test]
    fn a_look_is_a_colour_and_attributes() {
        let t = config(
            "[theme]\nfrom = \"default-dark\"\n[theme.syntax]\na = \"#61afef bold italic\"\nb = \"dim\"\nc = \"underline  bold\"\n",
        )
        .unwrap();
        let a = Look {
            bold: true,
            italic: true,
            ..colored("#61afef")
        };
        assert_eq!(t.syntax("a"), Some(&a));
        assert_eq!(
            t.syntax("b"),
            Some(&Look {
                dim: true,
                ..Look::default()
            })
        );
        let c = Look {
            underline: true,
            bold: true,
            ..Look::default()
        };
        assert_eq!(t.syntax("c"), Some(&c));
    }

    #[test]
    fn bad_looks_are_errors_at_their_value() {
        let looks = [
            ("\"#61afef blink\"", "`blink`"),
            ("\"\"", "empty"),
            ("\"#61afef #000000\"", "two colours"),
            ("\"#61af\"", "`#61af`"),
        ];
        for (look, message) in looks {
            let text =
                format!("[theme]\nfrom = \"default-dark\"\n[theme.syntax]\nkeyword = {look}\n");
            assert_error(config(&text), "config.toml:4:11", message);
        }
    }

    #[test]
    fn bad_colours_are_errors_at_their_value() {
        let text = "[theme]\nfrom = \"default-dark\"\nadded = \"green\"\n";
        assert_error(config(text), "config.toml:3:9", "`green`");
    }

    #[test]
    fn unknown_keys_are_errors() {
        let text = "[theme]\nfrom = \"default-dark\"\n[theme.ui]\nheadr = \"bold\"\n";
        assert_error(config(text), "config.toml:4:1", "line-number");
        let text = "[theme]\nfrom = \"default-dark\"\ncolour = 1\n";
        assert_error(config(text), "config.toml:3:1", "colour");
    }

    #[test]
    fn a_theme_needs_dark_added_and_removed() {
        for missing in ["dark", "added", "removed"] {
            let text: String = INLINE
                .lines()
                .filter(|l| !l.starts_with(&format!("{missing} =")))
                .map(|l| format!("{l}\n"))
                .collect();
            assert_error(config(&text), "config.toml:2:1", &format!("`{missing}`"));
        }
    }

    #[test]
    fn a_name_not_built_in_is_a_file_in_the_themes_directory() {
        let files = [
            ("config.toml", "theme = \"mine\"\n"),
            (
                "themes/mine.toml",
                "from = \"default-light\"\n[syntax]\nkeyword = \"#123456\"\n",
            ),
        ];
        let t = theme(&files).unwrap();
        assert!(!t.dark);
        assert_eq!(t.syntax("keyword"), Some(&colored("#123456")));
    }

    #[test]
    fn a_built_in_name_is_never_a_file() {
        let files = [
            ("config.toml", "theme = \"default-dark\"\n"),
            ("themes/default-dark.toml", "dark = false\n"),
        ];
        assert!(theme(&files).unwrap().dark);
    }

    #[test]
    fn a_path_is_relative_to_the_file_naming_it() {
        let files = [
            ("config.toml", "theme = \"mine/a.toml\"\n"),
            ("mine/a.toml", "from = \"b.toml\"\n"),
            (
                "mine/b.toml",
                "from = \"default-dark\"\nadded = \"#000001\"\n",
            ),
        ];
        assert_eq!(theme(&files).unwrap().added, rgb("#000001"));
    }

    #[test]
    fn errors_in_a_theme_file_point_into_it() {
        let files = [
            ("config.toml", "theme = \"bad\"\n"),
            (
                "themes/bad.toml",
                "from = \"default-dark\"\nadded = \"nope\"\n",
            ),
        ];
        assert_error(theme(&files), "themes/bad.toml:2:9", "`nope`");
    }

    #[test]
    fn a_missing_theme_is_an_error_naming_where_it_looked() {
        assert_error(
            config("theme = \"nope\"\n"),
            "config.toml:1:9",
            "default-dark",
        );
        let err = config("theme = \"nope\"\n").unwrap_err();
        assert!(err.message.contains("themes/nope.toml"), "{err}");
        let err = config("theme = \"gone.toml\"\n").unwrap_err();
        assert!(err.location.ends_with("config.toml:1:9"), "{err}");
    }

    #[test]
    fn a_cycle_of_froms_is_an_error() {
        let files = [
            ("config.toml", "theme = \"a\"\n"),
            ("themes/a.toml", "from = \"b\"\n"),
            ("themes/b.toml", "from = \"a\"\n"),
        ];
        assert_error(theme(&files), "themes/b.toml:1:8", "itself");
    }
}
