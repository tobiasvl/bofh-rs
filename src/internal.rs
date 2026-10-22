//! Internal, local commands to the bofh client.
//!
//! They share the first-word namespace with the server's command groups, so they abbreviate,
//! complete and highlight the same way, and `help` lists them alongside the
//! server's own groups.

use crate::Outcome;
use crate::helper::BofhHelper;
use bofh::{Bofh, BofhError};
use std::env;
use std::fmt::Write;
use std::fs::{File, OpenOptions, read_to_string};
use std::io::Write as _;
use std::path::PathBuf;

/// The flag `source` takes, which may be abbreviated to any prefix of itself.
const IGNORE_ERRORS: &str = "--ignore-errors";

/// What the client should do when an internal command is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Commands,
    Help,
    Quit,
    Script,
    Source,
}

/// A command the client answers locally.
pub(crate) struct InternalCommand {
    /// The command name.
    pub(crate) name: &'static str,
    /// How the command is invoked.
    usage: &'static str,
    /// Description of the command, used by `help`.
    description: &'static str,
    kind: Kind,
}

impl InternalCommand {
    /// Whether the words after this command name another command, as `help user info` does.
    ///
    /// The line editor needs to know, so that it completes and colours those words as the
    /// command they name rather than as subcommands of this one, which has none.
    pub(crate) fn names_a_command(&self) -> bool {
        self.kind == Kind::Help
    }
}

/// Every internal command, in the order they are listed in, which is the order they are shown in.
pub(crate) const INTERNAL_COMMANDS: &[InternalCommand] = &[
    InternalCommand {
        name: "commands",
        usage: "commands",
        description: "list every command, with its arguments",
        kind: Kind::Commands,
    },
    InternalCommand {
        name: "help",
        usage: "help | help <group> | help <group> <command> | help arg_help <reference>",
        description: "explain a command group, a command or an argument",
        kind: Kind::Help,
    },
    InternalCommand {
        name: "logout",
        usage: "logout",
        description: "log out and quit",
        kind: Kind::Quit,
    },
    InternalCommand {
        name: "quit",
        usage: "quit",
        description: "log out and quit",
        kind: Kind::Quit,
    },
    InternalCommand {
        name: "script",
        usage: "script | script <file>",
        description: "copy the session to a file, or stop copying",
        kind: Kind::Script,
    },
    InternalCommand {
        name: "source",
        usage: "source [--ignore-errors] <file>",
        description: "run the commands in a file",
        kind: Kind::Source,
    },
];

/// Look up an internal command by its full name.
pub(crate) fn lookup(name: &str) -> Option<&'static InternalCommand> {
    INTERNAL_COMMANDS
        .iter()
        .find(|command| command.name == name)
}

/// Run an internal command, with the words following it as its arguments.
///
/// # Errors
///
/// As for a server command, the `Err` is only for the two errors the caller can do something
/// about by re-establishing the session — and only `help` asks the server anything, so only
/// `help` can return one. Anything else is a message for the user, and so an
/// [`Outcome::Failed`].
pub(crate) fn run(
    command: &InternalCommand,
    args: &[&str],
    bofh: &Bofh,
    helper: &BofhHelper,
    script_file: &mut Option<Script>,
) -> Result<Outcome, BofhError> {
    Ok(match command.kind {
        Kind::Commands if args.is_empty() => Outcome::Output(commands(helper)),
        Kind::Quit if args.is_empty() => Outcome::Quit,
        Kind::Help => match help(args, bofh, helper) {
            Ok(help) => Outcome::Output(help),
            Err(HelpError::Message(message)) => Outcome::Failed(message),
            Err(HelpError::Session(error)) => return Err(error),
        },
        Kind::Script => script(args, script_file).map_or_else(Outcome::Failed, Outcome::Output),
        Kind::Source => source(args).map_or_else(Outcome::Failed, Outcome::Sourced),
        _ => Outcome::Failed(format!("Usage: {}", command.usage)),
    })
}

