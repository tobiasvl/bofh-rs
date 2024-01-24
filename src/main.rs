use bofh::Bofh;
use clap::Parser;
mod helper;
use crate::helper::BofhHelper;
use rpassword::prompt_password;
use rustyline::{config::Configurer, error::ReadlineError, Editor};
use std::process::ExitCode;

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

    /// authenticate as USER
    #[clap(long, short, help_heading = "Connection settings", default_value_t = whoami::username())]
    user: String,

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
    /// https://catonmat.net/ftp/bash-vi-editing-mode-cheat-sheet.pdf)
    #[clap(long, help_heading = "REPL behavior", alias = "vim")]
    vi: bool,

    /// use a custom prompt
    #[clap(long, short, help_heading = "Prompt", default_value_t = String::from("bofh> "))]
    prompt: String,
}

fn main() -> ExitCode {
    let args = Args::parse();

    println!("Connecting to {}\n", &args.url);
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

    let Ok(password) = prompt_password(format!("Password for {}: ", &args.user)) else {
        return ExitCode::FAILURE; // FIXME errors on windows?
    };

    let commands = match bofh.login(&args.user, password) {
        Ok(commands) => commands,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };

    let mut rl = Editor::new().expect("Failed to connect to terminal/TTY");
    rl.set_helper(Some(BofhHelper {
        commands: &commands,
    }));

    if args.vi {
        rl.set_edit_mode(rustyline::EditMode::Vi);
        rl.set_completion_type(rustyline::CompletionType::Circular);
    } else {
        rl.set_completion_type(rustyline::CompletionType::List);
    }

    if rl.load_history("history.txt").is_err() {
        println!("No previous history.");
    }

    loop {
        match rl.readline(&args.prompt) {
            Ok(line) => {
                let command: Vec<&str> = line.split_whitespace().collect();
                let helper = rl.helper().expect("Failed to get rustyline helper");
                if !command.is_empty() {
                    let candidates: Vec<&str> = helper.command_candidates(command[0]);
                    if candidates.len() == 1 {
                        let command_group = commands.get(candidates[0]).expect(
                            "Failed to retrieve command group (this shouldn't be possible)",
                        );
                        if command.len() > 1 {
                            let candidates =
                                helper.subcommand_candidates(candidates[0], command[1]);
                            if candidates.len() == 1 {
                                let subcommand = command_group.commands.get(candidates[0]).expect(
                                    "Failed to retrieve subcommand (this shouldn't be possible)",
                                );
                                match bofh.run_command(subcommand.fullname.as_str(), &command[2..])
                                {
                                    Ok(msg) => println!("{msg:?}"),
                                    Err(msg) => eprintln!("{msg}"),
                                }
                            } else {
                                eprintln!("Unknown command '{} {}'", command[0], command[1]);
                            }
                        } else {
                            eprintln!(
                                "Incomplete command '{}', possible subcommands:\n{}",
                                command_group.name,
                                command_group
                                    .commands
                                    .keys()
                                    .cloned()
                                    .collect::<Vec<String>>()
                                    .join(", "),
                            );
                        }
                    } else {
                        eprintln!("Unknown command '{}'", command[0]);
                    }
                }
                if let Err(err) = rl.add_history_entry(&line) {
                    eprintln!("Unable to save command to history: {err:?}");
                }
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                break;
            }
            Err(err) => {
                eprintln!("Error: {err:?}");
                return ExitCode::FAILURE;
            }
        }
    }
    println!("Go, then - there are other worlds than these.");
    rl.append_history("history.txt")
        .expect("Unable to write history to history.txt");

    ExitCode::SUCCESS
}
