//! Colour for output read on a terminal (command-language spec, §6.6).

use std::borrow::Cow;
use std::ffi::OsStr;
use std::str::FromStr;

use crate::highlight::prefixes;

/// When to colour a stream: the `--color` words.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum When {
    #[default]
    Auto,
    Always,
    Never,
}

impl When {
    /// The style for a stream that is a terminal or not, given the value of
    /// `NO_COLOR`.
    pub fn style(self, terminal: bool, no_color: Option<&OsStr>) -> Style {
        match self {
            When::Always => Style::Color,
            When::Never => Style::Plain,
            When::Auto if terminal && no_color.is_none_or(OsStr::is_empty) => Style::Color,
            When::Auto => Style::Plain,
        }
    }
}

impl FromStr for When {
    type Err = String;

    fn from_str(s: &str) -> Result<When, String> {
        match s {
            "auto" => Ok(When::Auto),
            "always" => Ok(When::Always),
            "never" => Ok(When::Never),
            _ => Err("expected auto, always or never".into()),
        }
    }
}

/// How output is painted: plain text, or with ANSI colours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Style {
    #[default]
    Plain,
    Color,
}

/// What a piece of output is, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A `show` region's header, an edit's summary line, a `fmt` header.
    Header,
    LineNumber,
    /// An outline entry's kind, the `fn` of `fn:parse`.
    Kind,
    /// Text of no importance: an outline entry's lines.
    Dim,
    HunkHeader,
    Added,
    Removed,
    Error,
    Warning,
    Info,
    Note,
    /// Highlighted code, by its highlight capture name.
    Code(&'static str),
}

/// What a highlight capture marks, which decides its colour with no theme. Captures
/// without one, such as `@variable` and `@punctuation`, stay plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    /// Constants, numbers, escapes and attributes.
    Constant,
    /// A JSX tag.
    Tag,
    /// A Markdown heading.
    Heading,
    /// A Markdown link destination or reference.
    Link,
}

/// The capture names, or dotted prefixes of them, that have a group.
const GROUPS: &[(&str, Group)] = &[
    ("keyword", Group::Keyword),
    ("string", Group::String),
    ("character", Group::String),
    ("text.literal", Group::String),
    ("comment", Group::Comment),
    ("function", Group::Function),
    ("constructor", Group::Type),
    ("type", Group::Type),
    ("constant", Group::Constant),
    ("number", Group::Constant),
    ("boolean", Group::Constant),
    ("escape", Group::Constant),
    ("string.escape", Group::Constant),
    ("attribute", Group::Constant),
    ("tag", Group::Tag),
    ("text.title", Group::Heading),
    ("text.uri", Group::Link),
    ("text.reference", Group::Link),
];

/// The group of the capture `name`, by its longest dotted prefix that has
/// one: `function.method` is a `Function`.
pub(crate) fn group(name: &str) -> Option<Group> {
    prefixes(name).find_map(|p| {
        GROUPS
            .iter()
            .find(|(g, _)| *g == p)
            .map(|(_, group)| *group)
    })
}

impl Style {
    /// Whether this style colours code highlighted as `capture`.
    pub fn colours(self, capture: &str) -> bool {
        self == Style::Color && group(capture).is_some()
    }

    /// `text` painted as `role`; unchanged when plain.
    pub fn paint(self, role: Role, text: &str) -> Cow<'_, str> {
        let code = match role {
            Role::Header => "1",
            Role::LineNumber | Role::Dim => "2",
            Role::Kind | Role::HunkHeader => "36",
            Role::Added => "32",
            Role::Removed => "31",
            Role::Error => "1;31",
            Role::Warning => "1;33",
            Role::Info => "1;34",
            Role::Note => "1;36",
            Role::Code(capture) => match group(capture) {
                Some(Group::Keyword) => "35",
                Some(Group::String) => "32",
                Some(Group::Comment) => "2",
                Some(Group::Function | Group::Tag) => "34",
                Some(Group::Type) => "33",
                Some(Group::Constant) => "36",
                Some(Group::Heading) => "1",
                Some(Group::Link) => "4",
                None => return Cow::Borrowed(text),
            },
        };
        match self {
            Style::Color if !text.is_empty() => Cow::Owned(format!("\x1b[{code}m{text}\x1b[0m")),
            _ => Cow::Borrowed(text),
        }
    }

    /// A message for stderr, with the `error:` or `note:` that starts it
    /// painted.
    pub fn message(self, text: &str) -> Cow<'_, str> {
        if self == Style::Plain {
            return Cow::Borrowed(text);
        }
        for (prefix, role) in [("error:", Role::Error), ("note:", Role::Note)] {
            if let Some(rest) = text.strip_prefix(prefix) {
                return Cow::Owned(format!("{}{rest}", self.paint(role, prefix)));
            }
        }
        Cow::Borrowed(text)
    }
}