/// List every command the user can run, as they would type it.
fn commands(helper: &BofhHelper) -> String {
    let mut lines: Vec<String> = Vec::new();

    for group in helper.commands.values() {
        for command in group.commands.values() {
            let mut line = format!("{} {}", group.name, command.name);
            if command.prompt_func {
                // The server hasn't said what these are; it asks for them one at a time
                line.push_str(" <prompted by the server>");
            }
            for argument in &command.args {
                // The type is what the hinter shows as well
                let name = argument
                    .arg_type
                    .as_deref()
                    .or(argument.help_ref.as_deref())
                    .unwrap_or("argument");
                let repeat = if argument.repeat { "..." } else { "" };
                let _ = if argument.optional {
                    write!(line, " [{name}{repeat}]")
                } else {
                    write!(line, " <{name}{repeat}>")
                };
            }
            lines.push(line);
        }
    }

    lines.push(String::from("\nCommands handled by the client itself:"));
    for command in INTERNAL_COMMANDS {
        lines.push(format!("   {}", command.usage));
    }

    lines.join("\n")
}

/// Why `help` had nothing to show.
#[derive(Debug)]
enum HelpError {
    /// Something to tell the user: a name that doesn't resolve, or the server refusing.
    Message(String),
    /// The session needs re-establishing, after which the same `help` can simply be asked again.
    ///
    /// Kept as the error it is rather than turned into its message. Turning it into text was the
    /// bug: an expired session came back from `help` as the words "Session expired", which left
    /// the REPL nothing to recover from, so it printed them and carried on with a dead session.
    Session(BofhError),
}

impl From<String> for HelpError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<BofhError> for HelpError {
    fn from(error: BofhError) -> Self {
        match error {
            error @ (BofhError::ServerRestartedError | BofhError::SessionExpiredError) => {
                Self::Session(error)
            }
            error => Self::Message(error.to_string()),
        }
    }
}

/// Explain a command group, a command, an argument type, or the whole command set.
fn help(args: &[&str], bofh: &Bofh, helper: &BofhHelper) -> Result<String, HelpError> {
    match args {
        [] => Ok(general_help(bofh)?),
        // `arg_help` is not a command group but bofhd's way of naming argument types, so it isn't
        // resolved against anything; the reference comes from an argument's `help_ref`
        ["arg_help", help_ref] => Ok(bofh.help_argument(help_ref)?),
        ["arg_help", ..] => Err(String::from("Usage: help arg_help <reference>").into()),
        [topic] => {
            let topic = helper.resolve_command_name(topic)?;
            match lookup(topic) {
                Some(command) => Ok(format!("{} -- {}", command.usage, command.description)),
                None => Ok(bofh.help_group(topic)?),
            }
        }
        [group, command] => {
            let group = helper.resolve_command_name(group)?;
            if let Some(internal) = lookup(group) {
                return Err(format!(
                    "'{}' is handled by the client and has no subcommands",
                    internal.name
                )
                .into());
            }
            let command = helper.resolve_subcommand_name(group, command)?;
            Ok(bofh.help_command(group, command)?)
        }
        _ => Err(String::from(
            "Usage: help | help <group> | help <group> <command> | help arg_help <reference>",
        )
        .into()),
    }
}

/// The server's general help, with the client's own commands appended to it.
///
/// The server lists one command group per line and cannot know about the internal commands, so
/// they are added here.
fn general_help(bofh: &Bofh) -> Result<String, BofhError> {
    let mut help = bofh.help()?;

    help.push_str("\n\nCommands handled by the client itself:\n");
    for command in INTERNAL_COMMANDS {
        // The same layout the server uses for its own command groups
        let _ = writeln!(help, "   {:<10} - {}", command.name, command.description);
    }

    Ok(help)
}

