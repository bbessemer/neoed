// `print!` and `println!` that ignore a closed stdout: the script still
// completes when the reader stops early (`ned ... | head`).
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stdout(), $($arg)*);
    }};
}

macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

// `eprintln!` with the message's `error:` or `note:` painted for stderr.
macro_rules! errln {
    ($($arg:tt)*) => {{
        eprintln!("{}", $crate::styles().1.message(&format!($($arg)*)));
    }};
}

mod daemon;
mod mcp;
mod repl;
mod session;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::OnceLock;

use clap::builder::{NonEmptyStringValueParser, PossibleValuesParser};
use clap::{Parser, Subcommand};
use ned_core::config;
use ned_core::invoke::{self, Failure, Invocation, Output};
use ned_core::lang::{self, Language};
use ned_core::style::{Depth, Style, When};
use ned_core::theme::Theme;
use ned_core::{help, script, workspace};

/// A `--lang` value, `None` for text. Clap would read `Option<Option<_>>` as a
/// flag whose value is optional.
type LangFlag = Option<Language>;

/// Token-economical, syntax-aware line editor for AI agents.
#[derive(Parser)]
#[command(
    name = "ned",
    version = env!("NED_VERSION"),
    args_conflicts_with_subcommands = true,
    disable_help_subcommand = true,
    // clap leaves a user-defined `help` subcommand out of the usage.
    override_usage = "ned [OPTIONS] [FILES... | -w [DIR]] [-e SCRIPT]...\n       ned repl [OPTIONS] [FILES... | -w [DIR]]    (edit interactively)\n       ned mcp [OPTIONS]    (serve agents over MCP)\n       ned help [TOPIC]    (the command language)\n       ned daemon start|status|stop [DIR]\n       ned history|undo [-s NAME] [-w DIR]\n       ned session list|delete"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Files to edit: the initial file set.
    files: Vec<String>,
    /// Start with every file in the workspace, DIR or the one containing the
    /// working directory, instead of FILES.
    #[arg(short, long, value_name = "DIR", num_args = 0..=1, conflicts_with = "files")]
    workspace: Option<Option<PathBuf>>,
    /// A script to run; repeat to join several with newlines. Without -e, the
    /// script is read from stdin.
    #[arg(short = 'e', value_name = "SCRIPT")]
    scripts: Vec<String>,
    /// Resolve and apply edits in memory and print the output, but write
    /// nothing.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Print only the per-file summary lines on success.
    #[arg(short, long)]
    quiet: bool,
    /// Skip the parse-error guard: apply edits even if they introduce syntax
    /// errors.
    #[arg(long)]
    force: bool,
    /// Don't run formatters.
    #[arg(long)]
    no_fmt: bool,
    /// Don't check edits with language servers.
    #[arg(long)]
    no_check: bool,
    /// Use this language for every file instead of detecting it, or `text` to
    /// parse none of them.
    #[arg(long, value_name = "LANG", value_parser = lang::parse_lang)]
    lang: Option<LangFlag>,
    /// Context lines around diff hunks.
    #[arg(long, value_name = "N", default_value_t = 1)]
    context: usize,
    /// Record the invocation in session NAME; overrides NED_SESSION.
    #[arg(short, long, value_name = "NAME")]
    session: Option<String>,
    /// Colour output for a terminal: auto, always or never.
    #[arg(long, value_name = "WHEN", default_value = "auto", global = true)]
    color: When,
    /// Commit the edits written, and nothing else, to git with message MSG.
    #[arg(long, value_name = "MSG", conflicts_with = "dry_run", value_parser = NonEmptyStringValueParser::new())]
    commit: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Edit interactively, in memory until written (`ned help repl`).
    Repl(repl::ReplArgs),
    /// Serve ned to an agent over the Model Context Protocol (`ned help mcp`).
    Mcp(mcp::McpArgs),
    /// Print a summary of the command language, or details of one topic.
    Help {
        #[arg(value_parser = PossibleValuesParser::new(help::TOPICS.iter().map(|(name, _)| name)))]
        topic: Option<String>,
    },
    /// Manage the language-server daemon for the workspace containing DIR.
    Daemon {
        #[command(subcommand)]
        action: daemon::Action,
    },
    /// Print the session's last 10 entries.
    History {
        /// The session; overrides NED_SESSION.
        #[arg(short, long, value_name = "NAME")]
        session: Option<String>,
        /// The workspace the session belongs to, instead of the working
        /// directory's.
        #[arg(short, long, value_name = "DIR")]
        workspace: Option<PathBuf>,
        /// Print every entry.
        #[arg(long)]
        all: bool,
    },
    /// Undo the session's last edit that isn't undone.
    Undo {
        /// The session; overrides NED_SESSION.
        #[arg(short, long, value_name = "NAME")]
        session: Option<String>,
        /// The workspace the session belongs to, instead of the working
        /// directory's.
        #[arg(short, long, value_name = "DIR")]
        workspace: Option<PathBuf>,
        /// Merge the undo into files changed since.
        #[arg(long)]
        force: bool,
    },
    /// List or delete sessions.
    Session {
        #[command(subcommand)]
        action: session::Action,
    },
}

