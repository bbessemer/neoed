//! Tests of `ned help` (command-language spec, §1).

use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use ned_core::script;

/// Budgets that keep help cheap to read into an agent's context.
const SUMMARY_BYTES: usize = 3200;
const TOPIC_BYTES: usize = 1600;

fn ned(args: &[&str]) -> Output {
    cargo_bin_cmd!("ned")
        .args(args)
        .write_stdin("")
        .output()
        .unwrap()
}

fn stdout(args: &[&str]) -> String {
    let output = ned(args);
    assert!(output.status.success(), "ned {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The topics, as `ned help` lists them when given a bad one.
fn topics() -> Vec<String> {
    let stderr = String::from_utf8(ned(&["help", "nope"]).stderr).unwrap();
    let list = stderr
        .split("[possible values: ")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap_or_else(|| panic!("no topic list in {stderr:?}"));
    list.split(", ").map(String::from).collect()
}

/// The script in each `Examples:` block: the indented lines after it.
fn examples(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = text.lines();
    while lines.by_ref().any(|l| l == "Examples:") {
        let block: Vec<&str> = lines
            .clone()
            .take_while(|l| l.starts_with("    ") || l.is_empty())
            .map(|l| l.strip_prefix("    ").unwrap_or(l))
            .collect();
        blocks.push(block.join("\n") + "\n");
    }
    blocks
}

#[test]
fn unknown_topics_exit_2_and_list_the_topics() {
    let output = ned(&["help", "nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(topics().contains(&"move".to_string()));
}

#[test]
fn the_summary_fits_its_budget_and_names_every_topic() {
    let summary = stdout(&["help"]);
    assert!(summary.len() <= SUMMARY_BYTES, "{} bytes", summary.len());
    for topic in topics() {
        assert!(summary.contains(&topic), "summary doesn't name {topic}");
    }
}

#[test]
fn every_topic_fits_its_budget_and_starts_with_its_name() {
    for topic in topics() {
        let text = stdout(&["help", &topic]);
        assert!(text.len() <= TOPIC_BYTES, "{topic}: {} bytes", text.len());
        assert!(text.starts_with(&topic), "{topic}: {text:?}");
        if let Some(usage) = ned_core::script::parser::usage(&topic) {
            assert_eq!(text.lines().next(), Some(usage), "{topic}");
        }
    }
}

#[test]
fn every_command_has_a_topic() {
    let stderr = String::from_utf8(ned(&["-e", "frobnicate"]).stderr).unwrap();
    let commands = stderr
        .split("commands are ")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .unwrap_or_else(|| panic!("no command list in {stderr:?}"));
    let topics = topics();
    for command in commands.split_whitespace() {
        assert!(
            topics.iter().any(|t| t == command),
            "no topic for {command}"
        );
    }
}

#[test]
fn every_verb_topic_has_examples_and_they_parse() {
    let texts = std::iter::once(("summary".to_string(), stdout(&["help"]))).chain(
        topics().into_iter().map(|t| {
            let text = stdout(&["help", &t]);
            (t, text)
        }),
    );
    for (topic, text) in texts {
        let blocks = examples(&text);
        let is_verb = !["selectors", "text", "config", "session"].contains(&topic.as_str());
        assert!(!is_verb || !blocks.is_empty(), "{topic} has no examples");
        for block in blocks {
            if let Err(err) = script::parse(&block) {
                panic!("{topic}: {}\n{block}", err.render(&block));
            }
        }
    }
}

#[test]
fn a_file_named_help_is_written_with_a_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("help"), "x\n").unwrap();
    let output = cargo_bin_cmd!("ned")
        .current_dir(dir.path())
        .args(["./help", "-e", "show"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "./help:1\n1:x\n");
}