/// A file the session is being copied to by `script`.
pub(crate) struct Script {
    /// The path as the user gave it, for the messages about it.
    name: String,
    file: File,
}

impl Script {
    /// Copy one line of the session to the file.
    ///
    /// A script file that has become unwritable is reported but doesn't end the session, which the
    /// user is in the middle of and may not be able to resume.
    pub(crate) fn write(&mut self, line: &str) {
        if let Err(err) = writeln!(self.file, "{line}") {
            eprintln!("Unable to write to script file {}: {err}", self.name);
        }
    }
}

/// Start or stop copying the session to a file.
fn script(args: &[&str], script_file: &mut Option<Script>) -> Result<String, String> {
    match args {
        // The closing message is not itself copied, since the file is already shut by then
        [] => Ok(match script_file.take() {
            Some(open) => format!("Closing script file {}", open.name),
            None => String::from("No script file is open"),
        }),
        [name] => {
            let file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(expand_tilde(name))
                .map_err(|err| format!("Unable to open script file {name}: {err}"))?;
            let name = (*name).to_owned();
            // Opening a second script file closes the first
            Ok(match script_file.replace(Script { name, file }) {
                Some(closed) => {
                    format!("Copying output to {}, closing {}", args[0], closed.name)
                }
                None => format!("Copying output to {}", args[0]),
            })
        }
        _ => Err(String::from("Usage: script | script <file>")),
    }
}

/// A file being read by `source`, and the lines of it that have yet to run.
#[derive(Debug)]
pub(crate) struct Source {
    /// The path as the user gave it, for the messages about it.
    pub(crate) name: String,
    /// Whether a line that fails should stop the rest of the file from running.
    pub(crate) ignore_errors: bool,
    /// The lines worth running, each with the line number it had in the file.
    lines: std::vec::IntoIter<(usize, String)>,
}

impl Source {
    /// The next line to run, if the file has one left.
    pub(crate) fn next_line(&mut self) -> Option<(usize, String)> {
        self.lines.next()
    }
}

/// Read a file of commands, to be run as if they had been typed.
fn source(args: &[&str]) -> Result<Source, String> {
    let (ignore_errors, name) = match args {
        [name] => (false, name),
        // pybofh lets the flag be abbreviated, down to a bare `-`
        [flag, name] if !flag.is_empty() && IGNORE_ERRORS.starts_with(flag) => (true, name),
        [flag, _] => return Err(format!("Expected {IGNORE_ERRORS}, found '{flag}'")),
        _ => return Err(String::from("Usage: source [--ignore-errors] <file>")),
    };

    let text = read_to_string(expand_tilde(name)).map_err(|err| format!("{name}: {err}"))?;
    // Blank lines and comments are dropped here rather than skipped while running, so that a file
    // of nothing but comments doesn't leave the REPL with an empty source to step through
    let lines: Vec<(usize, String)> = text
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(number, line)| (number, line.to_owned()))
        .collect();

    Ok(Source {
        name: (*name).to_owned(),
        ignore_errors,
        lines: lines.into_iter(),
    })
}

