//! `ned help [TOPIC]` (command-language spec, §1).

/// Each topic's name and text, in the order they're listed.
pub const TOPICS: &[(&str, &str)] = &[
    ("show", include_str!("../help/show.txt")),
    ("outline", include_str!("../help/outline.txt")),
    ("check", include_str!("../help/check.txt")),
    ("allow", include_str!("../help/allow.txt")),
    ("replace", include_str!("../help/replace.txt")),
    ("insert", include_str!("../help/insert.txt")),
    ("delete", include_str!("../help/delete.txt")),
    ("sub", include_str!("../help/sub.txt")),
    ("move", include_str!("../help/move.txt")),
    ("rename", include_str!("../help/rename.txt")),
    ("resolve", include_str!("../help/resolve.txt")),
    ("file", include_str!("../help/file.txt")),
    ("create", include_str!("../help/create.txt")),
    ("selectors", include_str!("../help/selectors.txt")),
    ("filters", include_str!("../help/filters.txt")),
    ("patterns", include_str!("../help/patterns.txt")),
    ("conflicts", include_str!("../help/conflicts.txt")),
    ("text", include_str!("../help/text.txt")),
    ("config", include_str!("../help/config.txt")),
    ("session", include_str!("../help/session.txt")),
    ("repl", include_str!("../help/repl.txt")),
    ("mcp", include_str!("../help/mcp.txt")),
];

/// The summary of the command language.
pub const SUMMARY: &str = include_str!("../help/summary.txt");

/// The text for `ned help [TOPIC]`, the topic named whatever its case, or
/// the error naming the topics.
pub fn text(name: Option<&str>) -> Result<&'static str, String> {
    let Some(name) = name else {
        return Ok(SUMMARY);
    };
    let found = TOPICS
        .iter()
        .find(|(topic, _)| topic.eq_ignore_ascii_case(name));
    found.map(|(_, text)| *text).ok_or_else(|| {
        let names: Vec<&str> = TOPICS.iter().map(|(name, _)| *name).collect();
        format!(
            "error: unknown topic `{name}`; topics are {}",
            names.join(" ")
        )
    })
}
