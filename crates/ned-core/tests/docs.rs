//! Every ```ned block in docs/ is a script that parses.

use std::fs;
use std::path::{Path, PathBuf};

use ned_core::hint::Frontend;
use ned_core::script;

fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(markdown_files(&path));
        } else if path.extension().is_some_and(|e| e == "md") {
            files.push(path);
        }
    }
    files
}

fn ned_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = text.lines();
    while lines.by_ref().any(|l| l.trim() == "```ned") {
        let block: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "```").collect();
        blocks.push(block.join("\n") + "\n");
    }
    blocks
}

#[test]
fn ned_blocks_in_the_docs_parse() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    for path in markdown_files(&docs) {
        for block in ned_blocks(&fs::read_to_string(&path).unwrap()) {
            if let Err(err) = script::parse(&block) {
                panic!(
                    "{}: {}\n{block}",
                    path.display(),
                    err.render(Frontend::Cli, Some(&block))
                );
            }
        }
    }
}

#[test]
fn the_agent_docs_have_examples() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    for path in ["agent-guide.md", "skills/ned/SKILL.md"] {
        let text = fs::read_to_string(docs.join(path)).unwrap_or_default();
        assert!(!ned_blocks(&text).is_empty(), "{path} has no ```ned blocks");
    }
}
