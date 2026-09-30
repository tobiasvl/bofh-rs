use bofh::{Bofh, BofhError, CommandGroup};
use clap::Parser;
mod helper;
mod internal;
mod parser;
use crate::helper::BofhHelper;
use crate::internal::{InternalCommand, Script, Source};
use crate::parser::split_words;
use rpassword::prompt_password;
use rustyline::{Editor, config::Configurer, error::ReadlineError, history::FileHistory};
use std::collections::BTreeMap;
use std::error::Error;
use std::process::ExitCode;

/// The rustyline editor, once it knows which commands the bofhd server offers.
type BofhEditor = Editor<BofhHelper, FileHistory>;

/// How many `source` commands may be nested, so that a file sourcing itself stops rather than
/// filling memory with lines to run.
const MAX_SOURCE_DEPTH: usize = 16;

/// The Cerebrum Bofh client
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Run command and exit
    #[clap(long)]
    cmd: Option<String>,

    /// Use CA certificates from PEM
    #[clap(short, long, help_heading = "Connection settings", value_name = "PEM", default_value_t = String::from("foo"))]
    cert: String,

    /// set verbosity of log messages to N
    #[clap(long, help_heading = "Output settings", value_name = "N")]
    verbosity: Option<String>,

    /// increase verbosity of log messages
    #[clap(
        short,
        action = clap::ArgAction::Count,
        help_heading = "Output settings",
        required = false
    )]
    verbosity_level: u8,

    /// silence all log messages
    #[clap(short, long, help_heading = "Output settings")]
    quiet: bool,

    /// connect to bofhd server at URL
    #[clap(long, help_heading = "Connection settings", default_value_t = String::from("https://cerebrum-uio-test.uio.no:8000/"))]
    url: String,

    /// authenticate as USER (defaults to the current user)
    #[clap(long, short, help_heading = "Connection settings")]
    user: Option<String>,

    /// skip certificate hostname validation
    #[clap(long, help_heading = "Connection settings")]
    insecure: bool,

    /// set connection timeout to N seconds
    #[clap(
        long,
        default_value_t = 0,
        help_heading = "Connection settings",
        value_name = "N"
    )]
    timeout: u8,

    /// use vi tab completion (circular) and command mode (cheatsheet:
    /// <https://catonmat.net/ftp/bash-vi-editing-mode-cheat-sheet.pdf>)
    #[clap(long, help_heading = "REPL behavior", alias = "vim")]
    vi: bool,

    /// use a custom prompt
    #[clap(long, short, help_heading = "Prompt", default_value_t = String::from("bofh> "))]
    prompt: String,
}

