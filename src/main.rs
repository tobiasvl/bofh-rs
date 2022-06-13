use bofh::Bofh;
use clap::Parser;
use rpassword::prompt_password;
use rustyline::{error::ReadlineError, Editor};

/// The Cerebrum Bofh client
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    ///// Run command and exit
    //#[clap(long, required = false)]
    //cmd: String,
    /// connect to bofhd server at URL
    #[clap(long, default_value_t = String::from("https://cerebrum-uio-test.uio.no:8000/"))]
    url: String,

    /// authenticate as USER
    #[clap(long, short, default_value_t = whoami::username())]
    user: String,

    /// skip certificate hostname validation
    #[clap(long)]
    insecure: bool,

    /// set connection timeout to N seconds
    #[clap(long, default_value_t = 0, value_name = "N")]
    timeout: u8,

    /// use vi command mode (cheatsheet:
    /// https://catonmat.net/ftp/bash-vi-editing-mode-cheat-sheet.pdf)
    #[clap(long, alias = "vim")]
    vi: bool,

    /// use a custom prompt
    #[clap(long, short, default_value_t = String::from("bofh> "))]
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
        Err(_) => std::process::exit(0),
    };

    if let Err(err) = bofh.login(&args.user, password, true) {
        println!("{}", err);
        std::process::exit(1);
    };

    let mut rl = Editor::<()>::new();

    //if args.vi {
    //    rl.set_edit_mode(rustyline::EditMode::Vi)
    //}
    if rl.load_history("history.txt").is_err() {
        println!("No previous history.");
    }
    loop {
        match rl.readline(&args.prompt) {
            Ok(line) => {
                println!("{:?}", bofh.run_command(&line, vec![]));
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
