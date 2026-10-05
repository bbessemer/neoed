//! Colour for output read on a terminal (command-language spec, §6.6).

use std::borrow::Cow;
use std::ffi::OsStr;
use std::str::FromStr;

use crate::color::{Rgb, nearest_256, tint};
use crate::highlight::prefixes;
use crate::theme::{Look, Theme};

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

/// How output is painted: plain text, with the terminal's 16 colours, or with
/// a theme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Style {
    #[default]
    Plain,
    Color,
    Theme(&'static Theme, Depth),
}

/// How many colours a terminal shows, of those more than 16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Truecolor,
    Xterm256,
}

impl Depth {
    /// The depth a terminal reports in `COLORTERM` and `TERM`, if more than
    /// 16 colours.
    pub fn detect(colorterm: Option<&OsStr>, term: Option<&OsStr>) -> Option<Depth> {
        let is = |var: Option<&OsStr>, test: fn(&str) -> bool| {
            var.and_then(OsStr::to_str).is_some_and(test)
        };
        if is(colorterm, |c| c == "truecolor" || c == "24bit") {
            Some(Depth::Truecolor)
        } else if is(term, |t| t.contains("256color")) {
            Some(Depth::Xterm256)
        } else {
            None
        }
    }
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
    /// This style with `theme`, if it colours and the terminal shows `depth`.
    pub fn themed(self, theme: Option<&'static Theme>, depth: Option<Depth>) -> Style {
        match (self, theme, depth) {
            (Style::Color, Some(theme), Some(depth)) => Style::Theme(theme, depth),
            _ => self,
        }
    }

    /// Whether this style colours code highlighted as `capture`.
    pub fn colours(self, capture: &str) -> bool {
        match self {
            Style::Plain => false,
            Style::Color => group(capture).is_some(),
            Style::Theme(theme, _) => theme.syntax(capture).is_some(),
        }
    }

    /// `text` painted as `role`; unchanged when plain.
    pub fn paint(self, role: Role, text: &str) -> Cow<'_, str> {
        let code = match self {
            Style::Plain => return Cow::Borrowed(text),
            Style::Color => ansi(role).map(Cow::Borrowed),
            Style::Theme(theme, depth) => {
                let line = |color| {
                    Some(Look {
                        color: Some(color),
                        ..Look::default()
                    })
                };
                let look = match role {
                    Role::Code(capture) => theme.syntax(capture).copied(),
                    Role::Added => line(theme.added),
                    Role::Removed => line(theme.removed),
                    _ => theme.ui(role).copied(),
                };
                match look {
                    Some(look) => Some(Cow::Owned(sgr(look, depth))),
                    None => ansi(role)
                        .filter(|_| !matches!(role, Role::Code(_)))
                        .map(Cow::Borrowed),
                }
            }
        };
        wrap(code.as_deref().unwrap_or(""), text)
    }

    /// `text`, highlighted as `capture`, painted on a line painted as `line`: a
    /// theme tints its colour by an added or removed line's.
    pub fn code<'a>(
        self,
        capture: &'static str,
        line: Option<Role>,
        text: &'a str,
    ) -> Cow<'a, str> {
        let Style::Theme(theme, depth) = self else {
            return self.paint(Role::Code(capture), text);
        };
        let by = match line {
            Some(Role::Added) => theme.added,
            Some(Role::Removed) => theme.removed,
            _ => return self.paint(Role::Code(capture), text),
        };
        match theme.syntax(capture) {
            None => self.paint(line.expect("a changed line"), text),
            Some(look) => {
                let color = look.color.map_or(by, |c| tint(c, by, theme.dark));
                wrap(
                    &sgr(
                        Look {
                            color: Some(color),
                            ..*look
                        },
                        depth,
                    ),
                    text,
                )
            }
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

/// The 16-colour SGR code for `role`, if it has one.
fn ansi(role: Role) -> Option<&'static str> {
    Some(match role {
        Role::Header => "1",
        Role::LineNumber | Role::Dim => "2",
        Role::Kind | Role::HunkHeader => "36",
        Role::Added => "32",
        Role::Removed => "31",
        Role::Error => "1;31",
        Role::Warning => "1;33",
        Role::Info => "1;34",
        Role::Note => "1;36",
        Role::Code(capture) => match group(capture)? {
            Group::Keyword => "35",
            Group::String => "32",
            Group::Comment => "2",
            Group::Function | Group::Tag => "34",
            Group::Type => "33",
            Group::Constant => "36",
            Group::Heading => "1",
            Group::Link => "4",
        },
    })
}

/// The SGR code for `look` on a terminal showing `depth`.
fn sgr(look: Look, depth: Depth) -> String {
    let attributes = [
        (look.bold, "1"),
        (look.dim, "2"),
        (look.italic, "3"),
        (look.underline, "4"),
    ];
    let mut codes: Vec<String> = attributes
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, code)| code.to_string())
        .collect();
    if let Some(Rgb { r, g, b }) = look.color {
        codes.push(match depth {
            Depth::Truecolor => format!("38;2;{r};{g};{b}"),
            Depth::Xterm256 => format!("38;5;{}", nearest_256(Rgb { r, g, b })),
        });
    }
    codes.join(";")
}

