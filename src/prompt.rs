//! Asking the user for the arguments a command line left out.
//!
//! bofhd describes most commands' arguments up front, in the table [`Bofh::get_commands`]
//! returns, and leaves a handful to describe theirs one at a time through
//! [`Bofh::call_prompt_func`]. Both end up here, so that a half-typed command is completed the
//! same way whichever kind it is.

use bofh::{ArgDefault, Argument, Bofh, BofhError, Command, Prompt, PromptMap};
use std::error::Error;
use std::fmt;
use std::io::{self, Write};

/// The three things asking for an argument needs a bofhd server for.
///
/// Narrowed to these so that the gathering below can be driven without one; everything else it
/// needs about a command is already in the table [`Bofh::get_commands`] returned.
pub(crate) trait Server {
    /// See [`Bofh::get_default_param`].
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the server can't be asked.
    fn default_param(&self, command: &str, args: &[&str]) -> Result<Option<String>, BofhError>;

    /// See [`Bofh::help_argument`].
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the server can't be asked.
    fn argument_help(&self, help_ref: &str) -> Result<String, BofhError>;

    /// See [`Bofh::call_prompt_func`].
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the server can't be asked.
    fn next_prompt(&self, command: &str, args: &[&str]) -> Result<Prompt, BofhError>;
}

impl Server for Bofh {
    fn default_param(&self, command: &str, args: &[&str]) -> Result<Option<String>, BofhError> {
        self.get_default_param(command, args)
    }

    fn argument_help(&self, help_ref: &str) -> Result<String, BofhError> {
        self.help_argument(help_ref)
    }

    fn next_prompt(&self, command: &str, args: &[&str]) -> Result<Prompt, BofhError> {
        self.call_prompt_func(command, args)
    }
}

/// Somewhere to put a question and read an answer.
///
/// An abstraction mostly so that the gathering above can be tested without a terminal, but it is
/// also what makes "there is nobody to ask" expressible: a line read from a sourced file has no
/// [`Ask`] behind it.
pub(crate) trait Ask {
    /// Ask for one value, echoing what is typed unless it is `secret`.
    ///
    /// Returns `None` if there is nothing left to read.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal can't be read.
    fn ask(&mut self, prompt: &str, secret: bool) -> io::Result<Option<String>>;
}

/// The user's terminal.
pub(crate) struct Terminal;

impl Ask for Terminal {
    /// Read the answer straight from the terminal, rather than through the line editor the REPL
    /// uses.
    ///
    /// The editor would offer command completion for something that isn't a command, and would
    /// put the answer in the history — pybofh reads prompts through readline and then has to take
    /// the value back out again, which is where it goes wrong (see `UPSTREAM-FINDINGS.md`). A
    /// value that was never added can't be removed wrongly.
    fn ask(&mut self, prompt: &str, secret: bool) -> io::Result<Option<String>> {
        if secret {
            // rpassword writes the prompt itself, and reads without echoing
            return Ok(Some(rpassword::prompt_password(prompt)?.trim().to_owned()));
        }

        print!("{prompt}");
        io::stdout().flush()?;

        let mut answer = String::new();
        if io::stdin().read_line(&mut answer)? == 0 {
            return Ok(None);
        }
        Ok(Some(answer.trim().to_owned()))
    }
}

/// Why a command's arguments couldn't be gathered.
#[derive(Debug)]
pub(crate) enum PromptError {
    /// The server couldn't be asked what to prompt for next.
    Bofh(BofhError),
    /// The terminal couldn't be read.
    Io(io::Error),
    /// The user gave up on the question.
    Cancelled,
    /// The command wants an argument, and there is nobody to ask for it.
    NoOneToAsk(String),
}

impl fmt::Display for PromptError {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Bofh(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "Unable to read the answer: {error}"),
            Self::Cancelled => write!(formatter, "Cancelled"),
            Self::NoOneToAsk(what) => write!(
                formatter,
                "{what} was not given, and a line that nobody typed is never prompted for"
            ),
        }
    }
}

