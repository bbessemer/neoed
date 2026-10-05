//! `ned mcp` (command-language spec §1.5).

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use ned_core::invoke;
use ned_core::workspace;
use ned_mcp::Server;

use crate::{LangFlag, daemon};

#[derive(Args)]
pub struct McpArgs {
    /// The workspace, instead of the one containing the working directory.
    #[arg(short, long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Record into session NAME; overrides NED_SESSION.
    #[arg(short, long, value_name = "NAME")]
    session: Option<String>,
    /// Use this language for every file instead of detecting it, or `text` to
    /// parse none of them.
    #[arg(long, value_name = "LANG", value_parser = ned_core::lang::parse_lang)]
    lang: Option<LangFlag>,
    /// Context lines around diff hunks.
    #[arg(long, value_name = "N", default_value_t = 1)]
    context: usize,
}

pub fn run(args: McpArgs) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = match &args.workspace {
        Some(dir) => match dir.canonicalize() {
            Ok(dir) => dir,
            Err(err) => {
                errln!("error: cannot read {}: {err}", dir.display());
                return ExitCode::from(3);
            }
        },
        None => workspace::root(&cwd).unwrap_or(cwd.clone()),
    };
    let name = invoke::session_name(args.session);
    let session = match ned_mcp::open_session(name.as_deref(), &root) {
        Ok(session) => session,
        Err((error, code)) => {
            errln!("{error}");
            return ExitCode::from(code);
        }
    };
    errln!("note: recording in session {}", session.name());
    let mut server = Server {
        cwd,
        root,
        session,
        named: name.is_some(),
        lang: args.lang,
        context: args.context,
        version: env!("NED_VERSION").to_string(),
        connect: daemon::workspace,
    };
    match server.serve(io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            errln!("error: {err}");
            ExitCode::from(3)
        }
    }
}
