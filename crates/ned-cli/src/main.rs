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

use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::OnceLock;

use clap::builder::{NonEmptyStringValueParser, PossibleValuesParser};
use clap::{Arg, CommandFactory, Parser, Subcommand};
use ned_core::config;
use ned_core::invoke::{self, Failure, Invocation, Output};
use ned_core::lang::{self, Language};
use ned_core::style::{Depth, Style, When};
use ned_core::theme::Theme;
use ned_core::{hint, script, workspace};

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
    override_usage = include_str!("usage.txt").trim_ascii_end()
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
    /// script is read from stdin; with -e, only in the place of a `-e -`.
    #[arg(short = 'e', value_name = "SCRIPT")]
    scripts: Vec<String>,
    /// Resolve and apply edits in memory and print the output, but write
    /// nothing.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Print only the per-file summary lines on success.
    #[arg(short, long)]
    quiet: bool,
    /// Skip the guards: apply edits even if they introduce syntax errors,
    /// language-server errors or a formatter failure.
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
        #[arg(value_parser = PossibleValuesParser::new(hint::Frontend::Cli.topics()))]
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

/// `styles`, of stdout and stderr, with the theme `read` gives when stdout
/// colours on a terminal of `depth`. Only then is the theme read, so a bad one
/// stops no run whose output a program reads (spec §6.6).
fn themed<E>(
    styles: (Style, Style),
    depth: Option<Depth>,
    read: impl FnOnce() -> Result<&'static Theme, E>,
) -> Result<(Style, Style), E> {
    if depth.is_none() || styles.0 != Style::Color {
        return Ok(styles);
    }
    let theme = Some(read()?);
    Ok((styles.0.themed(theme, depth), styles.1.themed(theme, depth)))
}