fn main() -> ExitCode {
    let args = Args::parse();

    println!("Connecting to {}\n", args.url);
    let mut bofh = match Bofh::new(args.url) {
        Ok(bofh) => bofh,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(motd) = &bofh.motd {
        println!("{motd}\n");
    }

    let mut rl = Editor::new().expect("Failed to connect to terminal/TTY");

    let user = match args.user.or_else(|| whoami::username().ok()) {
        Some(user) => user,
        None => loop {
            match rl.readline("Username: ") {
                Ok(line) if !line.trim().is_empty() => break line.trim().to_owned(),
                Ok(_) => (),
                Err(ReadlineError::Interrupted | ReadlineError::Eof) => return ExitCode::FAILURE,
                Err(err) => {
                    eprintln!("Error: {err}");
                    return ExitCode::FAILURE;
                }
            }
        },
    };

    let Ok(password) = prompt_password(format!("Password for {user}: ")) else {
        return ExitCode::FAILURE; // FIXME errors on windows?
    };

    match bofh.login(&user, password) {
        Ok(commands) => rl.set_helper(Some(BofhHelper { commands })),
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    }

    if args.vi {
        rl.set_edit_mode(rustyline::EditMode::Vi);
        rl.set_completion_type(rustyline::CompletionType::Circular);
    } else {
        rl.set_completion_type(rustyline::CompletionType::List);
    }

    if rl.load_history("history.txt").is_err() {
        println!("No previous history.");
    }

    if let Err(err) = repl(&mut bofh, &mut rl, &user, &args.prompt) {
        eprintln!("Error: {err}");
        return ExitCode::FAILURE;
    }

    // We adhere to the bofh tradition of printing a sci-fi farewell message upon logout
    println!("Go, then - there are other worlds than these.");
    rl.append_history("history.txt")
        .expect("Unable to write history to history.txt");

    ExitCode::SUCCESS
}

/// Read, evaluate and print bofh commands until the user quits with Ctrl-C or Ctrl-D.
///
/// # Errors
///
/// Returns an error if the terminal can't be read, or if the bofhd session couldn't be
/// re-established after the server expired or restarted it.
fn repl(
    bofh: &mut Bofh,
    rl: &mut BofhEditor,
    user: &str,
    prompt: &str,
) -> Result<(), Box<dyn Error>> {
    // The file `script` is copying the session to, and the files `source` is reading commands
    // from, innermost last. Both belong to the loop rather than to the commands that open them,
    // since they outlive the command and only the loop sees every line.
    let mut script: Option<Script> = None;
    let mut sources: Vec<Source> = Vec::new();

    loop {
        // A sourced line runs as if it had been typed, so it is echoed after the prompt to keep
        // the transcript readable
        let (line, line_number) = match next_sourced_line(&mut sources) {
            Some((number, line)) => {
                println!("{prompt}{line}");
                (line, Some(number))
            }
            None => match rl.readline(prompt) {
                Ok(line) => (line, None),
                Err(ReadlineError::Interrupted | ReadlineError::Eof) => return Ok(()),
                Err(err) => return Err(err.into()),
            },
        };

        if let Some(script) = script.as_mut() {
            script.write(&format!("{prompt}{line}"));
        }

        let words = match split_words(&line) {
            Ok(words) if words.is_empty() => continue,
            Ok(words) => Some(words),
            Err(msg) => {
                eprintln!("{msg}");
                None
            }
        };

        let mut failed = words.is_none();

        if let Some(words) = words {
            let helper = rl.helper().expect("Failed to get rustyline helper");
            let completed = match resolve_command(helper, &words) {
                Ok(resolved) => resolved.run(bofh, helper, user, &mut script)?,
                Err(message) => Outcome::Failed(message).into(),
            };

            match completed.outcome {
                Outcome::Output(output) => {
                    println!("{output}");
                    tee(script.as_mut(), &output);
                }
                Outcome::Failed(message) => {
                    eprintln!("{message}");
                    tee(script.as_mut(), &message);
                    failed = true;
                }
                Outcome::Quit => return Ok(()),
                Outcome::Sourced(source) => {
                    if sources.len() < MAX_SOURCE_DEPTH {
                        sources.push(source);
                    } else {
                        eprintln!(
                            "Not reading {}: already {MAX_SOURCE_DEPTH} files deep. \
                             Does a sourced file source itself?",
                            source.name
                        );
                        failed = true;
                    }
                }
            }

            // The session was re-established, so the helper needs to know about the server's
            // (possibly changed) commands
            if let Some(commands) = completed.commands {
                rl.set_helper(Some(BofhHelper { commands }));
            }
        }

        match line_number {
            // Sourced lines were never typed, so they don't belong in the history
            Some(number) if failed => abort_source(&mut sources, number),
            Some(_) => (),
            None => {
                if let Err(err) = rl.add_history_entry(&line) {
                    eprintln!("Unable to save command to history: {err:?}");
                }
            }
        }
    }
}

/// Copy a command's output to the script file, if one is open.
fn tee(script: Option<&mut Script>, output: &str) {
    if let Some(script) = script {
        script.write(output);
    }
}

/// The next line of the innermost file still being sourced, skipping past files that have run out.
fn next_sourced_line(sources: &mut Vec<Source>) -> Option<(usize, String)> {
    while let Some(source) = sources.last_mut() {
        if let Some(line) = source.next_line() {
            return Some(line);
        }
        sources.pop();
    }
    None
}

/// Stop reading the file a failing line came from, unless it was sourced with `--ignore-errors`.
///
/// Only the innermost file is given up on. A `source` that fails is a failing line like any other
/// in the file that sourced it, and gets the same treatment there.
fn abort_source(sources: &mut Vec<Source>, line: usize) {
    let Some(source) = sources.last() else { return };

    if !source.ignore_errors {
        eprintln!(
            "Sourcing of {name} aborted on line {line}\n\
             Hint: use 'source --ignore-errors {name}' to carry on past errors",
            name = source.name
        );
        sources.pop();
    }
}

/// What to tell the user after a command has run.
///
/// Both kinds of command report back the same way, so the REPL has one place that decides where
/// output goes (which is where `script` will tee it to a file).
pub(crate) enum Outcome {
    /// Show this to the user.
    Output(String),
    /// Tell the user this went wrong, but carry on: a command they mistyped, or one bofhd
    /// refused. Fatal errors are the `Err` of the surrounding [`Result`] instead.
    Failed(String),
    Quit,
    /// Run this file's commands next, as if they had been typed.
    Sourced(Source),
}

/// What a command left for the REPL to do.
struct Completed {
    /// What to tell the user.
    outcome: Outcome,
    /// A refreshed command table, when the session had to be re-established while the command
    /// ran. The helper is rebuilt from it even if the command itself then failed, since what
    /// changed is the session, not the command.
    commands: Option<BTreeMap<String, CommandGroup>>,
}

impl From<Outcome> for Completed {
    /// Nothing happened to the session, which is the usual case.
    fn from(outcome: Outcome) -> Self {
        Self {
            outcome,
            commands: None,
        }
    }
}

/// A command line, resolved to one command and the arguments that follow it.
///
/// Only *where* the command runs differs between the two kinds, so that is all [`Target`] records.
/// In particular the arguments are gathered here: an internal command is one word and a bofhd
/// command is two, and there is no reason for the caller to have to know which it is counting.
struct Resolved<'a> {
    target: Target,
    args: Vec<&'a str>,
}

