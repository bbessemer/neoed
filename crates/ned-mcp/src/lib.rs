//! The MCP server behind `ned mcp` (command-language spec §1.5): JSON-RPC 2.0
//! on stdio, one message per line.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use ned_core::help;
use ned_core::invoke::{self, Failure, Invocation, Output};
use ned_core::lang::Language;
use ned_core::lsp::Lsp;
use ned_core::script::{
    self,
    ast::{Command, CommandKind},
};
use ned_core::session::{self, Session};
use ned_core::style::Style;
use ned_core::workspace;
use serde_json::{Value, json};

/// The protocol versions the server speaks, latest first.
pub const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// A server for one workspace, recording into one session.
pub struct Server<C> {
    pub cwd: PathBuf,
    pub root: PathBuf,
    pub session: Session,
    /// Whether the session was named (`-s`, `NED_SESSION`), so it keeps its
    /// name in a new workspace.
    pub named: bool,
    /// `--lang`, for every call.
    pub lang: Option<Option<Language>>,
    /// `--context`, for every call.
    pub context: usize,
    /// The version `ned -V` prints.
    pub version: String,
    /// The language servers for a workspace's root.
    pub connect: C,
}

/// Session `name`, or the first `mcp-N` the workspace at `root` has no log
/// for, which it creates.
pub fn open_session(name: Option<&str>, root: &Path) -> Result<Session, Failure> {
    match name {
        Some(name) => invoke::open(name, root),
        None => session::state_dir()
            .and_then(|state| session::next_free(&state, root, "mcp"))
            .map_err(invoke::failure),
    }
}

impl<L: Lsp, C: FnMut(PathBuf) -> L> Server<C> {
    /// Answers each message of `input` that needs it on `output`, until
    /// `input` ends.
    pub fn serve(&mut self, mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
        let mut line = Vec::new();
        while input.read_until(b'\n', &mut line)? > 0 {
            if !line.trim_ascii().is_empty()
                && let Some(response) = self.answer(&line)
            {
                writeln!(output, "{response}")?;
                output.flush()?;
            }
            line.clear();
        }
        Ok(())
    }