fn main() -> ExitCode {
    let no_color = std::env::var_os("NO_COLOR");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let args: Vec<_> = std::env::args_os()
                .skip(1)
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            if err.use_stderr()
                && let Some(err) = misplaced_subcommand(&args)
            {
                errln!("error: {err}");
                return ExitCode::from(2);
            }
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
        let env = var("NED_THEME");
        let theme = config::user_theme(
            config::user_config().as_deref(),
            env.as_deref().and_then(OsStr::to_str),
        )?;
        Ok::<_, config::ConfigError>(&*Box::leak(Box::new(theme)))
    };
    match themed(styles, depth, read) {
        Ok(styles) => {
            let _ = STYLES.set(styles);
        }
        Err(err) => {
            let _ = STYLES.set(styles);
            errln!("{}", err.render(hint::Frontend::Cli, None));
            return ExitCode::from(2);
        }
    }
    match cli.command {
        Some(Command::Repl(args)) => return repl::run(args),
        Some(Command::Mcp(args)) => return mcp::run(args),
        Some(Command::Help { topic }) => {
            out!(
                "{}",
                hint::Frontend::Cli
                    .text(topic.as_deref())
                    .expect("clap checks the topic")
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
    let mut scripts = cli.scripts.clone();
    if scripts.is_empty() {
        scripts.push("-".to_string());
    }
    if let Some(script) = scripts.iter_mut().find(|script| *script == "-") {
        script.clear();
        if let Err(err) = io::stdin().read_to_string(script) {
            errln!("error: cannot read the script from stdin: {err}");
            return ExitCode::from(2);
        }
    }
    let src = scripts.join("\n");
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
        frontend: hint::Frontend::Cli,
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

/// A usage error for a subcommand after a flag in `args` (spec §1), with the
/// arguments it takes moved after it.
fn misplaced_subcommand(args: &[String]) -> Option<String> {
    let mut ned = Cli::command();
    ned.build();
    let is_subcommand =
        |arg: &str| ned.find_subcommand(arg).is_some() && !std::path::Path::new(arg).exists();
    if args
        .first()
        .is_none_or(|arg| ned.find_subcommand(arg).is_some())
    {
        return None;
    }
    let flag = |found: &dyn Fn(&Arg) -> bool| ned.get_arguments().find(|arg| found(arg));
    // The arguments before the subcommand: each flag with its value (`None` for
    // a FILE), and whether a value was given.
    let mut given: Vec<(Option<&Arg>, Vec<String>, bool)> = Vec::new();
    let mut i = 0;
    let name = loop {
        let arg = args.get(i)?.as_str();
        i += 1;
        if arg == "--" {
            return None;
        }
        if is_subcommand(arg) {
            break arg;
        }
        let mut words = vec![arg.to_owned()];
        let (found, attached) = if let Some(long) = arg.strip_prefix("--") {
            let (long, attached) = match long.split_once('=') {
                Some((long, _)) => (long, true),
                None => (long, false),
            };
            (flag(&|a| a.get_long() == Some(long)), attached)
        } else if let Some(shorts) = arg.strip_prefix('-').filter(|s| !s.is_empty()) {
            // A cluster: each short flag up to one that takes a value, which is
            // the rest of the cluster, if any.
            let mut found = None;
            for (at, c) in shorts.char_indices() {
                let Some(short) = flag(&|a| a.get_short() == Some(c)) else {
                    break;
                };
                if !short.get_action().takes_values() {
                    given.push((Some(short), vec![format!("-{c}")], false));
                    continue;
                }
                words = vec![format!("-{c}")];
                let rest = &shorts[at + c.len_utf8()..];
                if !rest.is_empty() {
                    words.push(rest.to_owned());
                }
                found = Some(short);
                break;
            }
            match found {
                Some(short) => (Some(short), words.len() > 1),
                None => continue,
            }
        } else {
            given.push((None, words, false));
            continue;
        };
        let Some(found) = found else { continue };
        let mut value = attached;
        if found.get_action().takes_values() && !attached {
            let optional = found.get_num_args().is_some_and(|n| n.min_values() == 0);
            if let Some(next) = args.get(i)
                && !(optional && (next.starts_with('-') || is_subcommand(next)))
            {
                words.push(next.clone());
                value = true;
                i += 1;
            }
        }
        given.push((Some(found), words, value));
    };
    if !args[..i - 1]
        .iter()
        .any(|arg| arg.len() > 1 && arg.starts_with('-'))
    {
        return None;
    }
    let sub = ned.find_subcommand(name)?;
    let takes_files = sub
        .get_positionals()
        .any(|arg| arg.get_num_args().is_some_and(|n| n.max_values() > 1));
    let mut line = vec!["ned", name];
    for (found, words, value) in &given {
        let own = found.and_then(|found| {
            sub.get_arguments().find(|own| {
                !own.is_positional()
                    && ((own.get_long().is_some() && own.get_long() == found.get_long())
                        || (own.get_short().is_some() && own.get_short() == found.get_short()))
            })
        });
        let keep = match (found, own) {
            (None, _) => takes_files,
            (Some(_), None) => false,
            (Some(_), Some(own)) => {
                *value || own.get_num_args().is_none_or(|n| n.min_values() == 0)
            }
        };
        if keep {
            line.extend(words.iter().map(String::as_str));
        }
    }
    line.extend(args[i..].iter().map(String::as_str));
    Some(format!(
        "`{name}` is a subcommand, not a file; give it first: {}",
        line.join(" ")
    ))
}

/// A usage error found before the script is read (spec §1): a subcommand after
/// a flag, or a command's name given as a FILE.
fn usage_error(cli: &Cli) -> Option<String> {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if let Some(err) = misplaced_subcommand(&args) {
        return Some(err);
    }
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
    if cli.scripts.iter().filter(|script| *script == "-").count() > 1 {
        return Some("stdin holds one script; give `-e -` once".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default() -> &'static Theme {
        Box::leak(Box::new(
            config::user_theme(None, Some("default-dark")).unwrap(),
        ))
    }

    #[test]
    fn a_coloured_stdout_reads_the_theme_for_both_streams() {
        let theme = default();
        let styles = themed((Style::Color, Style::Color), Some(Depth::Xterm256), || {
            Ok::<_, ()>(theme)
        });
        let painted = Style::Theme(theme, Depth::Xterm256);
        assert_eq!(styles, Ok((painted, painted)));
    }

    #[test]
    fn only_a_coloured_stdout_reads_the_theme() {
        let unread = || -> Result<&'static Theme, &str> { Err("read") };
        let depth = Some(Depth::Truecolor);
        for styles in [(Style::Plain, Style::Color), (Style::Plain, Style::Plain)] {
            assert_eq!(themed(styles, depth, unread), Ok(styles));
        }
        let styles = (Style::Color, Style::Plain);
        assert_eq!(themed(styles, None, unread), Ok(styles));
        assert_eq!(themed(styles, depth, unread), Err("read"));
    }
}
