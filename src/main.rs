use bofh::Bofh;
use clap::Parser;
use rpassword::prompt_password;
use rustyline::{config::Configurer, error::ReadlineError, Editor};

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
    #[clap(long, short, help_heading = "REPL behavior", default_value_t = String::from("bofh> "))]
    prompt: String,
}

fn main() {
    let args = Args::parse();

    println!("Connecting to {}\n", &args.url);
    let mut bofh = match Bofh::new(args.url) {
        Ok(bofh) => bofh,
        Err(err) => {
            println!("{}", err);
            std::process::exit(1);
        }
    };

    if let Some(motd) = &bofh.motd {
        println!("{}\n", motd);
    }

    let password = match prompt_password(format!("Password for {}: ", &args.user)) {
        Ok(password) => password,
        Err(_) => std::process::exit(0), // FIXME errors on windows?
    };

    if let Err(err) = bofh.login(&args.user, password, true) {
        println!("{}", err);
        std::process::exit(1);
    };

    let mut rl = Editor::<()>::new();

    if args.vi {
        rl.set_edit_mode(rustyline::EditMode::Vi);
        rl.set_completion_type(rustyline::CompletionType::Circular);
    }

    if rl.load_history("history.txt").is_err() {
        println!("No previous history.");
    }

    loop {
        match rl.readline(&args.prompt) {
            Ok(line) => {
                let command: Vec<&str> = line.split_whitespace().collect();
                println!("{:?}", bofh.run_command(&command));
                rl.add_history_entry(&line);
            }
            Err(ReadlineError::Interrupted) => {
                break;
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }
    println!("So long, and thanks for all the fish!");
    rl.append_history("history.txt").unwrap();
}