/// Expand a leading `~`.
fn expand_tilde(path: &str) -> PathBuf {
    let rest = match path {
        "~" => Some(""),
        path => path.strip_prefix("~/"),
    };

    // Let's not support `~someone-else`
    rest.and_then(|rest| env::home_dir().map(|home| home.join(rest)))
        .unwrap_or_else(|| PathBuf::from(path))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        HelpError, INTERNAL_COMMANDS, Script, Source, commands, expand_tilde, lookup, script,
        source,
    };
    use crate::helper::BofhHelper;
    use bofh::BofhError;
    use bofh::{Argument, Command, CommandGroup};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::{env, fs};

    /// One group whose arguments cover every way an argument can be written out.
    fn helper() -> BofhHelper {
        let arguments = [
            ("info", vec![argument("accountName", false, false)]),
            (
                "password",
                vec![
                    argument("accountName", false, false),
                    argument("accountPassword", true, false),
                ],
            ),
            ("set_spread", vec![argument("spread", false, true)]),
            ("nameless", vec![Argument::default()]),
            // The server asks for this one's arguments itself, so it advertises none
            ("create", vec![]),
        ];

        BofhHelper {
            commands: BTreeMap::from([(
                String::from("user"),
                CommandGroup {
                    name: String::from("user"),
                    commands: arguments
                        .into_iter()
                        .map(|(name, args)| {
                            (
                                name.to_owned(),
                                Command {
                                    fullname: format!("user_{name}"),
                                    name: name.to_owned(),
                                    args,
                                    prompt_func: name == "create",
                                    help: None,
                                },
                            )
                        })
                        .collect(),
                },
            )]),
        }
    }

    fn argument(arg_type: &str, optional: bool, repeat: bool) -> Argument {
        Argument {
            optional,
            repeat,
            arg_type: Some(arg_type.to_owned()),
            ..Argument::default()
        }
    }

    #[test]
    fn commands_are_listed_as_they_would_be_typed() {
        // Not as a dump of the client's own command table, which is what the other two clients do
        let listing = commands(&helper());
        let listed: Vec<&str> = listing.lines().take(5).collect();
        assert_eq!(
            listed,
            [
                // A command whose arguments the server asks for itself has none to list
                "user create <prompted by the server>",
                "user info <accountName>",
                // An argument with no type at all still has to be shown as taking something
                "user nameless <argument>",
                "user password <accountName> [accountPassword]",
                "user set_spread <spread...>",
            ]
        );
    }

    #[test]
    fn the_command_listing_includes_the_clients_own_commands() {
        let listing = commands(&helper());
        for command in INTERNAL_COMMANDS {
            assert!(
                listing.contains(command.usage),
                "'{}' is missing from the command listing",
                command.name
            );
        }
    }

    #[test]
    fn quit_and_logout_are_the_same_command_under_two_names() {
        assert_eq!(lookup("quit").unwrap().kind, lookup("logout").unwrap().kind);
        assert!(lookup("halt_and_catch_fire").is_none());
        // Abbreviations are resolved before lookup, so only full names are ever looked up
        assert!(lookup("q").is_none());
    }

    /// A file of commands that has already been read, for tests that step through the lines
    /// rather than read them.
    pub(crate) fn source_of(name: &str, ignore_errors: bool, lines: &[&str]) -> Source {
        Source {
            name: name.to_owned(),
            ignore_errors,
            lines: lines
                .iter()
                .enumerate()
                .map(|(index, &line)| (index + 1, line.to_owned()))
                .collect::<Vec<_>>()
                .into_iter(),
        }
    }

    /// An empty path of our own in the temporary directory, named after the test using it.
    fn temp_path(test: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("bofh-rs-{test}"));
        let _ = fs::remove_file(&path);
        path
    }

    /// Read a file of commands, and report the lines `source` kept and what it numbered them.
    fn sourced(test: &str, contents: &str) -> Vec<(usize, String)> {
        let path = temp_path(test);
        fs::write(&path, contents).unwrap();
        let mut source = source(&[path.to_str().unwrap()]).unwrap();

        let mut lines = Vec::new();
        while let Some(line) = source.next_line() {
            lines.push(line);
        }
        lines
    }

    #[test]
    fn sourcing_keeps_the_line_numbers_of_the_lines_it_skips() {
        // The numbers are what an error message points the user at, so they count the file as
        // written, not the commands that came out of it
        let lines = sourced(
            "skipping",
            "# a comment\nuser info foo\n\n   \n  user info bar  \n#\n",
        );
        assert_eq!(
            lines,
            [
                (2, String::from("user info foo")),
                (5, String::from("user info bar")),
            ]
        );
    }

    #[test]
    fn sourcing_a_file_of_nothing_but_comments_yields_no_lines() {
        assert!(sourced("comments", "# one\n\n# two\n").is_empty());
    }

    #[test]
    fn the_ignore_errors_flag_can_be_abbreviated() {
        let path = temp_path("flag");
        fs::write(&path, "user info foo\n").unwrap();
        let path = path.to_str().unwrap();

        assert!(!source(&[path]).unwrap().ignore_errors);
        for flag in ["--ignore-errors", "--ig", "-"] {
            assert!(source(&[flag, path]).unwrap().ignore_errors, "{flag}");
        }
        assert_eq!(
            source(&["--quietly", path]).unwrap_err(),
            "Expected --ignore-errors, found '--quietly'"
        );
    }

    #[test]
    fn sourcing_a_file_that_is_not_there_fails() {
        let missing = temp_path("missing");
        let error = source(&[missing.to_str().unwrap()]).unwrap_err();
        assert!(error.starts_with(missing.to_str().unwrap()), "{error}");
    }

    #[test]
    fn a_script_file_is_opened_written_and_closed() {
        let path = temp_path("script");
        let name = path.to_str().unwrap();
        let mut open: Option<Script> = None;

        assert_eq!(
            script(&[name], &mut open),
            Ok(format!("Copying output to {name}"))
        );
        open.as_mut().unwrap().write("user info foo");
        assert_eq!(
            script(&[], &mut open),
            Ok(format!("Closing script file {name}"))
        );
        assert!(open.is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), "user info foo\n");

        // A second `script` appends to the file rather than truncating it
        script(&[name], &mut open).unwrap();
        open.as_mut().unwrap().write("user info bar");
        script(&[], &mut open).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "user info foo\nuser info bar\n"
        );
    }

    #[test]
    fn closing_a_script_file_that_was_never_opened_is_not_an_error() {
        let mut open: Option<Script> = None;
        assert_eq!(
            script(&[], &mut open),
            Ok(String::from("No script file is open"))
        );
    }

    #[test]
    fn opening_a_second_script_file_closes_the_first() {
        let first = temp_path("script-first");
        let second = temp_path("script-second");
        let (first, second) = (first.to_str().unwrap(), second.to_str().unwrap());
        let mut open: Option<Script> = None;

        script(&[first], &mut open).unwrap();
        assert_eq!(
            script(&[second], &mut open),
            Ok(format!("Copying output to {second}, closing {first}"))
        );
        assert_eq!(
            open.as_ref().map(|open| open.name.clone()),
            Some(String::from(second))
        );
    }

    #[test]
    fn a_leading_tilde_is_expanded_but_someone_elses_is_not() {
        let home = env::home_dir().expect("No home directory to expand '~' to");
        assert_eq!(expand_tilde("~/scripts/foo"), home.join("scripts/foo"));
        assert_eq!(expand_tilde("~"), home.join(""));
        // Looking up another user's home needs a password database we have no business reading
        assert_eq!(expand_tilde("~root/foo"), PathBuf::from("~root/foo"));
        assert_eq!(expand_tilde("/tmp/foo"), PathBuf::from("/tmp/foo"));
    }

    /// `help person find` on an expired session used to print "Session expired" and carry on,
    /// because the error was turned into its message before anyone could act on it. The two
    /// errors a new session can fix have to survive as errors; the rest are only ever read.
    #[test]
    fn help_keeps_the_errors_a_new_session_can_fix() {
        assert!(matches!(
            HelpError::from(BofhError::SessionExpiredError),
            HelpError::Session(BofhError::SessionExpiredError)
        ));
        assert!(matches!(
            HelpError::from(BofhError::ServerRestartedError),
            HelpError::Session(BofhError::ServerRestartedError)
        ));
        match HelpError::from(BofhError::CerebrumError(String::from("No such command"))) {
            HelpError::Message(message) => assert_eq!(message, "No such command"),
            other @ HelpError::Session(_) => panic!("expected a message, got {other:?}"),
        }
    }
}