    /// The response to the message `line`; `None` for a notification.
    fn answer(&mut self, line: &[u8]) -> Option<Value> {
        let message: Value = match serde_json::from_slice(line) {
            Ok(message) => message,
            Err(err) => {
                return Some(error(
                    Value::Null,
                    PARSE_ERROR,
                    &format!("not JSON: {err}; send one JSON-RPC message per line"),
                ));
            }
        };
        let Value::Object(message) = message else {
            let why = "a message is one JSON object; batches aren't supported";
            return Some(error(Value::Null, INVALID_REQUEST, why));
        };
        // A response from the client answers nothing the server asked, so it needs no
        // answer either.
        if !message.contains_key("method")
            && (message.contains_key("result") || message.contains_key("error"))
        {
            return None;
        }
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            let id = id.unwrap_or(Value::Null);
            return Some(error(
                id,
                INVALID_REQUEST,
                "a request needs a method; add one, such as `\"method\": \"tools/call\"`",
            ));
        };
        let id = id?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call(&params),
            _ => Err((
                METHOD_NOT_FOUND,
                format!(
                    "unknown method `{method}`; methods are initialize ping tools/list tools/call"
                ),
            )),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error(id, code, &message),
        })
    }

    /// The answer to `initialize`: the protocol version the client asked for,
    /// if the server speaks it, else the latest.
    fn initialize(&self, params: &Value) -> Value {
        let asked = params["protocolVersion"].as_str();
        let version = VERSIONS.iter().find(|version| Some(**version) == asked);
        let name = self.session.name();
        let instructions = format!(
            "ned edits files with short scripts; the `ned` tool's description is its language. \
         Every call is recorded in session {name} of the workspace {}: a `ned` script of `!!` \
         repeats the last one, `undo` reverts the last edit and `history` lists the calls.",
            self.root.display()
        );
        json!({
            "protocolVersion": version.unwrap_or(&VERSIONS[0]),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "ned", "version": self.version },
            "instructions": instructions,
        })
    }

    /// The result of `tools/call`: the tool's output, or a protocol error for
    /// an unknown tool or bad arguments.
    fn call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let Some(name) = params["name"].as_str() else {
            return Err((
                INVALID_PARAMS,
                "tools/call needs the tool's `name`; add one, such as `\"name\": \"ned\"`".into(),
            ));
        };
        let args = Arguments(&params["arguments"]);
        let mut transcript = Transcript::default();
        let exit = match name {
            "ned" => {
                let script = args.required("script")?;
                self.run(script, &args, &mut transcript)?
            }
            "outline" => self.run("outline".into(), &args, &mut transcript)?,
            "show" => {
                let script = format!("show {}", args.required("selector")?);
                if shows_more_than_a_selector(&script) {
                    let error = "error: `selector` holds more than a selector; give a script of several commands to the `ned` tool";
                    transcript.message(error);
                    2
                } else {
                    self.run(script, &args, &mut transcript)?
                }
            }
            "history" => self.history(args.flag("all")?, &mut transcript),
            "undo" => self.undo(args.flag("force")?, &mut transcript),
            "cd" => {
                let dir = args.required("dir")?;
                self.cd(&dir, &mut transcript)
            }
            "help" => match help::text(args.string("topic")?.as_deref()) {
                Ok(text) => {
                    transcript.out(text);
                    0
                }
                Err(error) => {
                    transcript.message(&error);
                    2
                }
            },
            _ => {
                let tools = tools();
                let names: Vec<&str> = tools
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|tool| tool["name"].as_str())
                    .collect();
                let error = format!("unknown tool `{name}`; tools are {}", names.join(" "));
                return Err((INVALID_PARAMS, error));
            }
        };
        Ok(transcript.result(exit))
    }

    /// Runs `script` as `ned` would with the call's file set and flags,
    /// returning the exit code.
    fn run(
        &mut self,
        script: String,
        args: &Arguments,
        out: &mut Transcript,
    ) -> Result<u8, (i64, String)> {
        let invocation = Invocation {
            cwd: self.cwd.clone(),
            files: args.strings("files")?,
            workspace: args.flag("workspace")?,
            root: self.root.clone(),
            dry_run: args.flag("dry_run")?,
            quiet: args.flag("quiet")?,
            force: args.flag("force")?,
            no_fmt: args.flag("no_fmt")?,
            no_check: args.flag("no_check")?,
            lang: self.lang,
            context: self.context,
            commit: args.string("commit")?,
            style: Style::Plain,
            comment: args
                .string("comment")?
                .filter(|comment| !comment.is_empty()),
        };
        let usage = if !invocation.files.is_empty() && invocation.workspace {
            Some("error: give `files` or `workspace`, not both")
        } else if invocation.commit.is_some() && invocation.dry_run {
            Some("error: `commit` writes the edits, so it can't go with `dry_run`; drop one")
        } else if invocation.commit.as_deref() == Some("") {
            Some("error: `commit` needs a message; give one, or drop `commit`")
        } else {
            None
        };
        if let Some(error) = usage {
            out.message(error);
            return Ok(2);
        }
        let session = Some(&self.session);
        Ok(invoke::invoke(
            invocation,
            script,
            session,
            &mut self.connect,
            out,
        ))
    }

    /// `ned history [--all]` of the session, returning the exit code.
    fn history(&self, all: bool, out: &mut Transcript) -> u8 {
        match invoke::history(&self.session, all, out) {
            Ok(()) => 0,
            Err(failure) => invoke::fail(failure, out),
        }
    }

    /// `ned undo [--force]` in the session, returning the exit code.
    fn undo(&self, force: bool, out: &mut Transcript) -> u8 {
        // Canonical, as the paths an undo reverts are, to show them relative to it.
        let cwd = self.cwd.canonicalize().unwrap_or(self.cwd.clone());
        match invoke::undo(&self.session, cwd, force, Style::Plain, out) {
            Ok(()) => 0,
            Err(failure) => invoke::fail(failure, out),
        }
    }

    /// Moves the server to `dir`, returning the exit code.
    fn cd(&mut self, dir: &str, out: &mut Transcript) -> u8 {
        let dir = self.cwd.join(dir);
        let cwd = match dir.canonicalize() {
            Ok(cwd) if cwd.is_dir() => cwd,
            Ok(file) => {
                let parent = file.parent().unwrap_or(&file);
                let error = format!(
                    "error: {} isn't a directory; give its directory, {}",
                    dir.display(),
                    parent.display()
                );
                return invoke::fail((error, 3), out);
            }
            Err(err) => {
                return invoke::fail(
                    (
                        format!(
                            "error: cannot read {}: {err}; give a directory relative to {}",
                            dir.display(),
                            self.cwd.display()
                        ),
                        3,
                    ),
                    out,
                );
            }
        };
        let root = workspace::root(&cwd).unwrap_or(cwd.clone());
        let session = if self.root.canonicalize().is_ok_and(|old| old == root) {
            None
        } else {
            let name = self.named.then(|| self.session.name().to_string());
            match open_session(name.as_deref(), &root) {
                Ok(session) => Some(session),
                Err(failure) => return invoke::fail(failure, out),
            }
        };
        // Scripts read their files, and git finds its repository, from the
        // process's working directory.
        if let Err(err) = std::env::set_current_dir(&cwd) {
            return invoke::fail(
                (
                    format!(
                        "error: cannot enter {}: {err}; give a directory you may enter",
                        cwd.display()
                    ),
                    3,
                ),
                out,
            );
        }
        if let Some(session) = session {
            self.session = session;
        }
        out.out(&format!(
            "workspace {}, recording in session {}\n",
            root.display(),
            self.session.name()
        ));
        (self.cwd, self.root) = (cwd, root);
        0
    }
}