/// `text` in SGR `code`, if both have something in them.
fn wrap<'a>(code: &str, text: &'a str) -> Cow<'a, str> {
    match code.is_empty() || text.is_empty() {
        true => Cow::Borrowed(text),
        false => Cow::Owned(format!("\x1b[{code}m{text}\x1b[0m")),
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

    const THEME: &str = r##"
[theme]
dark = true
added = "#00ff00"
removed = "#ff0000"
[theme.syntax]
keyword = "#c678dd bold"
function = "#61afef"
comment = "italic"
[theme.ui]
hunk-header = "#56b6c2"
line-number = "dim"
"##;

    fn themed(depth: Depth) -> Style {
        Style::Theme(crate::theme::tests::leaked(THEME), depth)
    }

    #[test]
    fn depth_comes_from_colorterm_then_term() {
        let os = |s: &'static str| Some(OsStr::new(s));
        assert_eq!(Depth::detect(os("truecolor"), None), Some(Depth::Truecolor));
        assert_eq!(
            Depth::detect(os("24bit"), os("xterm")),
            Some(Depth::Truecolor)
        );
        assert_eq!(
            Depth::detect(os("truecolor"), os("xterm-256color")),
            Some(Depth::Truecolor)
        );
        assert_eq!(
            Depth::detect(None, os("xterm-256color")),
            Some(Depth::Xterm256)
        );
        assert_eq!(
            Depth::detect(os("yes"), os("screen-256color")),
            Some(Depth::Xterm256)
        );
        assert_eq!(Depth::detect(os("yes"), os("xterm")), None);
        assert_eq!(Depth::detect(None, None), None);
    }

    #[test]
    fn only_colour_on_a_deep_terminal_takes_a_theme() {
        let theme = crate::theme::tests::leaked(THEME);
        let deep = Some(Depth::Truecolor);
        assert_eq!(
            Style::Color.themed(Some(theme), deep),
            Style::Theme(theme, Depth::Truecolor)
        );
        assert_eq!(Style::Color.themed(None, deep), Style::Color);
        assert_eq!(Style::Color.themed(Some(theme), None), Style::Color);
        assert_eq!(Style::Plain.themed(Some(theme), deep), Style::Plain);
    }

    #[test]
    fn a_theme_paints_captures_in_truecolor() {
        let painted = |capture| shown(&themed(Depth::Truecolor).paint(Role::Code(capture), "x"));
        assert_eq!(painted("keyword"), r"\e[1;38;2;198;120;221mx\e[0m");
        assert_eq!(painted("function.method"), r"\e[38;2;97;175;239mx\e[0m");
        assert_eq!(painted("comment"), r"\e[3mx\e[0m");
        assert_eq!(painted("string"), "x");
    }

    #[test]
    fn a_theme_on_a_256_colour_terminal_takes_the_nearest() {
        let painted = shown(&themed(Depth::Xterm256).paint(Role::Code("function"), "x"));
        let nearest = crate::color::nearest_256("#61afef".parse().unwrap());
        assert_eq!(painted, format!(r"\e[38;5;{nearest}mx\e[0m"));
    }

    #[test]
    fn a_theme_paints_ui_roles_or_leaves_them_as_without_one() {
        let painted = |role| shown(&themed(Depth::Truecolor).paint(role, "x"));
        assert_eq!(painted(Role::HunkHeader), r"\e[38;2;86;182;194mx\e[0m");
        assert_eq!(painted(Role::LineNumber), r"\e[2mx\e[0m");
        assert_eq!(painted(Role::Header), r"\e[1mx\e[0m");
        assert_eq!(painted(Role::Error), r"\e[1;31mx\e[0m");
        assert_eq!(painted(Role::Added), r"\e[38;2;0;255;0mx\e[0m");
        assert_eq!(painted(Role::Removed), r"\e[38;2;255;0;0mx\e[0m");
        assert_eq!(shown(&themed(Depth::Truecolor).paint(Role::Added, "")), "");
    }

    #[test]
    fn a_theme_tints_code_on_changed_lines() {
        let style = themed(Depth::Truecolor);
        let code = |capture, line| shown(&style.code(capture, line, "x"));
        assert_eq!(
            code("function", None),
            shown(&style.paint(Role::Code("function"), "x"))
        );
        let blue: crate::color::Rgb = "#61afef".parse().unwrap();
        let crate::color::Rgb { r, g, b } =
            crate::color::tint(blue, "#00ff00".parse().unwrap(), true);
        assert_eq!(
            code("function", Some(Role::Added)),
            format!(r"\e[38;2;{r};{g};{b}mx\e[0m")
        );
        let purple = "#c678dd".parse().unwrap();
        let crate::color::Rgb { r, g, b } =
            crate::color::tint(purple, "#ff0000".parse().unwrap(), true);
        assert_eq!(
            code("keyword", Some(Role::Removed)),
            format!(r"\e[1;38;2;{r};{g};{b}mx\e[0m")
        );
        assert_eq!(
            code("comment", Some(Role::Added)),
            r"\e[3;38;2;0;255;0mx\e[0m"
        );
        assert_eq!(
            code("string", Some(Role::Added)),
            shown(&style.paint(Role::Added, "x"))
        );
    }

    #[test]
    fn without_a_theme_code_on_changed_lines_keeps_its_colour() {
        let code = shown(&Style::Color.code("keyword", Some(Role::Added), "x"));
        assert_eq!(code, r"\e[35mx\e[0m");
        assert_eq!(Style::Plain.code("keyword", Some(Role::Added), "x"), "x");
    }

    #[test]
    fn a_theme_colours_the_captures_it_sets() {
        let style = themed(Depth::Truecolor);
        assert!(style.colours("function.method"));
        assert!(style.colours("comment"));
        assert!(!style.colours("string"));
    }

    #[test]
    fn a_theme_paints_messages_with_its_looks() {
        let message = shown(&themed(Depth::Truecolor).message("error: bad"));
        assert_eq!(message, r"\e[1;31merror:\e[0m bad");
    }
}
