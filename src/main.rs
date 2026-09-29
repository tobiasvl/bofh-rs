use bofh::{Bofh, BofhError, CommandGroup};
use clap::Parser;
mod helper;
use crate::helper::BofhHelper;
use rpassword::prompt_password;
use rustyline::{config::Configurer, error::ReadlineError, history::FileHistory, Editor};
use std::collections::BTreeMap;
use std::error::Error;
use std::process::ExitCode;

/// The rustyline editor, once it knows which commands the bofhd server offers.
type BofhEditor = Editor<BofhHelper, FileHistory>;

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
    loop {
        let line = match rl.readline(prompt) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => return Ok(()),
            Err(err) => return Err(err.into()),
        };

        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }

        let helper = rl.helper().expect("Failed to get rustyline helper");
        match resolve_command(helper, &words) {
            Ok(command) => {
                // Getting a set of commands back means the session was re-established, so the
                // helper needs to know about the server's (possibly changed) commands
                if let Some(commands) = run_command(bofh, user, &command, &words[2..])? {
                    rl.set_helper(Some(BofhHelper { commands }));
                }
            }
            Err(msg) => eprintln!("{msg}"),
        }

        if let Err(err) = rl.add_history_entry(&line) {
            eprintln!("Unable to save command to history: {err:?}");
        }
    }
}

/// Resolve a command line into the full bofhd command name, eg. `user_info`.
///
/// Both the command group and its subcommand can be abbreviated, as long as the abbreviation is
/// unambiguous.
///
/// # Errors
///
/// Returns a message meant for the user if the line doesn't name exactly one command.
fn resolve_command(helper: &BofhHelper, words: &[&str]) -> Result<String, String> {
    let groups = helper.command_candidates(words[0]);
    let [group] = groups[..] else {
        return Err(format!("Unknown command '{}'", words[0]));
    };
    let group = helper
        .commands
        .get(group)
        .expect("Failed to retrieve command group (this shouldn't be possible)");

    let Some(&word) = words.get(1) else {
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

    let subcommands = helper.subcommand_candidates(&group.name, word);
    let [subcommand] = subcommands[..] else {
        return Err(format!("Unknown command '{} {}'", words[0], word));
    };

    Ok(group
        .commands
        .get(subcommand)
        .expect("Failed to retrieve subcommand (this shouldn't be possible)")
        .fullname
        .clone())
}

/// Run a bofh command and print its result, transparently recovering from a restarted server or
/// an expired session.
///
/// Returns the refreshed set of commands if the session had to be re-established, since the
/// caller then needs to rebuild the rustyline helper.
///
/// # Errors
///
/// Returns an error if reauthentication fails, in which case the client can't carry on.
fn run_command(
    bofh: &mut Bofh,
    user: &str,
    command: &str,
    args: &[&str],
) -> Result<Option<BTreeMap<String, CommandGroup>>, Box<dyn Error>> {
    let error = match bofh.run_command(command, args) {
        Ok(msg) => {
            println!("{msg:?}");
            return Ok(None);
        }
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
        error => {
            eprintln!("{error}");
            return Ok(None);
        }
    };

    // Re-run the command, now that the session works again
    match bofh.run_command(command, args) {
        Ok(msg) => println!("{msg:?}"),
        Err(error) => eprintln!("{error}"),
    }

    Ok(Some(commands))
}