/// What a call prints, as `ned` prints it: stdout, then stderr.
#[derive(Default)]
struct Transcript {
    stdout: String,
    stderr: String,
}

impl Output for Transcript {
    fn out(&mut self, text: &str) {
        self.stdout.push_str(text);
    }

    fn message(&mut self, message: &str) {
        self.stderr.push_str(message);
        self.stderr.push('\n');
    }
}

impl Transcript {
    /// The call's result, failed if `exit` isn't 0.
    fn result(self, exit: u8) -> Value {
        let mut text = self.stdout + &self.stderr;
        if exit != 0 {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text += &format!("exit {exit}\n");
        }
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": exit != 0,
        })
    }
}

/// A call's `arguments`, each checked against its type in the tool's schema.
struct Arguments<'a>(&'a Value);

impl Arguments<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|value| !value.is_null())
    }

    fn string(&self, key: &str) -> Result<Option<String>, (i64, String)> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(wrong(
                key,
                "a string",
                &format!("quote it, as in `\"{key}\": \"...\"`"),
            )),
        }
    }

    fn required(&self, key: &str) -> Result<String, (i64, String)> {
        let missing = || {
            (
                INVALID_PARAMS,
                format!("missing argument `{key}`; add it to the call's `arguments`"),
            )
        };
        self.string(key)?.ok_or_else(missing)
    }

    fn flag(&self, key: &str) -> Result<bool, (i64, String)> {
        match self.get(key) {
            None => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(wrong(key, "a boolean", "give `true` or `false`")),
        }
    }

    fn strings(&self, key: &str) -> Result<Vec<String>, (i64, String)> {
        let strings = match self.get(key) {
            None => return Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| item.as_str().map(String::from))
                .collect(),
            Some(_) => None,
        };
        strings.ok_or_else(|| {
            wrong(
                key,
                "an array of strings",
                &format!("give a list, as in `\"{key}\": [\"a.rs\"]`"),
            )
        })
    }
}

/// Whether `script`, a `show` the `show` tool builds, parses as more than that
/// one command, as `fn:a; delete fn:b` would make it. A script that doesn't
/// parse runs, to fail as `ned` would.
fn shows_more_than_a_selector(script: &str) -> bool {
    script::parse(script).is_ok_and(|parsed| {
        let show = matches!(
            parsed.commands[..],
            [Command {
                kind: CommandKind::Show { .. },
                ..
            }]
        );
        !show || !parsed.stages.is_empty()
    })
}

fn wrong(key: &str, what: &str, fix: &str) -> (i64, String) {
    (
        INVALID_PARAMS,
        format!("argument `{key}` must be {what}; {fix}"),
    )
}