/// `text` with its escapes visible, as `\e[...m`, for tests.
#[cfg(test)]
pub(crate) fn shown(text: &str) -> String {
    text.replace('\x1b', r"\e")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_parses_the_color_words() {
        assert_eq!("auto".parse(), Ok(When::Auto));
        assert_eq!("always".parse(), Ok(When::Always));
        assert_eq!("never".parse(), Ok(When::Never));
        let err = "sometimes".parse::<When>().unwrap_err();
        assert!(err.contains("auto, always or never"), "{err}");
    }

    #[test]
    fn auto_colours_a_terminal_unless_no_color_is_set() {
        let set = OsStr::new("1");
        assert_eq!(When::Auto.style(true, None), Style::Color);
        assert_eq!(When::Auto.style(false, None), Style::Plain);
        assert_eq!(When::Auto.style(true, Some(set)), Style::Plain);
        assert_eq!(When::Auto.style(true, Some(OsStr::new(""))), Style::Color);
    }

    #[test]
    fn always_and_never_ignore_the_terminal_and_no_color() {
        let set = Some(OsStr::new("1"));
        for terminal in [true, false] {
            for no_color in [None, set] {
                assert_eq!(When::Always.style(terminal, no_color), Style::Color);
                assert_eq!(When::Never.style(terminal, no_color), Style::Plain);
            }
        }
    }

    #[test]
    fn plain_paints_nothing() {
        assert!(matches!(
            Style::Plain.paint(Role::Removed, "-x"),
            Cow::Borrowed("-x")
        ));
        let message = "error: bad\nnote: also";
        assert!(matches!(Style::Plain.message(message), Cow::Borrowed(m) if m == message));
    }

    #[test]
    fn color_wraps_text_in_its_roles_codes() {
        let painted = |role| shown(&Style::Color.paint(role, "x"));
        assert_eq!(painted(Role::Header), r"\e[1mx\e[0m");
        assert_eq!(painted(Role::LineNumber), r"\e[2mx\e[0m");
        assert_eq!(painted(Role::Dim), r"\e[2mx\e[0m");
        assert_eq!(painted(Role::Kind), r"\e[36mx\e[0m");
        assert_eq!(painted(Role::HunkHeader), r"\e[36mx\e[0m");
        assert_eq!(painted(Role::Added), r"\e[32mx\e[0m");
        assert_eq!(painted(Role::Removed), r"\e[31mx\e[0m");
        assert_eq!(painted(Role::Error), r"\e[1;31mx\e[0m");
        assert_eq!(painted(Role::Warning), r"\e[1;33mx\e[0m");
        assert_eq!(painted(Role::Info), r"\e[1;34mx\e[0m");
        assert_eq!(painted(Role::Note), r"\e[1;36mx\e[0m");
    }

    #[test]
    fn captures_group_by_their_longest_dotted_prefix() {
        let cases = [
            ("keyword", Some(Group::Keyword)),
            ("function.method", Some(Group::Function)),
            ("function.macro", Some(Group::Function)),
            ("string.special", Some(Group::String)),
            ("string.escape", Some(Group::Constant)),
            ("escape", Some(Group::Constant)),
            ("comment.documentation", Some(Group::Comment)),
            ("constant.builtin", Some(Group::Constant)),
            ("number", Some(Group::Constant)),
            ("attribute", Some(Group::Constant)),
            ("constructor", Some(Group::Type)),
            ("type.builtin", Some(Group::Type)),
            ("variable.builtin", None),
            ("tag", Some(Group::Tag)),
            ("text.title", Some(Group::Heading)),
            ("text.uri", Some(Group::Link)),
            ("text.reference", Some(Group::Link)),
            ("text.literal", Some(Group::String)),
            ("variable", None),
            ("variable.parameter", None),
            ("property", None),
            ("punctuation.bracket", None),
            ("operator", None),
            ("none", None),
            ("keywords", None),
        ];
        for (name, expected) in cases {
            assert_eq!(group(name), expected, "{name}");
        }
    }

    #[test]
    fn code_takes_its_captures_groups_colour() {
        let painted = |capture| shown(&Style::Color.paint(Role::Code(capture), "x"));
        assert_eq!(painted("keyword"), r"\e[35mx\e[0m");
        assert_eq!(painted("string"), r"\e[32mx\e[0m");
        assert_eq!(painted("comment"), r"\e[2mx\e[0m");
        assert_eq!(painted("function.method"), r"\e[34mx\e[0m");
        assert_eq!(painted("type"), r"\e[33mx\e[0m");
        assert_eq!(painted("constant"), r"\e[36mx\e[0m");
        assert_eq!(painted("tag"), r"\e[34mx\e[0m");
        assert_eq!(painted("text.title"), r"\e[1mx\e[0m");
        assert_eq!(painted("text.uri"), r"\e[4mx\e[0m");
        assert_eq!(painted("variable"), "x");
    }

    #[test]
    fn only_colour_colours_captures_with_a_group() {
        assert!(Style::Color.colours("function.method"));
        assert!(!Style::Color.colours("variable"));
        assert!(!Style::Plain.colours("function"));
    }

    #[test]
    fn color_leaves_empty_text_alone() {
        assert!(matches!(
            Style::Color.paint(Role::Added, ""),
            Cow::Borrowed("")
        ));
    }

    #[test]
    fn messages_paint_only_their_leading_prefix() {
        let message = |text| shown(&Style::Color.message(text));
        assert_eq!(
            message("error: a.rs:3: note: x"),
            r"\e[1;31merror:\e[0m a.rs:3: note: x"
        );
        assert_eq!(
            message("note: read .toml files as text\nerror: later"),
            "\\e[1;36mnote:\\e[0m read .toml files as text\nerror: later"
        );
        assert_eq!(message("warning: x"), "warning: x");
        assert_eq!(message("  error: indented"), "  error: indented");
    }
}
