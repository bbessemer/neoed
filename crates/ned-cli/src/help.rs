//! `ned help [TOPIC]` (command-language spec, §1).

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Topic {
    Show,
    Outline,
    Replace,
    Insert,
    Delete,
    Sub,
    Move,
    File,
    Selectors,
    Text,
    Config,
}

/// The help text for `topic`, or the summary.
pub fn text(topic: Option<Topic>) -> &'static str {
    let _ = topic;
    ""
}