/// The styles of stdout and stderr, from `--color` (spec §6.6).
static STYLES: OnceLock<(Style, Style)> = OnceLock::new();

fn styles() -> (Style, Style) {
    STYLES.get().copied().unwrap_or_default()
}

/// The `--color` word among `args`, for an error found before clap has
/// parsed them.
fn color_arg(args: impl Iterator<Item = OsString>) -> When {
    let mut when = When::default();
    let mut args = args.map(|arg| arg.to_string_lossy().into_owned());
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--" => break,
            "--color" => args.next(),
            _ => arg.strip_prefix("--color=").map(str::to_owned),
        };
        if let Some(word) = value.and_then(|value| value.parse().ok()) {
            when = word;
        }
    }
    when
}

/// `styles`, of stdout and stderr, with the theme `read` gives, if any, when
/// stdout colours on a terminal of `depth`. Only then is the theme read, so a
/// bad one stops no run whose output a program reads (spec §6.6).
fn themed<E>(
    styles: (Style, Style),
    depth: Option<Depth>,
    read: impl FnOnce() -> Result<Option<&'static Theme>, E>,
) -> Result<(Style, Style), E> {
    if depth.is_none() || styles.0 != Style::Color {
        return Ok(styles);
    }
    let theme = read()?;
    Ok((styles.0.themed(theme, depth), styles.1.themed(theme, depth)))
}

fn main() -> ExitCode {
    let no_color = std::env::var_os("NO_COLOR");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let terminal = match err.use_stderr() {
                true => io::stderr().is_terminal(),
                false => io::stdout().is_terminal(),
            };
            let args = std::env::args_os().skip(1);
            let text = err.render();
            let text = match color_arg(args).style(terminal, no_color.as_deref()) {
                Style::Plain => text.to_string(),
                _ => text.ansi().to_string(),
            };
            match err.use_stderr() {
                true => eprint!("{text}"),
                false => out!("{text}"),
            }
            return ExitCode::from(err.exit_code() as u8);
        }
    };
    let style = |terminal| cli.color.style(terminal, no_color.as_deref());
    let styles = (
        style(io::stdout().is_terminal()),
        style(io::stderr().is_terminal()),
    );
    let var = std::env::var_os;
    let depth = Depth::detect(var("COLORTERM").as_deref(), var("TERM").as_deref());
    let read = || {
        let theme = config::user_theme(config::user_config().as_deref())?;
        Ok::<_, config::ConfigError>(theme.map(|theme| &*Box::leak(Box::new(theme))))
    };
    match themed(styles, depth, read) {
        Ok(styles) => {
            let _ = STYLES.set(styles);
        }
        Err(err) => {
            let _ = STYLES.set(styles);
            errln!("error: {err}");
            return ExitCode::from(2);
        }
    }
    match cli.command {
        Some(Command::Repl(args)) => return repl::run(args),
        Some(Command::Mcp(args)) => return mcp::run(args),
        Some(Command::Help { topic }) => {
            out!(
                "{}",
                help::text(topic.as_deref()).expect("clap checks the topic")
            );
            return ExitCode::SUCCESS;
        }
        Some(Command::Daemon { action }) => return daemon::run(action),
        Some(Command::History {
            session,
            workspace,
            all,
        }) => return finish(session::history(session, workspace, all)),
        Some(Command::Undo {
            session,
            workspace,
            force,
        }) => return finish(session::undo(session, workspace, force)),
        Some(Command::Session { action }) => return finish(session::run(action)),
        None => {}
    }
    if cli.scripts.is_empty() && io::stdin().is_terminal() && usage_error(&cli).is_none() {
        return match repl::ReplArgs::from_cli(cli) {
            Ok(args) => repl::run(args),
            Err(err) => {
                errln!("error: {err}");
                ExitCode::from(2)
            }
        };
    }
    if let Some(err) = usage_error(&cli) {
        errln!("error: {err}");
        return ExitCode::from(2);
    }
    let src = if cli.scripts.is_empty() {
        let mut src = String::new();
        if let Err(err) = io::stdin().read_to_string(&mut src) {
            errln!("error: cannot read the script from stdin: {err}");
            return ExitCode::from(2);
        }
        src
    } else {
        cli.scripts.join("\n")
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = match &cli.workspace {
        Some(Some(dir)) => match dir.canonicalize() {
            Ok(dir) => dir,
            Err(err) => {
                errln!("error: cannot read {}: {err}", dir.display());
                return ExitCode::from(3);
            }
        },
        _ => workspace::root(&cwd).unwrap_or(cwd.clone()),
    };
    let session = match invoke::session_name(cli.session.clone()) {
        None => None,
        Some(name) => match invoke::open(&name, &root) {
            Ok(session) => Some(session),
            Err(failure) => return finish(Err(failure)),
        },
    };
    let invocation = Invocation {
        cwd,
        files: cli.files,
        workspace: cli.workspace.is_some(),
        root,
        dry_run: cli.dry_run,
        quiet: cli.quiet,
        force: cli.force,
        no_fmt: cli.no_fmt,
        no_check: cli.no_check,
        lang: cli.lang,
        context: cli.context,
        commit: cli.commit,
        style: crate::styles().0,
        comment: None,
    };
    let exit = invoke::invoke(
        invocation,
        src,
        session.as_ref(),
        daemon::workspace,
        &mut Terminal,
    );
    ExitCode::from(exit)
}

