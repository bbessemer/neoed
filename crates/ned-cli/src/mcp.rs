//! `ned mcp` (command-language spec §1.5).

use std::path::PathBuf;
use std::process::ExitCode;
use std::{fmt, io};

use clap::Args;
use ned_core::hint::{Fix, Frontend, Hint};
use ned_core::invoke;
use ned_core::workspace;
use ned_mcp::Server;

use crate::{LangFlag, daemon, error};

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
        Some(dir) => match error::canonical(dir) {
            Ok(dir) => dir,
            Err(err) => return crate::fail(err),
        },
        None => workspace::root(&cwd).unwrap_or(cwd.clone()),
    };
    let name = invoke::session_name(args.session);
    let opened = ned_mcp::open_session(name.as_deref(), &root);
    let session = match invoke::report(opened, Frontend::Cli, None, &mut crate::Terminal) {
        Ok(session) => session,
        Err(failure) => return crate::finish(Err(failure)),
    };
    errln!("{}", error::recording(session.name()).render(Frontend::Cli));
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
        Err(err) => crate::fail(Disconnected(err)),
    }
}

/// The client's stdin or stdout failed.
#[derive(Debug)]
struct Disconnected(io::Error);

impl fmt::Display for Disconnected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot talk to the client: {}", self.0)
    }
}

impl Hint for Disconnected {
    fn exit_code(&self) -> u8 {
        3
    }

    fn fix(&self) -> Option<Fix> {
        Some("restart the server".into())
    }
}