/// Where a resolved command runs.
enum Target {
    /// In the client itself.
    Internal(&'static InternalCommand),
    /// On the server, under this full bofhd command name, eg. `user_info`.
    Server(String),
}

impl Resolved<'_> {
    /// Run the command, wherever it runs, and report what the REPL should do next.
    ///
    /// # Errors
    ///
    /// Returns an error if the bofhd session couldn't be re-established after the server expired
    /// or restarted it, in which case the client can't carry on.
    fn run(
        self,
        bofh: &mut Bofh,
        helper: &BofhHelper,
        user: &str,
        script: &mut Option<Script>,
    ) -> Result<Completed, Box<dyn Error>> {
        match self.target {
            Target::Internal(command) => {
                Ok(internal::run(command, &self.args, bofh, helper, script).into())
            }
            Target::Server(command) => run_command(bofh, user, &command, &self.args),
        }
    }
}

/// Resolve a command line into one command and its arguments.
///
/// Both the command group and its subcommand can be abbreviated, as long as the abbreviation is
/// unambiguous. Internal commands share the namespace of the command groups and so abbreviate the
/// same way, but they take their arguments directly and have no subcommand.
///
/// # Errors
///
/// Returns a message meant for the user if the line doesn't name exactly one command.
fn resolve_command<'a>(helper: &BofhHelper, words: &'a [String]) -> Result<Resolved<'a>, String> {
    let arguments = |from: usize| words[from..].iter().map(String::as_str).collect();
    let name = helper.resolve_command_name(&words[0])?;

    if let Some(command) = internal::lookup(name) {
        return Ok(Resolved {
            target: Target::Internal(command),
            args: arguments(1),
        });
    }

    let group = helper
        .commands
        .get(name)
        .expect("Failed to retrieve command group (this shouldn't be possible)");

    let Some(word) = words.get(1) else {
        return Err(format!(
            "Incomplete command '{}', possible subcommands:\n{}",
            group.name,
            group
                .commands
                .keys()
                .cloned()
                .collect::<Vec<String>>()
                .join(", "),
        ));
    };

    let subcommand = helper.resolve_subcommand_name(&group.name, word)?;

    Ok(Resolved {
        target: Target::Server(
            group
                .commands
                .get(subcommand)
                .expect("Failed to retrieve subcommand (this shouldn't be possible)")
                .fullname
                .clone(),
        ),
        args: arguments(2),
    })
}