/// A response to the request `id` that failed with `code`.
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// The tools, as `tools/list` lists them (spec §1.5).
fn tools() -> Value {
    let flag = |description: &str| json!({ "type": "boolean", "description": description });
    let files = json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Files and globs to start with, relative to the server's working directory",
    });
    let workspace = flag("Start with every file in the workspace (-w) instead of files");
    let tool =
        |name: &str, description: &str, read_only: bool, properties: Value, required: &[&str]| {
            json!({
                "name": name,
                "description": description,
                "inputSchema": { "type": "object", "properties": properties, "required": required },
                "annotations": { "readOnlyHint": read_only },
            })
        };
    let topics: Vec<&str> = help::TOPICS.iter().map(|(name, _)| *name).collect();
    json!([
        tool(
            "ned",
            help::SUMMARY,
            false,
            json!({
                "script": {
                    "type": "string",
                    "description": "The script: commands separated by newlines or `;`",
                },
                "comment": {
                    "type": "string",
                    "description": "What the call is for, in a sentence: recorded in the session, and shown to a human following it",
                },
                "files": files,
                "workspace": workspace,
                "dry_run": flag("Apply the edits in memory and print them, but write nothing (-n)"),
                "quiet": flag("Print only the per-file summary lines (-q)"),
                "force": flag("Skip the parse-error guard and blocking on introduced errors (--force)"),
                "no_fmt": flag("Don't run formatters (--no-fmt)"),
                "no_check": flag("Don't check edits with language servers (--no-check)"),
                "commit": {
                    "type": "string",
                    "description": "Commit the edits written, and nothing else, to git with this message (--commit)",
                },
            }),
            &["script"],
        ),
        tool(
            "outline",
            "List the files' syntax items, each a selector to paste into a script (ned's `outline`)",
            true,
            json!({ "files": files, "workspace": workspace }),
            &[],
        ),
        tool(
            "show",
            "Print the lines a selector matches, numbered (ned's `show SELECTOR`); `all /re/ +2` searches, with 2 lines of context",
            true,
            json!({
                "selector": { "type": "string", "description": "The selector, as written after `show`" },
                "files": files,
                "workspace": workspace,
            }),
            &["selector"],
        ),
        tool(
            "history",
            "The session's last 10 calls, oldest first, or every one (ned history)",
            true,
            json!({ "all": flag("Every call, not just the last 10 (--all)") }),
            &[],
        ),
        tool(
            "undo",
            "Revert the files of the session's last call that changed files and isn't undone (ned undo); repeat to walk back",
            false,
            json!({ "force": flag("Merge the undo into files changed since (--force)") }),
            &[],
        ),
        tool(
            "help",
            "The command language's summary, or the details of one topic (ned help)",
            true,
            json!({
                "topic": {
                    "type": "string",
                    "description": format!("One of: {}", topics.join(" ")),
                },
            }),
            &[],
        ),
        tool(
            "cd",
            "Move the server to dir, as if started there, for the rest of the session: later calls' files are relative to it, and its workspace is dir's, and so is its session if that's another workspace",
            true,
            json!({
                "dir": { "type": "string", "description": "The directory, relative to the server's working directory" },
            }),
            &["dir"],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ned_core::lsp::{
        Diagnosis, Document, Formatting, Locate, Located, LspFailure, Position, Renamed,
    };
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    /// Language servers no test reaches.
    struct NoLsp;

    impl Lsp for NoLsp {
        fn diagnose(&mut self, _: &[Document], _: bool) -> Result<Diagnosis, LspFailure> {
            unreachable!()
        }

        fn sync(&mut self, _: &[Document]) -> Result<(), LspFailure> {
            unreachable!()
        }

        fn rename(&mut self, _: &Document, _: Position, _: &str) -> Result<Renamed, LspFailure> {
            unreachable!()
        }

        fn locate(&mut self, _: Locate, _: &Document, _: Position) -> Result<Located, LspFailure> {
            unreachable!()
        }

        fn format(&mut self, _: &Document) -> Result<Formatting, LspFailure> {
            unreachable!()
        }
    }

    /// The responses a server in a fresh workspace, recording into session
    /// `mcp-1`, writes for `input`.
    fn serve(input: impl AsRef<[u8]>) -> Vec<Value> {
        let dir = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let mut server = Server {
            cwd: root.clone(),
            session: Session::new(&state.path().join("ned"), &root, "mcp-1").unwrap(),
            named: false,
            root,
            lang: None,
            context: 1,
            version: "1.2.3".to_string(),
            connect: |_: PathBuf| NoLsp,
        };
        let mut output = Vec::new();
        server
            .serve(Cursor::new(input.as_ref()), &mut output)
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn request(id: Value, method: &str, params: Value) -> String {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string() + "\n"
    }

    fn initialize(version: &str) -> String {
        let params = json!({
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        });
        request(json!(1), "initialize", params)
    }

    fn error_code(response: &Value) -> i64 {
        response["error"]["code"].as_i64().unwrap()
    }

    fn tools() -> Vec<Value> {
        let responses = serve(request(json!(1), "tools/list", json!({})));
        responses[0]["result"]["tools"].as_array().unwrap().clone()
    }

    fn tool(name: &str) -> Value {
        let found = tools().into_iter().find(|tool| tool["name"] == name);
        found.unwrap_or_else(|| panic!("no tool {name}"))
    }

    #[test]
    fn initialize_answers_with_a_version_it_knows() {
        for version in VERSIONS {
            let responses = serve(initialize(version));
            assert_eq!(responses[0]["result"]["protocolVersion"], *version);
        }
    }

    #[test]
    fn initialize_answers_an_unknown_version_with_the_latest() {
        let responses = serve(initialize("1999-01-01"));
        assert_eq!(responses[0]["result"]["protocolVersion"], VERSIONS[0]);
    }

    #[test]
    fn initialize_names_the_server_its_tools_and_its_session() {
        let responses = serve(initialize(VERSIONS[0]));
        let response = &responses[0];
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        let result = &response["result"];
        assert_eq!(
            result["serverInfo"],
            json!({"name": "ned", "version": "1.2.3"})
        );
        assert!(result["capabilities"]["tools"].is_object());
        let instructions = result["instructions"].as_str().unwrap();
        assert!(instructions.contains("mcp-1"), "{instructions}");
    }

    #[test]
    fn ping_answers_with_an_empty_result_and_the_requests_id() {
        let responses = serve(request(json!("a"), "ping", json!({})));
        assert_eq!(
            responses,
            [json!({"jsonrpc": "2.0", "id": "a", "result": {}})]
        );
    }

    #[test]
    fn notifications_get_no_answer() {
        let input = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()
            + "\n"
            + &json!({"jsonrpc": "2.0", "method": "no/such"}).to_string()
            + "\n";
        assert_eq!(serve(input), Vec::<Value>::new());
    }

    #[test]
    fn requests_are_answered_in_order_and_blank_lines_skipped() {
        let input =
            request(json!(1), "ping", json!({})) + "\n" + &request(json!(2), "ping", json!({}));
        let ids: Vec<Value> = serve(input).iter().map(|r| r["id"].clone()).collect();
        assert_eq!(ids, [json!(1), json!(2)]);
    }

    #[test]
    fn a_line_that_isnt_json_is_a_parse_error() {
        let responses = serve("{not json\n");
        assert_eq!(error_code(&responses[0]), -32700);
        assert_eq!(responses[0]["id"], Value::Null);
        let message = responses[0]["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("not JSON: "), "{message}");
        assert!(
            message.ends_with("; send one JSON-RPC message per line"),
            "{message}"
        );
    }

    #[test]
    fn a_line_that_isnt_utf8_is_a_parse_error_and_serving_goes_on() {
        let mut input = b"\xff\xfe\n".to_vec();
        input.extend(request(json!(2), "ping", json!({})).bytes());
        let responses = serve(input);
        assert_eq!(error_code(&responses[0]), -32700);
        assert_eq!(responses[0]["id"], Value::Null);
        assert_eq!(
            responses[1],
            json!({"jsonrpc": "2.0", "id": 2, "result": {}})
        );
    }

    #[test]
    fn a_batch_is_an_invalid_request() {
        let batch = format!("[{}]\n", request(json!(1), "ping", json!({})).trim_end());
        let responses = serve(batch);
        assert_eq!(error_code(&responses[0]), -32600);
        assert_eq!(responses[0]["id"], Value::Null);
    }

    #[test]
    fn a_message_without_a_method_is_an_invalid_request() {
        let input = json!({"jsonrpc": "2.0", "id": 7}).to_string() + "\n";
        let responses = serve(input);
        assert_eq!(error_code(&responses[0]), -32600);
        assert_eq!(responses[0]["id"], 7);
        let message = &responses[0]["error"]["message"];
        assert_eq!(
            message,
            "a request needs a method; add one, such as `\"method\": \"tools/call\"`"
        );
    }

    #[test]
    fn responses_from_the_client_get_no_answer() {
        let input = json!({"jsonrpc": "2.0", "id": 7, "result": {}}).to_string()
            + "\n"
            + &json!({"jsonrpc": "2.0", "id": 8, "error": {"code": -1, "message": "no"}})
                .to_string()
            + "\n";
        assert_eq!(serve(input), Vec::<Value>::new());
    }

    #[test]
    fn bad_arguments_say_how_to_give_them() {
        let call = |name: &str, arguments: Value| {
            let params = json!({"name": name, "arguments": arguments});
            let responses = serve(request(json!(1), "tools/call", params));
            assert_eq!(error_code(&responses[0]), -32602);
            responses[0]["error"]["message"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            call("ned", json!({})),
            "missing argument `script`; add it to the call's `arguments`"
        );
        assert_eq!(
            call("help", json!({"topic": 1})),
            "argument `topic` must be a string; quote it, as in `\"topic\": \"...\"`"
        );
        assert_eq!(
            call("history", json!({"all": "yes"})),
            "argument `all` must be a boolean; give `true` or `false`"
        );
        assert_eq!(
            call("outline", json!({"files": "a.rs"})),
            "argument `files` must be an array of strings; give a list, as in `\"files\": [\"a.rs\"]`"
        );
    }

    #[test]
    fn a_commit_without_a_message_says_to_give_one() {
        let params = json!({"name": "ned", "arguments": {"script": "outline", "commit": ""}});
        let responses = serve(request(json!(1), "tools/call", params));
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(
            text,
            "error: `commit` needs a message; give one, or drop `commit`\nexit 2\n"
        );
    }

    #[test]
    fn a_call_without_a_name_says_to_give_one() {
        let responses = serve(request(json!(1), "tools/call", json!({"arguments": {}})));
        assert_eq!(error_code(&responses[0]), -32602);
        assert_eq!(
            responses[0]["error"]["message"],
            "tools/call needs the tool's `name`; add one, such as `\"name\": \"ned\"`"
        );
    }

    #[test]
    fn an_unknown_tool_lists_every_tool() {
        let params = json!({"name": "frobnicate", "arguments": {}});
        let responses = serve(request(json!(1), "tools/call", params));
        assert_eq!(error_code(&responses[0]), -32602);
        let message = responses[0]["error"]["message"].as_str().unwrap();
        assert!(
            message.ends_with("; tools are ned outline show history undo help cd"),
            "{message}"
        );
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let responses = serve(request(json!(3), "resources/list", json!({})));
        assert_eq!(error_code(&responses[0]), -32601);
        assert_eq!(responses[0]["id"], 3);
        let message = &responses[0]["error"]["message"];
        assert_eq!(
            message,
            "unknown method `resources/list`; methods are initialize ping tools/list tools/call"
        );
    }

    #[test]
    fn tools_list_has_every_tool() {
        let names: Vec<Value> = tools().iter().map(|tool| tool["name"].clone()).collect();
        assert_eq!(
            names,
            ["ned", "outline", "show", "history", "undo", "help", "cd"]
        );
    }

    #[test]
    fn the_ned_tool_is_described_by_the_help_summary() {
        assert_eq!(tool("ned")["description"], ned_core::help::SUMMARY);
    }

    #[test]
    fn reads_are_marked_read_only() {
        for (name, read_only) in [
            ("ned", false),
            ("outline", true),
            ("show", true),
            ("history", true),
            ("undo", false),
            ("help", true),
            ("cd", true),
        ] {
            let hint = &tool(name)["annotations"]["readOnlyHint"];
            assert_eq!(hint.as_bool().unwrap_or(false), read_only, "{name}");
        }
    }

    #[test]
    fn tool_arguments_follow_the_spec() {
        let properties = |name: &str| -> Vec<String> {
            let schema = &tool(name)["inputSchema"];
            assert_eq!(schema["type"], "object");
            let mut keys: Vec<String> = schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            keys.sort();
            keys
        };
        let required = |name: &str| tool(name)["inputSchema"]["required"].clone();
        assert_eq!(
            properties("ned"),
            [
                "comment",
                "commit",
                "dry_run",
                "files",
                "force",
                "no_check",
                "no_fmt",
                "quiet",
                "script",
                "workspace"
            ]
        );
        assert_eq!(required("ned"), json!(["script"]));
        assert_eq!(properties("outline"), ["files", "workspace"]);
        assert_eq!(properties("show"), ["files", "selector", "workspace"]);
        assert_eq!(required("show"), json!(["selector"]));
        assert_eq!(properties("history"), ["all"]);
        assert_eq!(properties("undo"), ["force"]);
        assert_eq!(properties("help"), ["topic"]);
        assert_eq!(properties("cd"), ["dir"]);
        assert_eq!(required("cd"), json!(["dir"]));
        let files = &tool("ned")["inputSchema"]["properties"]["files"];
        assert_eq!(files["type"], "array");
        assert_eq!(files["items"]["type"], "string");
    }

    #[test]
    fn serving_ends_with_its_input() {
        assert!(serve("").is_empty());
    }
}
