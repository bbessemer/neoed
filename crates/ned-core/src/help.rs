//! `ned help [TOPIC]` (command-language spec, §1) and the MCP server's `help`
//! tool (§1.5), rendered in each frontend's terms (`hint::Frontend::render`).

use crate::hint::Frontend;

/// Every frontend's topics, by name, in the order they're listed.
const SHARED: &[(&str, &str)] = &[
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
];

/// The CLI's own topics, listed after the shared ones.
const CLI: &[(&str, &str)] = &[
    ("session", include_str!("../help/cli/session.txt")),
    ("repl", include_str!("../help/cli/repl.txt")),
    ("mcp", include_str!("../help/cli/mcp.txt")),
];

/// The MCP server's own topics, listed after the shared ones.
const MCP: &[(&str, &str)] = &[("session", include_str!("../help/mcp/session.txt"))];

const SUMMARY: &str = include_str!("../help/summary.txt");

impl Frontend {
    /// The topics' names, in the order they're listed.
    pub fn topics(self) -> impl Iterator<Item = &'static str> {
        self.texts().map(|(name, _)| name)
    }

    /// The summary of the command language.
    pub fn summary(self) -> String {
        self.render(SUMMARY)
    }

    /// The summary, or the text of the topic named whatever its case; or the
    /// error naming the topics.
    pub fn text(self, topic: Option<&str>) -> Result<String, String> {
        let Some(topic) = topic else {
            return Ok(self.summary());
        };
        let found = self
            .texts()
            .find(|(name, _)| name.eq_ignore_ascii_case(topic));
        found.map(|(_, text)| self.render(text)).ok_or_else(|| {
            let names: Vec<&str> = self.topics().collect();
            format!(
                "error: unknown topic `{topic}`; topics are {}",
                names.join(" ")
            )
        })
    }

    fn texts(self) -> impl Iterator<Item = (&'static str, &'static str)> {
        let own = match self {
            Frontend::Cli | Frontend::Repl => CLI,
            Frontend::Mcp => MCP,
        };
        SHARED.iter().chain(own).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hint::OPTIONS;

    const FRONTENDS: [Frontend; 3] = [Frontend::Cli, Frontend::Mcp, Frontend::Repl];

    /// The summary and every topic's text.
    fn texts(frontend: Frontend) -> Vec<(String, String)> {
        let topics = frontend
            .topics()
            .map(|topic| (topic.to_string(), frontend.text(Some(topic)).unwrap()));
        std::iter::once(("summary".to_string(), frontend.summary()))
            .chain(topics)
            .collect()
    }

    #[test]
    fn the_summary_is_the_text_without_a_topic() {
        for frontend in FRONTENDS {
            assert_eq!(frontend.text(None), Ok(frontend.summary()));
        }
    }

    #[test]
    fn topics_are_found_whatever_their_case() {
        assert_eq!(
            Frontend::Mcp.text(Some("SHOW")),
            Frontend::Mcp.text(Some("show"))
        );
    }

    #[test]
    fn the_cli_has_its_own_topics_and_the_mcp_server_doesnt() {
        let cli: Vec<_> = Frontend::Cli.topics().collect();
        let mcp: Vec<_> = Frontend::Mcp.topics().collect();
        assert_eq!(cli.len(), mcp.len() + 2);
        assert_eq!(cli[..cli.len() - 2], mcp[..]);
        assert_eq!(cli[cli.len() - 3..], ["session", "repl", "mcp"]);
        let error = Frontend::Mcp.text(Some("repl")).unwrap_err();
        assert!(
            error.starts_with("error: unknown topic `repl`; topics are show "),
            "{error}"
        );
        assert!(error.ends_with(" config session"), "{error}");
    }

    #[test]
    fn each_frontend_has_its_own_session_topic() {
        let cli = Frontend::Cli.text(Some("session")).unwrap();
        let mcp = Frontend::Mcp.text(Some("session")).unwrap();
        assert!(cli.contains("ned history"), "{cli}");
        assert!(mcp.contains("`history` lists"), "{mcp}");
    }

    #[test]
    fn options_are_named_as_each_frontend_takes_them() {
        let check = |frontend: Frontend| frontend.text(Some("check")).unwrap();
        assert!(check(Frontend::Cli).contains("--no-check skips checking; --force applies"));
        assert!(check(Frontend::Mcp).contains("`no_check` skips checking; `force` applies"));
        let file = |frontend: Frontend| frontend.text(Some("file")).unwrap();
        assert!(
            file(Frontend::Cli).contains("the FILE arguments, or every workspace file with -w.")
        );
        assert!(file(Frontend::Mcp).contains("`files`, or every workspace file with `workspace`."));
    }

    #[test]
    fn a_frontends_text_is_left_out_of_the_others() {
        let outline = |frontend: Frontend| frontend.text(Some("outline")).unwrap();
        assert!(
            outline(Frontend::Cli).contains("Files without a language are skipped (use --lang).\n")
        );
        assert!(outline(Frontend::Mcp).contains("Files without a language are skipped.\n"));
        let cli = Frontend::Cli.summary();
        assert!(cli.starts_with("ned: a line editor for coding agents. A script is one transaction: every edit\napplies, or none does.\n\n  ned [FLAGS]"), "{cli}");
        assert!(cli.contains("\nFlags: "));
        assert!(!cli.contains("dry_run"));
        let mcp = Frontend::Mcp.summary();
        assert!(mcp.contains("none does. Give it in `script`"), "{mcp}");
        assert!(mcp.contains("\nArguments: dry_run"));
        assert!(mcp.ends_with(" config session\n"), "{mcp}");
    }

    #[test]
    fn no_placeholder_is_left() {
        for frontend in FRONTENDS {
            for (topic, text) in texts(frontend) {
                assert!(
                    !["{cli:", "{mcp:", "{repl:"]
                        .iter()
                        .any(|p| text.contains(p)),
                    "{frontend:?} {topic}"
                );
                for (key, ..) in OPTIONS {
                    assert!(
                        !text.contains(&format!("{{{key}}}")),
                        "{frontend:?} {topic}: {key}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_mcp_texts_name_no_command_line_usage() {
        let cli = regex::Regex::new(r"(^|[\s(`])(-[wenqs]|--(force|no-check|no-fmt|lang|context|commit))\b|ned (help|history|undo|session|mcp|repl)\b|stdin|NED_SESSION").unwrap();
        for (topic, text) in texts(Frontend::Mcp) {
            if let Some(found) = cli.find(&text) {
                panic!("{topic} names {:?}:\n{text}", found.as_str());
            }
        }
    }

    #[test]
    fn the_repl_has_the_clis_topics_in_its_own_terms() {
        assert!(Frontend::Repl.topics().eq(Frontend::Cli.topics()));
        let summary = Frontend::Repl.summary();
        assert!(!summary.contains("ned [FLAGS]"), "{summary}");
        assert!(
            summary.contains("\nEach line at the prompt is a script"),
            "{summary}"
        );
        assert!(
            summary.contains("\nFlags are given when the REPL starts: "),
            "{summary}"
        );
        assert!(summary.ends_with(" config session repl mcp\n"), "{summary}");
        let outline = Frontend::Repl.text(Some("outline")).unwrap();
        assert!(
            outline.contains("are skipped (start the REPL with --lang).\n"),
            "{outline}"
        );
        let text = Frontend::Repl.text(Some("text")).unwrap();
        assert!(
            text.contains("stays four characters, so type the character itself"),
            "{text}"
        );
        let selectors = Frontend::Repl.text(Some("selectors")).unwrap();
        assert!(selectors.contains("(:help filters)"), "{selectors}");
        let check = Frontend::Repl.text(Some("check")).unwrap();
        assert!(
            check.contains("`ned repl --no-check` skips checking"),
            "{check}"
        );
    }
}