/// Run a bofh command on the server, transparently recovering from a restarted server or an
/// expired session.
///
/// # Errors
///
/// Returns an error if reauthentication fails, in which case the client can't carry on.
fn run_command(
    bofh: &mut Bofh,
    user: &str,
    command: &str,
    args: &[&str],
) -> Result<Completed, Box<dyn Error>> {
    let error = match bofh.run_command(command, args) {
        Ok(output) => return Ok(Outcome::Output(output).into()),
        Err(error) => error,
    };

    let commands = match error {
        // The server was restarted; let's get the valid commands again as they might have changed
        BofhError::ServerRestartedError => bofh.get_commands()?,
        // The session expired; re-authenticate the user
        BofhError::SessionExpiredError => {
            eprintln!("Session expired, please reauthenticate");
            let password = prompt_password(format!("Password for {user}: "))?;
            bofh.login(user, password)?
        }
        error => return Ok(Outcome::Failed(error.to_string()).into()),
    };

    // Re-run the command, now that the session works again
    Ok(Completed {
        outcome: match bofh.run_command(command, args) {
            Ok(output) => Outcome::Output(output),
            Err(error) => Outcome::Failed(error.to_string()),
        },
        commands: Some(commands),
    })
}

#[cfg(test)]
mod tests {
    use super::{Resolved, Target, abort_source, next_sourced_line, resolve_command};
    use crate::helper::tests::helper;
    use crate::internal::tests::source_of;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    /// The command a line names, as its full name plus the arguments left over.
    fn resolve(words: &[String]) -> Result<(String, Vec<&str>), String> {
        let Resolved { target, args } = resolve_command(&helper(), words)?;
        Ok((
            match target {
                Target::Internal(command) => command.name.to_owned(),
                Target::Server(command) => command,
            },
            args,
        ))
    }

    #[test]
    fn a_line_goes_either_to_the_client_or_to_the_server() {
        assert_eq!(resolve(&words("quit")), Ok((String::from("quit"), vec![])));
        assert_eq!(
            resolve(&words("co")),
            Ok((String::from("commands"), vec![]))
        );
        assert_eq!(
            resolve(&words("user info foo")),
            Ok((String::from("user_info"), vec!["foo"]))
        );
        assert_eq!(
            resolve(&words("g add")),
            Ok((String::from("group_add"), vec![]))
        );
    }

    /// An internal command is one word and a bofhd command is two, which is exactly why the
    /// arguments are gathered during resolution rather than counted by the caller afterwards.
    #[test]
    fn arguments_start_after_the_command_however_many_words_that_took() {
        assert_eq!(
            resolve(&words("help user info")),
            Ok((String::from("help"), vec!["user", "info"]))
        );
        assert_eq!(
            resolve(&words("user info foo bar")),
            Ok((String::from("user_info"), vec!["foo", "bar"]))
        );
    }

    #[test]
    fn a_group_on_its_own_lists_its_subcommands() {
        let Err(message) = resolve(&words("person")) else {
            panic!("a bare command group should not resolve to a command");
        };
        assert!(message.contains("find, info"), "{message}");
    }

    #[test]
    fn sourced_files_are_read_innermost_first() {
        let mut sources = vec![
            source_of("outer", false, &["user info outer"]),
            source_of("inner", false, &["user info inner"]),
        ];

        let lines = std::iter::from_fn(|| next_sourced_line(&mut sources)).collect::<Vec<_>>();
        // The inner file runs first, and the outer one carries on where it left off
        assert_eq!(
            lines,
            [
                (1, String::from("user info inner")),
                (1, String::from("user info outer")),
            ]
        );
        assert!(sources.is_empty());
    }

    #[test]
    fn a_failing_line_gives_up_on_its_own_file_only() {
        let mut sources = vec![
            source_of("outer", false, &["user info outer"]),
            source_of("inner", false, &["nonsense", "user info inner"]),
        ];

        // The line has already been taken off the file by the time it runs and fails
        let (number, _) = next_sourced_line(&mut sources).expect("Nothing to run");
        abort_source(&mut sources, number);
        assert_eq!(
            next_sourced_line(&mut sources),
            Some((1, String::from("user info outer")))
        );
    }

    #[test]
    fn ignore_errors_carries_on_past_a_failing_line() {
        let mut sources = vec![source_of("script", true, &["nonsense", "user info foo"])];

        let (number, _) = next_sourced_line(&mut sources).expect("Nothing to run");
        abort_source(&mut sources, number);
        assert_eq!(
            next_sourced_line(&mut sources),
            Some((2, String::from("user info foo")))
        );
    }

    #[test]
    fn a_typed_line_that_fails_has_no_file_to_give_up_on() {
        abort_source(&mut Vec::new(), 1);
    }
}