/// Prints an invocation's output to stdout, and its messages to stderr.
struct Terminal;

impl Output for Terminal {
    fn out(&mut self, text: &str) {
        out!("{text}");
    }

    fn message(&mut self, message: &str) {
        errln!("{message}");
    }
}

/// The exit code of a subcommand, printing its error if it failed.
fn finish(result: Result<(), Failure>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err((error, code)) => {
            errln!("{error}");
            ExitCode::from(code)
        }
    }
}

/// A usage error found before the script is read (spec §1): a command's name
/// given as a FILE.
fn usage_error(cli: &Cli) -> Option<String> {
    let exists = |arg: &str| std::path::Path::new(arg).exists();
    let is_command = |arg: &str| script::error::COMMANDS.split(' ').any(|c| c == arg);
    if cli.workspace.is_none()
        && let Some(verb) = cli.files.iter().position(|a| !exists(a) && is_command(a))
    {
        // The command's words are the arguments from it on that aren't files.
        let (mut files, mut words) = (Vec::new(), Vec::new());
        for (i, arg) in cli.files.iter().enumerate() {
            match i < verb || exists(arg) {
                true => files.push(arg.as_str()),
                false => words.push(arg.as_str()),
            }
        }
        return Some(format!(
            "`{}` is a command, not a file; give the script with -e: ned {} -e '{}'",
            cli.files[verb],
            files.join(" "),
            words.join(" ")
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default() -> &'static Theme {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"default-dark\"\n").unwrap();
        let theme = config::user_theme(Some(&path)).unwrap().unwrap();
        Box::leak(Box::new(theme))
    }

    #[test]
    fn a_coloured_stdout_reads_the_theme_for_both_streams() {
        let theme = default();
        let styles = themed((Style::Color, Style::Color), Some(Depth::Xterm256), || {
            Ok::<_, ()>(Some(theme))
        });
        let painted = Style::Theme(theme, Depth::Xterm256);
        assert_eq!(styles, Ok((painted, painted)));
    }

    #[test]
    fn only_a_coloured_stdout_reads_the_theme() {
        let unread = || -> Result<Option<&'static Theme>, &str> { Err("read") };
        let depth = Some(Depth::Truecolor);
        for styles in [(Style::Plain, Style::Color), (Style::Plain, Style::Plain)] {
            assert_eq!(themed(styles, depth, unread), Ok(styles));
        }
        let styles = (Style::Color, Style::Plain);
        assert_eq!(themed(styles, None, unread), Ok(styles));
        assert_eq!(themed(styles, depth, unread), Err("read"));
    }
}
