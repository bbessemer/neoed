//! `ned help [TOPIC]` (command-language spec, §1).

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Topic {
    Show,
    Outline,
    Check,
    Allow,

    Replace,
    Insert,
    Delete,
    Sub,
    Move,
    File,
    Create,
    Selectors,
    Text,
    Config,
}

/// The help text for `topic`, or the summary.
pub fn text(topic: Option<Topic>) -> &'static str {
    match topic {
        None => include_str!("../help/summary.txt"),
        Some(Topic::Show) => include_str!("../help/show.txt"),
        Some(Topic::Outline) => include_str!("../help/outline.txt"),
        Some(Topic::Check) => include_str!("../help/check.txt"),
        Some(Topic::Allow) => include_str!("../help/allow.txt"),

        Some(Topic::Replace) => include_str!("../help/replace.txt"),
        Some(Topic::Insert) => include_str!("../help/insert.txt"),
        Some(Topic::Delete) => include_str!("../help/delete.txt"),
        Some(Topic::Sub) => include_str!("../help/sub.txt"),
        Some(Topic::Move) => include_str!("../help/move.txt"),
        Some(Topic::File) => include_str!("../help/file.txt"),
        Some(Topic::Create) => include_str!("../help/create.txt"),
        Some(Topic::Selectors) => include_str!("../help/selectors.txt"),
        Some(Topic::Text) => include_str!("../help/text.txt"),
        Some(Topic::Config) => include_str!("../help/config.txt"),
    }
}