impl Error for PromptError {}

/// One question to put to the user.
struct Question<'a> {
    /// What the argument is called, as the server words it.
    text: String,
    /// What an empty answer means.
    default: Option<String>,
    /// What to look the argument's help up under, when the user answers `?`.
    help_ref: Option<&'a str>,
    /// Whether the answer must not be echoed.
    secret: bool,
    /// Whether an empty answer ends the command rather than repeating the question.
    optional: bool,
    /// The rows the answer may be picked from, if the server offered any.
    map: Option<&'a PromptMap>,
}

/// Fill in the arguments `given` didn't supply, by asking for them.
///
/// `ask` is `None` for a command line nobody typed — one read from a file by `source`, say — in
/// which case a missing argument is an error rather than a question. That is what pybofh does by
/// calling `eval()` without a prompter, and it is the right way round: a file being read
/// unattended should stop rather than wait for an answer that isn't coming.
///
/// # Errors
///
/// Returns a [`PromptError`] if the server can't be asked what to prompt for, if the terminal
/// can't be read, or if there is nobody to ask.
pub(crate) fn gather_arguments(
    server: &dyn Server,
    command: &Command,
    given: &[&str],
    mut ask: Option<&mut (dyn Ask + '_)>,
) -> Result<Vec<String>, PromptError> {
    let mut args: Vec<String> = given.iter().map(|&arg| arg.to_owned()).collect();

    if command.prompt_func {
        return prompt_func(server, command, args, ask);
    }

    // Once we have started asking we keep asking, optional arguments included, since the user is
    // being taken through the whole command. An optional argument we haven't reached yet is
    // where the command line ends: skipping it would shift every later answer one place left.
    let mut asking = false;

    for (position, argument) in command.args.iter().enumerate().skip(args.len()) {
        if !asking && argument.optional {
            break;
        }
        asking = true;

        let default = default_for(server, command, argument, &args)?;
        let Some(ask) = ask.as_deref_mut() else {
            return Err(PromptError::NoOneToAsk(format!(
                "Argument {} of '{}' ({})",
                position + 1,
                command.fullname,
                argument_prompt(argument),
            )));
        };

        let question = Question {
            text: argument_prompt(argument),
            default,
            help_ref: argument.help_ref.as_deref(),
            secret: argument.is_password(),
            optional: argument.optional,
            map: None,
        };
        match ask_value(server, ask, &question)? {
            Some(answer) => args.push(answer),
            // An optional argument left blank is the user saying there is nothing more to give
            None => break,
        }
    }

    Ok(args)
}

/// Gather the arguments of a command that describes them one at a time.
///
/// Each answer is appended and the server asked again, until it says the last argument is in.
/// `bofh/proto.py:_Command._prompt_func` is the reference, which recurses where this loops.
fn prompt_func(
    server: &dyn Server,
    command: &Command,
    mut args: Vec<String>,
    mut ask: Option<&mut (dyn Ask + '_)>,
) -> Result<Vec<String>, PromptError> {
    loop {
        let so_far: Vec<&str> = args.iter().map(String::as_str).collect();
        let prompt = server
            .next_prompt(&command.fullname, &so_far)
            .map_err(PromptError::Bofh)?;

        // No question and no more arguments: the server is happy with what it has
        let Some(text) = prompt.prompt else {
            if prompt.last_arg {
                return Ok(args);
            }
            return Err(PromptError::Bofh(BofhError::CerebrumError(format!(
                "'{}' asked for an argument without saying what it is",
                command.fullname
            ))));
        };

        let Some(ask) = ask.as_deref_mut() else {
            return Err(PromptError::NoOneToAsk(format!(
                "'{}' ({text})",
                command.fullname
            )));
        };

        if let Some(map) = &prompt.map {
            println!("{}", map.header);
            for (number, option) in map.options.iter().enumerate() {
                println!("{:>4} {}", number + 1, option.label);
            }
        }

        let question = Question {
            text,
            default: prompt.default,
            help_ref: prompt.help_ref.as_deref(),
            secret: false,
            // The server decides when it has enough, so an empty answer is never the end
            optional: false,
            map: prompt.map.as_ref(),
        };
        let Some(answer) = ask_value(server, ask, &question)? else {
            continue;
        };
        args.push(answer);

        if prompt.last_arg {
            return Ok(args);
        }
    }
}

/// What an empty answer to this argument should mean.
///
/// Most defaults arrive with the command table; the rest are a promise that the server will work
/// one out when asked, given the answers so far.
fn default_for(
    server: &dyn Server,
    command: &Command,
    argument: &Argument,
    args: &[String],
) -> Result<Option<String>, PromptError> {
    match &argument.default {
        None => Ok(None),
        Some(ArgDefault::Value(value)) => Ok(Some(value.clone())),
        Some(ArgDefault::FromServer) => {
            let so_far: Vec<&str> = args.iter().map(String::as_str).collect();
            server
                .default_param(&command.fullname, &so_far)
                .map_err(PromptError::Bofh)
        }
    }
}

/// Put a question to the user until it is answered, and return what they said.
///
/// `None` means an optional question was left blank, which ends the command line.
fn ask_value(
    server: &dyn Server,
    ask: &mut dyn Ask,
    question: &Question,
) -> Result<Option<String>, PromptError> {
    let text = match &question.default {
        Some(default) => format!("{} [{default}] > ", question.text),
        None => format!("{} > ", question.text),
    };

    loop {
        let Some(answer) = ask.ask(&text, question.secret).map_err(PromptError::Io)? else {
            return Err(PromptError::Cancelled);
        };

        if answer.is_empty() {
            if let Some(default) = &question.default {
                return Ok(Some(default.clone()));
            }
            if question.optional {
                return Ok(None);
            }
            continue;
        }

        // A password is taken as typed: there is no help worth reading for one, and a user whose
        // password really is a question mark should be able to log in
        if answer == "?" && !question.secret {
            println!("{}", explain(server, question.help_ref));
            continue;
        }

        let Some(map) = question.map else {
            return Ok(Some(answer));
        };
        match select(map, &answer) {
            Ok(value) => return Ok(Some(value)),
            Err(complaint) => println!("{complaint}"),
        }
    }
}

/// What the server says an argument means, for a user who answered `?`.
fn explain(server: &dyn Server, help_ref: Option<&str>) -> String {
    match help_ref {
        Some(help_ref) => server
            .argument_help(help_ref)
            .unwrap_or_else(|error| format!("Unable to look that up: {error}")),
        None => String::from("Sorry, no help available"),
    }
}

/// Turn what was typed at a prompt offering a list into the value to send.
///
/// A number picks a row; anything else is sent as it was typed, since bofhd's maps list the
/// likely answers rather than the only acceptable ones.
fn select(map: &PromptMap, answer: &str) -> Result<String, String> {
    if is_multiple_selection(answer) {
        return Err(String::from(
            "Picking more than one item at a time isn't supported yet; choose a single number",
        ));
    }

    let Ok(number) = answer.parse::<usize>() else {
        return Ok(answer.to_owned());
    };
    let index = number
        .checked_sub(1)
        .ok_or_else(|| String::from("The list is numbered from 1"))?;

    map.options
        .get(index)
        .map(|option| option.value.clone())
        .ok_or_else(|| format!("There is no item {number} in the list"))
}

/// Whether an answer picks several rows at once, as `1-3` or `1,2,5` do.
///
/// Sending those means sending an argument that is a list, which bofhd expands into one run of
/// the command per element — the same machinery parenthesised lists use, and not implemented
/// yet. Until it is, saying so beats picking one of them silently.
fn is_multiple_selection(answer: &str) -> bool {
    let parts: Vec<&str> = answer.split([',', '-']).collect();

    parts.len() > 1
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// What to call an argument when asking for it.
fn argument_prompt(argument: &Argument) -> String {
    // `Parameter.getPrompt` answers with an empty string for an argument whose help reference
    // isn't in the server's table, so there has to be something to fall back to
    argument
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.is_empty())
        .or(argument.arg_type.as_deref())
        .or(argument.help_ref.as_deref())
        .unwrap_or("Argument")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Ask, PromptError, Server, gather_arguments, is_multiple_selection, select};
    use bofh::{ArgDefault, Argument, BofhError, Command, Prompt, PromptMap, PromptOption};
    use std::cell::RefCell;
    use std::io;

    /// An [`Ask`] that answers from a script, for driving the prompting without a terminal.
    struct Scripted {
        answers: Vec<String>,
        /// Every question that was put, so a test can check what the user would have seen.
        asked: Vec<(String, bool)>,
    }

    impl Scripted {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .rev()
                    .map(|&answer| answer.to_owned())
                    .collect(),
                asked: Vec::new(),
            }
        }

        /// The questions that were put, without the ` > ` every prompt ends in.
        fn questions(&self) -> Vec<&str> {
            self.asked
                .iter()
                .map(|(prompt, _)| prompt.trim_end_matches(" > "))
                .collect()
        }
    }

    impl Ask for Scripted {
        fn ask(&mut self, prompt: &str, secret: bool) -> io::Result<Option<String>> {
            self.asked.push((prompt.to_owned(), secret));
            Ok(self.answers.pop())
        }
    }

    /// A [`Server`] that answers from a script rather than over XML-RPC.
    #[derive(Default)]
    struct Stub {
        /// What `get_default_param` should answer, in order.
        defaults: RefCell<Vec<String>>,
        /// What `call_prompt_func` should answer, in order.
        prompts: RefCell<Vec<Prompt>>,
        /// The arguments each call was made with, to check the server is told what it needs.
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Stub {
        fn record(&self, args: &[&str]) {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|&arg| arg.to_owned()).collect());
        }
    }

    impl Server for Stub {
        fn default_param(&self, _: &str, args: &[&str]) -> Result<Option<String>, BofhError> {
            self.record(args);
            Ok(if self.defaults.borrow().is_empty() {
                None
            } else {
                Some(self.defaults.borrow_mut().remove(0))
            })
        }

        fn argument_help(&self, help_ref: &str) -> Result<String, BofhError> {
            Ok(format!("Help for {help_ref}"))
        }

        fn next_prompt(&self, _: &str, args: &[&str]) -> Result<Prompt, BofhError> {
            self.record(args);
            Ok(self.prompts.borrow_mut().remove(0))
        }
    }

    fn command(args: Vec<Argument>) -> Command {
        Command {
            fullname: String::from("user_password"),
            name: String::from("password"),
            args,
            prompt_func: false,
            help: None,
        }
    }

    fn argument(prompt: &str) -> Argument {
        Argument {
            prompt: Some(prompt.to_owned()),
            help_ref: Some(format!("{prompt}_help")),
            ..Argument::default()
        }
    }

    fn prompt(text: &str, last_arg: bool) -> Prompt {
        Prompt {
            prompt: Some(text.to_owned()),
            help_ref: None,
            last_arg,
            default: None,
            map: None,
        }
    }

    /// Gather the arguments of a command as if the user were sitting there answering.
    fn gather(
        server: &Stub,
        command: &Command,
        given: &[&str],
        answers: &[&str],
    ) -> (Result<Vec<String>, PromptError>, Scripted) {
        let mut user = Scripted::new(answers);
        let gathered = gather_arguments(server, command, given, Some(&mut user));
        (gathered, user)
    }

    #[test]
    fn a_complete_command_line_is_not_asked_about() {
        let (gathered, user) = gather(
            &Stub::default(),
            &command(vec![argument("Account"), argument("Password")]),
            &["foo", "hunter2"],
            &[],
        );
        assert_eq!(gathered.unwrap(), ["foo", "hunter2"]);
        assert!(user.asked.is_empty());
    }

    #[test]
    fn the_arguments_the_line_left_out_are_asked_for_in_order() {
        let (gathered, user) = gather(
            &Stub::default(),
            &command(vec![argument("Account"), argument("Group")]),
            &["foo"],
            &["students"],
        );
        assert_eq!(gathered.unwrap(), ["foo", "students"]);
        assert_eq!(user.questions(), ["Group"]);
    }

    /// The whole point of the argument type, and what keeps the password out of the history.
    #[test]
    fn a_password_is_asked_for_without_echoing_it() {
        let password = Argument {
            arg_type: Some(String::from("accountPassword")),
            ..argument("Password")
        };
        let (gathered, user) = gather(
            &Stub::default(),
            &command(vec![argument("Account"), password]),
            &["foo"],
            &["hunter2"],
        );
        assert_eq!(gathered.unwrap(), ["foo", "hunter2"]);
        assert!(user.asked[0].1, "the password was echoed");
    }

    #[test]
    fn an_empty_answer_takes_the_default_and_shows_it_in_the_prompt() {
        let argument = Argument {
            default: Some(ArgDefault::Value(String::from("yes"))),
            ..argument("Really")
        };
        let (gathered, user) = gather(&Stub::default(), &command(vec![argument]), &[], &[""]);
        assert_eq!(gathered.unwrap(), ["yes"]);
        assert_eq!(user.asked[0].0, "Really [yes] > ");
    }

    /// bofhd sends the integer 1 instead of a default it would rather compute on demand, given
    /// the answers so far.
    #[test]
    fn a_default_the_server_keeps_to_itself_is_asked_for() {
        let server = Stub {
            defaults: RefCell::new(vec![String::from("printer-1")]),
            ..Stub::default()
        };
        let argument = Argument {
            default: Some(ArgDefault::FromServer),
            ..argument("Printer")
        };
        let (gathered, user) = gather(
            &server,
            &command(vec![self::argument("Account"), argument]),
            &["foo"],
            &[""],
        );
        assert_eq!(gathered.unwrap(), ["foo", "printer-1"]);
        assert_eq!(user.asked[0].0, "Printer [printer-1] > ");
        // The server works the default out from the arguments already answered, and indexes the
        // parameter it is being asked about by how many of them there are
        assert_eq!(server.calls.into_inner(), [["foo"]]);
    }

    #[test]
    fn a_required_argument_is_asked_for_until_it_is_answered() {
        let (gathered, user) = gather(
            &Stub::default(),
            &command(vec![argument("Account")]),
            &[],
            &["", "", "foo"],
        );
        assert_eq!(gathered.unwrap(), ["foo"]);
        assert_eq!(user.asked.len(), 3);
    }

    #[test]
    fn a_question_mark_asks_the_server_what_the_argument_means() {
        let (gathered, user) = gather(
            &Stub::default(),
            &command(vec![argument("Account")]),
            &[],
            &["?", "foo"],
        );
        assert_eq!(gathered.unwrap(), ["foo"]);
        assert_eq!(user.asked.len(), 2);
    }

    /// As in jbofh. Skipping it and asking for the next one instead would shift the answer into
    /// the wrong position, which is what pybofh does (see `UPSTREAM-FINDINGS.md`).
    #[test]
    fn a_trailing_optional_argument_is_not_asked_for_on_its_own() {
        let optional = Argument {
            optional: true,
            ..argument("Reason")
        };
        let (gathered, user) = gather(
            &Stub::default(),
            &command(vec![argument("Account"), optional]),
            &["foo"],
            &[],
        );
        assert_eq!(gathered.unwrap(), ["foo"]);
        assert!(user.asked.is_empty());
    }

    /// Once we have started taking the user through a command we take them through all of it,
    /// but an optional argument they leave blank ends the line.
    #[test]
    fn an_optional_argument_is_asked_for_once_we_are_asking_and_may_be_left_blank() {
        let optional = Argument {
            optional: true,
            ..argument("Reason")
        };
        let (gathered, user) = gather(
            &Stub::default(),
            &command(vec![argument("Account"), optional]),
            &[],
            &["foo", ""],
        );
        assert_eq!(gathered.unwrap(), ["foo"]);
        assert_eq!(user.questions(), ["Account", "Reason"]);
    }

    /// A file being read unattended should stop rather than wait for an answer nobody will give.
    #[test]
    fn a_line_nobody_typed_is_never_prompted_for() {
        let command = command(vec![argument("Account")]);
        let error = gather_arguments(&Stub::default(), &command, &[], None).unwrap_err();
        assert!(
            matches!(error, PromptError::NoOneToAsk(_)),
            "{error} should have been a refusal to prompt"
        );
    }

    #[test]
    fn a_prompt_func_command_asks_the_server_what_to_ask_next() {
        let server = Stub {
            prompts: RefCell::new(vec![
                prompt("Account", false),
                prompt("Group", true),
            ]),
            ..Stub::default()
        };
        let command = Command {
            args: vec![],
            prompt_func: true,
            ..command(vec![])
        };
        let (gathered, user) = gather(&server, &command, &[], &["foo", "students"]);

        assert_eq!(gathered.unwrap(), ["foo", "students"]);
        assert_eq!(user.questions(), ["Account", "Group"]);
        // Each answer is handed back to the server, which is how it knows what to ask next
        assert_eq!(
            server.calls.into_inner(),
            [vec![] as Vec<String>, vec![String::from("foo")]]
        );
    }

    /// The server having nothing left to ask is how a prompt func says the command is complete,
    /// even when the client had already typed every argument.
    #[test]
    fn a_prompt_func_that_has_nothing_to_ask_runs_the_command_as_it_is() {
        let server = Stub {
            prompts: RefCell::new(vec![Prompt {
                prompt: None,
                last_arg: true,
                ..prompt("", true)
            }]),
            ..Stub::default()
        };
        let command = Command {
            prompt_func: true,
            ..command(vec![])
        };
        let (gathered, user) = gather(&server, &command, &["foo"], &[]);
        assert_eq!(gathered.unwrap(), ["foo"]);
        assert!(user.asked.is_empty());
    }

    #[test]
    fn a_prompt_func_answer_picked_from_a_list_is_sent_as_the_value_behind_it() {
        let server = Stub {
            prompts: RefCell::new(vec![Prompt {
                map: Some(map()),
                ..prompt("Choose template #", true)
            }]),
            ..Stub::default()
        };
        let command = Command {
            prompt_func: true,
            ..command(vec![])
        };
        let (gathered, _) = gather(&server, &command, &[], &["2"]);
        assert_eq!(gathered.unwrap(), ["value2"]);
    }

    fn map() -> PromptMap {
        PromptMap {
            header: String::from("Alternatives"),
            options: ["first", "second", "third"]
                .iter()
                .enumerate()
                .map(|(index, label)| PromptOption {
                    value: format!("value{}", index + 1),
                    label: (*label).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_number_picks_the_value_the_server_mapped_it_to() {
        assert_eq!(select(&map(), "1"), Ok(String::from("value1")));
        assert_eq!(select(&map(), "3"), Ok(String::from("value3")));
    }

    /// A map lists the likely answers, not the only ones, so anything else goes as it was typed.
    #[test]
    fn anything_that_is_not_a_number_is_sent_as_typed() {
        assert_eq!(select(&map(), "somewhere-else"), Ok(String::from("somewhere-else")));
        assert_eq!(select(&map(), "2nd"), Ok(String::from("2nd")));
    }

    #[test]
    fn a_number_outside_the_list_is_refused_rather_than_clamped() {
        assert!(select(&map(), "0").is_err());
        assert!(select(&map(), "4").is_err());
    }

    #[test]
    fn picking_several_rows_says_it_is_not_supported_yet() {
        assert!(is_multiple_selection("1-3"));
        assert!(is_multiple_selection("1,2,5"));
        assert!(!is_multiple_selection("1"));
        assert!(!is_multiple_selection("some-printer"));
        assert!(!is_multiple_selection("1-"));
        assert!(select(&map(), "1-3").is_err());
    }
}
