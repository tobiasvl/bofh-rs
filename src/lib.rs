pub mod formatting;

pub use crate::formatting::FormatSuggestion;

use dxr::{Fault, MethodCall, Value};
use dxr_client::utils::{request_to_body, response_to_result};
use std::borrow::Cow;
use std::collections::BTreeMap;
use thiserror::Error;

/// The name this client identifies itself as to a bofhd server.
pub const CLIENT_ID: &str = "bofh-rs";

/// The version this client identifies itself as to a bofhd server.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How much of a response to read before giving up on it.
///
/// Only a sanity limit: the largest thing bofhd answers with is a listing, and the client holds
/// the whole response in memory anyway. The HTTP client's own default is lower than some
/// listings can plausibly get.
const MAX_RESPONSE_SIZE: u64 = 64 * 1024 * 1024;

/// Errors that might occur when communicating with a bofhd server.
#[derive(Error, Debug)]
pub enum BofhError {
    /// The request could not be sent, or the response could not be read back.
    #[error("{0}")]
    TransportError(#[from] ureq::Error),
    /// Error occurring in the XML-RPC protocol.
    #[error("{0}")]
    XmlRpcError(#[from] dxr::Error),
    /// Attempted to run authenticated command before session was established.
    #[error("Attempted to run authenticated command before session was established")]
    NoSessionError,
    /// Error in a Cerebrum/bofhd command.
    #[error("{0}")]
    CerebrumError(String),
    /// Server restarted in the middle of the session.
    #[error("Server restarted")]
    ServerRestartedError,
    /// Session has expired, and the client must re-authenticate.
    #[error("Session expired")]
    SessionExpiredError,
    /// The bofhd server reported that a command was not implemented.
    #[error("{0}")]
    NotImplementedError(String),
    /// XML-RPC request reported a fault.
    #[error("{0}")]
    Fault(String),
}

/// A bofhd command.
#[derive(Debug, Clone)]
pub struct Command {
    /// The actual, full bofhd command name, which can be supplied to [`Bofh::run_command`].
    pub fullname: String,
    /// The name of this subcommand.
    pub name: String,
    /// Valid arguments to this command.
    ///
    /// Empty when [`Self::prompt_func`] is set: the server describes those arguments one at a
    /// time as they are asked for, rather than up front.
    pub args: Vec<Argument>,
    /// Whether the server asks for this command's arguments itself, through
    /// [`Bofh::call_prompt_func`], instead of advertising them in [`Self::args`].
    pub prompt_func: bool,
    /// Help text for command, supplied by the server.
    pub help: Option<String>,
}

/// An argument for a bofhd command.
#[derive(Debug, Default, Clone)]
pub struct Argument {
    /// Whether this argument is optional or required.
    pub optional: bool,
    /// Whether this argument can be repeated.
    pub repeat: bool,
    /// The default value for this argument, if it has one.
    pub default: Option<ArgDefault>,
    /// The argument type.
    pub arg_type: Option<String>,
    /// The help reference that should be used for this argument, if the client requests help.
    pub help_ref: Option<String>,
    /// The prompt that should be used for this argument, if it's not supplied.
    pub prompt: Option<String>,
}

/// The argument type that means the client must not echo what the user types.
///
/// Of the 54 argument types stock Cerebrum declares, it is the only one that carries any
/// behaviour, and every client implements just this one (`readlineui.py:198`, `JBofh.java:505`).
/// So the type stays an open [`String`] rather than an enum that would have to guess at the rest,
/// including whatever a site-local bofhd module adds.
const PASSWORD_ARG_TYPE: &str = "accountPassword";

impl Argument {
    /// Whether this argument is a password, and so must not be echoed or kept in the history.
    #[must_use]
    pub fn is_password(&self) -> bool {
        self.arg_type.as_deref() == Some(PASSWORD_ARG_TYPE)
    }
}

/// The default value for an [`Argument`], which the server either sends or promises to compute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgDefault {
    /// The default itself, sent along with the command table.
    Value(String),
    /// `Parameter.get_struct` sends the *integer* `1` in place of any default that isn't text,
    /// meaning the client should ask [`Bofh::get_default_param`] for the real one
    /// (`cmd_param.py:79-87`).
    FromServer,
}

/// What a command whose arguments are driven by the server wants asked next.
///
/// Returned by [`Bofh::call_prompt_func`]. The client asks the question, appends the answer to
/// the arguments it already has, and calls again until [`Self::last_arg`] says to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The question to put to the user. `None` together with [`Self::last_arg`] means the server
    /// has nothing left to ask and the command should be run with the arguments gathered so far.
    pub prompt: Option<String>,
    /// What to pass to [`Bofh::help_argument`] if the user asks what the argument means.
    pub help_ref: Option<String>,
    /// Whether the answer to this question is the command's last argument.
    pub last_arg: bool,
    /// The value to use if the user answers with an empty line.
    pub default: Option<String>,
    /// A list to pick the answer from, if the server offered one.
    pub map: Option<PromptMap>,
}

/// A list of answers a [`Prompt`] offers to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMap {
    /// The first row of the server's map, which labels the ones below it.
    pub header: String,
    /// The rows the user picks from, in the order the server sent them.
    pub options: Vec<PromptOption>,
    /// Whether the list is only there to be read.
    ///
    /// The server's `raw` flag. When it is set, the rows are shown to the user and the answer is
    /// sent on exactly as it was typed, because the server parses the selection itself — which is
    /// how it accepts things no single row stands for, such as `1,3` or `3-15`. Leave
    /// [`Self::options`] alone in that case: their [`PromptOption::value`]s are not what to send.
    pub raw: bool,
}

/// One row of a [`PromptMap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptOption {
    /// What to send back to the server when this row is picked, unless the map is
    /// [`PromptMap::raw`].
    pub value: String,
    /// What to show the user, with the row's format suggestion already applied.
    pub label: String,
}

/// Decode bofhd's encoding of values that XML-RPC can't express by itself.
///
/// XML-RPC has no nil type, so bofhd sends `None` as the string `":None"`, and escapes any string
/// that really does start with a colon by prefixing it with another one.
///
/// Only the values of a struct are decoded, not its keys, which is also what pybofh does; bofhd
/// only ever uses plain identifiers as keys.
fn wash_value(value: Value) -> Value {
    match value {
        Value::String(string) => {
            if string == ":None" {
                Value::Nil
            } else if let Some(string) = string.strip_prefix(':') {
                Value::String(string.to_owned())
            } else {
                Value::String(string)
            }
        }
        Value::Array(array) => Value::Array(array.into_iter().map(wash_value).collect()),
        Value::Struct(strct) => Value::Struct(
            strct
                .into_iter()
                .map(|(key, value)| (key, wash_value(value)))
                .collect(),
        ),
        value => value,
    }
}

/// Read a struct, or nothing if the value is some other kind.
///
/// [`Value`] is a plain enum with no accessors of its own, so the three shapes bofhd's responses
/// are made of get one each here rather than a `match` at every use.
pub(crate) fn as_struct(value: &Value) -> Option<&BTreeMap<String, Value>> {
    match value {
        Value::Struct(strct) => Some(strct),
        _ => None,
    }
}

/// Read an array, or nothing if the value is some other kind.
pub(crate) fn as_array(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Array(array) => Some(array),
        _ => None,
    }
}

/// Read a string, or nothing if the value is some other kind.
pub(crate) fn as_str(value: &Value) -> Option<&str> {
    match value {
        Value::String(string) => Some(string),
        _ => None,
    }
}

/// Read one field of a response entry, or nothing if the entry hasn't got it.
pub(crate) fn field<'a>(entry: &'a Value, name: &str) -> Option<&'a Value> {
    as_struct(entry)?.get(name)
}

/// Read a flag the way bofhd sends them, which is not always as a boolean.
///
/// `cmd_param` hands some flags to the XML-RPC layer as the *text* Python's `str()` makes of a
/// bool, so `"True"` has to mean the same as `true`; see `BOFH-BUGS.md`. A missing key is false,
/// which is how bofhd omits the flags it hasn't set.
fn as_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Boolean(value)) => *value,
        Some(Value::String(value)) => value == "True",
        Some(Value::Integer(value)) => *value != 0,
        _ => false,
    }
}

/// Read a value that the server may have left out or sent as nil.
fn as_optional_string(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Nil) => None,
        Some(value) => Some(formatting::render_value(value)),
    }
}

/// Escape an argument that starts with a colon, which bofhd would otherwise strip.
///
/// This is the inverse of the escaping [`wash_value`] undoes.
fn escape_argument(argument: &str) -> Cow<'_, str> {
    if argument.starts_with(':') {
        Cow::Owned(format!(":{argument}"))
    } else {
        Cow::Borrowed(argument)
    }
}

/// Encode one argument of a bofhd call.
///
/// Every argument goes on the wire as a string: bofhd's own clients send nothing else, and
/// `xmlutils.xmlrpc_to_native` turns the text back into whatever the command wants.
fn escaped_argument(argument: &str) -> Value {
    Value::String(escape_argument(argument).into_owned())
}

/// Read what the server sent back, whether that turned out to be an answer or a fault.
///
/// Kept apart from the sending so that the whole of the decoding can be tested without a server:
/// a fault arrives as an ordinary successful response, and telling the two apart is the server's
/// shape rather than HTTP's.
fn read_response(body: &str) -> Result<Value, BofhError> {
    match response_to_result(body)? {
        Ok(response) => Ok(wash_value(response.value)),
        Err(fault) => Err(bofhd_fault(&fault.fault)),
    }
}

/// Work out which [`BofhError`] a fault from bofhd stands for.
///
/// `protocol._format_xmlrpc_fault` spells faults as `"<ErrType>:<message>"`. Only three error
/// types keep their own name; everything else a command can raise, permission denials included,
/// is flattened to `CerebrumError` before it leaves the server, so the fault string is as much
/// as the client can know. The numeric code says only whether the server blames Cerebrum or
/// itself, which it has already said in the type, so it is not read.
fn bofhd_fault(fault: &Fault) -> BofhError {
    let Some(bofhd_error) = fault
        .string()
        .strip_prefix("Cerebrum.modules.bofhd.errors.")
    else {
        return match fault.string().strip_prefix("NotImplementedError:") {
            Some(not_implemented) => BofhError::NotImplementedError(not_implemented.to_owned()),
            None => BofhError::Fault(fault.string().to_owned()),
        };
    };

    if let Some(cerebrum_error) = bofhd_error.strip_prefix("CerebrumError:") {
        BofhError::CerebrumError(cerebrum_error.to_owned())
    } else if bofhd_error.starts_with("ServerRestartedError:") {
        BofhError::ServerRestartedError
    } else if bofhd_error.starts_with("SessionExpiredError:") {
        BofhError::SessionExpiredError
    } else {
        BofhError::Fault(bofhd_error.to_owned())
    }
}

/// Read the struct that `call_prompt_func` answers with.
///
/// The map is flattened here rather than in the client: applying a row's printf format is the
/// same job the library already does for format suggestions, and it is what keeps
/// [`Value`] out of the client.
fn parse_prompt(response: &Value) -> Prompt {
    let empty = BTreeMap::new();
    let strct = as_struct(response).unwrap_or(&empty);

    Prompt {
        prompt: as_optional_string(strct.get("prompt")),
        help_ref: as_optional_string(strct.get("help_ref")),
        last_arg: as_bool(strct.get("last_arg")),
        default: as_optional_string(strct.get("default")),
        map: match strct.get("map") {
            Some(Value::Array(rows)) => parse_prompt_map(rows, as_bool(strct.get("raw"))),
            _ => None,
        },
    }
}

/// Flatten the rows of a prompt's map, the first of which is the header for the rest.
fn parse_prompt_map(rows: &[Value], raw: bool) -> Option<PromptMap> {
    let (header, rows) = rows.split_first()?;

    Some(PromptMap {
        header: render_map_row(header).unwrap_or_default(),
        // A row that can't be read keeps its place in the list, so that the numbers the user
        // picks from still line up with the rows the server sent
        options: rows
            .iter()
            .map(|row| PromptOption {
                value: as_optional_string(as_array(row).and_then(|row| row.get(1)))
                    .unwrap_or_default(),
                label: render_map_row(row).unwrap_or_default(),
            })
            .collect(),
        raw,
    })
}

/// Render one row of a map, which pairs a printf format with the values to interpolate into it.
fn render_map_row(row: &Value) -> Option<String> {
    let (format, values) = as_array(as_array(row)?.first()?)?.split_first()?;
    let format = as_str(format)?;

    if !format.contains('%') {
        return Some(format.to_owned());
    }
    let values: Vec<String> = values.iter().map(formatting::render_value).collect();
    // A format we can't reproduce is shown as it came rather than guessed at, as elsewhere
    Some(formatting::apply_format(format, &values).unwrap_or_else(|| format.to_owned()))
}

/// A bofhd command group, ie. semantically linked command prefixes.
#[derive(Debug, Clone)]
pub struct CommandGroup {
    /// The common prefix of the grouped commands.
    pub name: String,
    /// The command group's subcommands.
    pub commands: BTreeMap<String, Command>,
}

/// The bofh client communicating with the bofhd server.
pub struct Bofh {
    /// The URL to the bofhd server.
    pub url: String,
    /// The Message Of The Day provided by the bofhd server after connection.
    pub motd: Option<String>,
    session: Option<String>,
    /// Output format suggestion cache.
    format_suggestions: BTreeMap<String, Option<FormatSuggestion>>,
    /// The HTTP client every call goes through, which also holds the connection pool.
    ///
    /// This is where `--timeout`, `--cert` and `--insecure` will be configured once they do
    /// anything; see `TODO-PARITY.md`.
    agent: ureq::Agent,
}

impl Bofh {
    /// Creates a new connection to a bofhd server, and tests the connection by requesting the server's Message of the Day (which is stored in [`Self::motd`]).
    ///
    /// # Errors
    ///
    /// Will return a [`BofhError`] if the connection to the bofhd server fails, or it doesn't respond to the [`Self::get_motd`] command.
    pub fn new(url: String) -> Result<Self, BofhError> {
        let mut bofh = Self {
            url,
            session: None,
            motd: None,
            format_suggestions: BTreeMap::new(),
            // A fault is sent with an ordinary 200, but a server that answers an error status
            // with a fault body should still be read as a fault rather than as a status
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .user_agent(format!("{CLIENT_ID}/{CLIENT_VERSION}"))
                    .build(),
            ),
        };
        // bofhd returns an empty string when the site has no MOTD file and nothing to nag about
        bofh.motd = Some(bofh.get_motd(CLIENT_ID, CLIENT_VERSION)?).filter(|motd| !motd.is_empty());
        Ok(bofh)
    }

    fn run_request(&self, call: &MethodCall) -> Result<Value, BofhError> {
        let response = self
            .agent
            .post(&self.url)
            .content_type("text/xml")
            .send(request_to_body(call)?)?;

        // A fault is a perfectly ordinary 200 from bofhd, and the status is of no further
        // interest once there is a body to read: whatever the server says went wrong, it says
        // in the fault, not in the status line
        let mut body = response.into_body();
        let body = body
            .with_config()
            .limit(MAX_RESPONSE_SIZE)
            .read_to_string()?;

        read_response(&body)
    }

    fn run_raw_command(&self, command: &str, args: &[&str]) -> Result<Value, BofhError> {
        self.run_request(&MethodCall {
            name: Cow::Borrowed(command),
            params: args.iter().map(|arg| escaped_argument(arg)).collect(),
        })
    }

    fn run_raw_sess_command(&self, command: &str, args: &[&str]) -> Result<Value, BofhError> {
        if let Some(session) = &self.session {
            // The session identifier comes from the server, so it's sent back unescaped
            let mut params = vec![Value::String(session.clone())];
            params.extend(args.iter().map(|arg| escaped_argument(arg)));
            self.run_request(&MethodCall {
                name: Cow::Borrowed(command),
                params,
            })
        } else {
            Err(BofhError::NoSessionError)
        }
    }

    // XXX: There are only a handful of bofhd commands:
    // motd = get_motd(client_name, version)
    // session = login(user, pass)
    // logout(session)
    // get_commands(session) -- see _init_commands
    // help(session) -- general help
    // help(session, "arg_help", ref) -- help on arg type,
    //                                   ref found in arg['help_ref']
    // help(session, group) -- help on group
    // help(session, group, cmd) -- help on command
    // run_command(session, command, args)  # command = group_cmd
    // call_prompt_func(session, command, args) =>
    //   {prompt: string, help_ref: key, last_arg: bool, default: value,
    //    map: [[["Header", None], value], [[format, *args], value], ...],
    //    raw: bool}
    // get_default_param(session, command, args)
    // get_format_suggestion(command)

    /// Returns the commands available to the current user on the bofh server.
    ///
    /// This also discards any cached format suggestions, since the usual reason to call it again
    /// is that the server restarted and its commands may have changed.
    ///
    /// # Errors
    ///
    /// Will return a [`BofhError::NoSessionError`] if the user isn't logged in (with [`Self::login`]).
    ///
    /// # Panics
    ///
    /// Should normally not panic, but it might happen if the bofhd server returns malformed data.
    pub fn get_commands(&mut self) -> Result<BTreeMap<String, CommandGroup>, BofhError> {
        let response = self.run_raw_sess_command("get_commands", &[])?;

        // Discard existing format suggestion cache, in case this is a server restart and commands
        // have changed on the server.
        self.format_suggestions.clear();

        let mut commands = BTreeMap::<String, CommandGroup>::new();
        for (cmd, array) in as_struct(&response).unwrap() {
            let array = as_array(array).unwrap();
            let name = as_array(&array[0]).unwrap();
            let cmd_group = as_str(&name[0]).unwrap();
            if !commands.contains_key(cmd_group) {
                commands.insert(
                    cmd_group.into(),
                    CommandGroup {
                        name: cmd_group.into(),
                        commands: BTreeMap::new(),
                    },
                );
            }
            let cmd_name = as_str(&name[1]).unwrap();
            commands.get_mut(cmd_group).unwrap().commands.insert(
                cmd_name.into(),
                Command {
                    name: cmd_name.into(),
                    fullname: cmd.into(),
                    // A command with no parameters and no prompt func is sent as a one-element
                    // array, so the arguments have to be asked for rather than indexed
                    // (`cmd_param.py:303-309`). Twenty commands in the Cerebrum tree are like
                    // that, `misc affiliations` among them
                    args: match array.get(1) {
                        Some(Value::Array(array)) => {
                            let mut vector = vec![];
                            for strct in array {
                                let strct = as_struct(strct).unwrap();
                                vector.push(Argument {
                                    optional: as_bool(strct.get("optional")),
                                    repeat: as_bool(strct.get("repeat")),
                                    default: match strct.get("default") {
                                        None | Some(Value::Nil) => None,
                                        // Anything that isn't text is the sentinel, whatever
                                        // number the server chose to spell it with
                                        Some(Value::String(value)) => {
                                            Some(ArgDefault::Value(value.clone()))
                                        }
                                        Some(_) => Some(ArgDefault::FromServer),
                                    },
                                    arg_type: as_optional_string(strct.get("type")),
                                    help_ref: as_optional_string(strct.get("help_ref")),
                                    prompt: as_optional_string(strct.get("prompt")),
                                });
                            }
                            vector
                        }
                        // The literal string `"prompt_func"`: the server will ask for the
                        // arguments itself, so there are none to list here
                        _ => vec![],
                    },
                    prompt_func: matches!(array.get(1), Some(Value::String(_))),
                    help: None,
                },
            );
        }
        Ok(commands)
    }

    /// Run a bofh command on the bofhd server.
    ///
    /// Note that this function actually runs the raw bofhd command `run_command bofh_command`, and can't be used to run
    /// arbitrary raw bofhd commands. Those are all exposed through separate functions, such as [`Self::login`] and [`Self::get_motd`].
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason.
    ///
    /// If the bofhd session has expired and this function returns a [`BofhError::SessionExpiredError`], the client might want to reauthenticate using [`Self::login`] and then retry the command.
    pub fn run_command(&mut self, command: &str, args: &[&str]) -> Result<String, BofhError> {
        let command_args: Vec<&str> = {
            let mut command_args = vec![command];
            for &arg in args {
                command_args.push(arg);
            }
            command_args
        };
        let response = self.run_raw_sess_command("run_command", &command_args)?;

        Ok(match self.cached_format_suggestion(command) {
            Some(suggestion) => suggestion.format(&response),
            None => formatting::render_plain(&response),
        })
    }

    /// Ask the server what to prompt for next, for a command that gathers its own arguments.
    ///
    /// A command whose [`Command::prompt_func`] is set doesn't advertise its arguments; instead
    /// the client asks one question at a time, appending each answer to `args` and calling again
    /// until the returned [`Prompt::last_arg`] says to stop.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason, including a
    /// [`BofhError::CerebrumError`] if the command has no prompt function, or if the answers so
    /// far don't make sense to it.
    pub fn call_prompt_func(&self, command: &str, args: &[&str]) -> Result<Prompt, BofhError> {
        let mut call = vec![command];
        call.extend_from_slice(args);
        Ok(parse_prompt(&self.run_raw_sess_command(
            "call_prompt_func",
            &call,
        )?))
    }

    /// Ask the server to compute the default value for the next argument of a command.
    ///
    /// Only needed for an [`Argument`] whose default is [`ArgDefault::FromServer`]; `args` is
    /// what the user has answered so far, which is also how the server knows which argument is
    /// being asked about (`handler.py:644` indexes the parameter list by their number).
    ///
    /// Returns `None` when the server has no default to offer, which it says with an empty
    /// string.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason.
    pub fn get_default_param(
        &self,
        command: &str,
        args: &[&str],
    ) -> Result<Option<String>, BofhError> {
        let mut call = vec![command];
        call.extend_from_slice(args);
        Ok(as_optional_string(Some(
            &self.run_raw_sess_command("get_default_param", &call)?,
        ))
        .filter(|default| !default.is_empty()))
    }

    /// Return the format suggestion for a command, fetching it the first time it's needed.
    ///
    /// A command that has no (or a malformed) suggestion gets the format suggestion `None`.
    fn cached_format_suggestion(&mut self, command: &str) -> Option<&FormatSuggestion> {
        if !self.format_suggestions.contains_key(command)
            && let Ok(suggestion) = self.get_format_suggestion(command)
        {
            self.format_suggestions
                .insert(command.to_owned(), suggestion);
        }
        self.format_suggestions.get(command)?.as_ref()
    }

    /// Ask the bofhd server for a format suggestion for a specific command's output.
    ///
    /// Returns `None` if the command has no format suggestion, in which case it returns a string
    /// that the server has already formatted.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason.
    pub fn get_format_suggestion(
        &self,
        command: &str,
    ) -> Result<Option<FormatSuggestion>, BofhError> {
        Ok(FormatSuggestion::parse(
            &self.run_raw_command("get_format_suggestion", &[command])?,
        ))
    }

    /// Authenticate with the bofhd server and set up a session.
    ///
    /// Returns the commands available to the authenticated user. These commands can be re-requested later with [`Self::get_commands`] without logging in again, for example when
    /// handling a [`BofhError::ServerRestartedError`] (which might occur if a new version of bofhd is deployed, with new commands).
    ///
    /// Note that this consumes `password` to discourage user-facing clients from holding onto the user's password.
    /// If the user needs to reauthenticate (if [`Self::run_command`] later returns a [`BofhError::SessionExpiredError`], for example), please prompt the user for the password again.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if logging in or getting the commands from the server fail for some reason.
    ///
    /// # Panics
    ///
    /// Will normally never panic, unless the session identifier returned by the bofhd server is in an invalid format.
    #[allow(clippy::needless_pass_by_value)]
    pub fn login(
        &mut self,
        username: &str,
        password: String,
    ) -> Result<BTreeMap<String, CommandGroup>, BofhError> {
        if self.session.is_some() {
            let _ = self.run_raw_sess_command("logout", &[]);
        }
        self.session = Some(
            as_str(&self.run_raw_command("login", &[username, &password])?)
                .expect("Invalid bofhd session identifier")
                .to_owned(),
        );
        self.get_commands()
    }

    /// Ask the bofhd server for its general help text: a blurb, then one line per command group.
    ///
    /// Only the groups the user is allowed to use are listed.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason.
    pub fn help(&self) -> Result<String, BofhError> {
        self.help_request(&[])
    }

    /// Ask the bofhd server for the help text for a command group, eg. `user`.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason, including a
    /// [`BofhError::CerebrumError`] if the server doesn't know the group.
    pub fn help_group(&self, group: &str) -> Result<String, BofhError> {
        self.help_request(&[group])
    }

    /// Ask the bofhd server for the help text for a single command, eg. `user`, `info`.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason, including a
    /// [`BofhError::CerebrumError`] if the server doesn't know the command.
    pub fn help_command(&self, group: &str, command: &str) -> Result<String, BofhError> {
        self.help_request(&[group, command])
    }

    /// Ask the bofhd server what an argument means, given an [`Argument::help_ref`].
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason, including a
    /// [`BofhError::CerebrumError`] if the server doesn't know the reference.
    pub fn help_argument(&self, help_ref: &str) -> Result<String, BofhError> {
        self.help_request(&["arg_help", help_ref])
    }

    /// Run bofhd's variadic `help`, which the four public forms above spell out.
    ///
    /// # Panics
    ///
    /// Will normally never panic, unless the help text returned by the bofhd server is in an
    /// invalid format.
    fn help_request(&self, topic: &[&str]) -> Result<String, BofhError> {
        Ok(as_str(&self.run_raw_sess_command("help", topic)?)
            .expect("Invalid bofhd response")
            .to_owned())
    }

    /// Get the current Message of the Day from the bofhd server.
    ///
    /// The client identifies itself so the server can append a notice when the site expects a
    /// newer version of it than `client_version`; see [`CLIENT_ID`] and [`CLIENT_VERSION`] for
    /// what [`Self::new`] passes.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason.
    ///
    /// # Panics
    ///
    /// Will normally never panic, unless the Message of the Day returned by the bofhd server is in an invalid format.
    pub fn get_motd(&self, client_id: &str, client_version: &str) -> Result<String, BofhError> {
        Ok(as_str(&self.run_raw_command("get_motd", &[client_id, client_version])?)
            .expect("Invalid bofhd response")
            .to_owned())
    }
}

impl Drop for Bofh {
    /// Logs the user out of the bofhd session.
    fn drop(&mut self) {
        if self.session.is_some() {
            let _ = self.run_raw_sess_command("logout", &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Bofh, BofhError, bofhd_fault, escaped_argument, parse_prompt, read_response, wash_value,
    };
    use dxr::{Fault, MethodCall, Value};
    use std::borrow::Cow;

    /// A string value, which is what nearly everything bofhd sends is.
    fn text(string: &str) -> Value {
        Value::String(string.to_owned())
    }

    #[test]
    fn connect() {
        let _bofh = Bofh::new(String::from("https://cerebrum-uio-test.uio.no:8000"));
    }

    #[test]
    fn washing_decodes_none_and_unescapes_colons() {
        assert_eq!(wash_value(text(":None")), Value::Nil);
        assert_eq!(wash_value(text("::None")), text(":None"));
        assert_eq!(wash_value(text(":::foo")), text("::foo"));
        assert_eq!(wash_value(text("None")), text("None"));
        assert_eq!(wash_value(text("")), text(""));
        assert_eq!(wash_value(Value::Integer(1)), Value::Integer(1));
    }

    #[test]
    fn washing_recurses_into_arrays_and_structs() {
        assert_eq!(
            wash_value(Value::Array(vec![
                text("None"),
                text(":None"),
                text("::None"),
            ])),
            Value::Array(vec![text("None"), Value::Nil, text(":None"),])
        );
        assert_eq!(
            wash_value(Value::Struct(
                [
                    ("expire_date".to_owned(), text(":None")),
                    (
                        "affiliations".to_owned(),
                        Value::Array(vec![text(":None")])
                    ),
                ]
                .into()
            )),
            Value::Struct(
                [
                    ("expire_date".to_owned(), Value::Nil),
                    ("affiliations".to_owned(), Value::Array(vec![Value::Nil])),
                ]
                .into()
            )
        );
    }

    /// A `call_prompt_func` response shaped the way `misc print_passwords` sends it.
    fn prompt_response(raw: bool) -> Value {
        let row = |format: &str, name: &str, value: Value| {
            Value::Array(vec![
                Value::Array(vec![
                    text(format),
                    text(name),
                    text("password"),
                ]),
                value,
            ])
        };
        let mut strct = vec![
            ("prompt".to_owned(), text("Choose user(s)")),
            (
                "map".to_owned(),
                // The header carries no value of its own, and the rows below it are numbered
                // from 1 by the client, not by the server
                Value::Array(vec![
                    row("%8s %s", "uname", Value::Nil),
                    row("%-6s %s", "alice", Value::Integer(1)),
                    row("%-6s %s", "bob", Value::Integer(2)),
                ]),
            ),
            ("last_arg".to_owned(), Value::Boolean(true)),
        ];
        if raw {
            strct.push(("raw".to_owned(), Value::Boolean(true)));
        }
        Value::Struct(strct.into_iter().collect())
    }

    /// Each row is rendered with its own format, and carries the value the server mapped it to —
    /// never its position, whatever `raw` says. `raw` belongs to the map as a whole, and means
    /// the answer is the user's to write and the server's to read.
    #[test]
    fn a_prompt_map_keeps_the_values_the_server_sent() {
        for raw in [false, true] {
            let prompt = parse_prompt(&prompt_response(raw));
            let map = prompt.map.expect("the response has a map");

            assert_eq!(prompt.prompt.as_deref(), Some("Choose user(s)"));
            assert!(prompt.last_arg);
            assert_eq!(map.raw, raw);
            assert_eq!(map.header, "   uname password");
            assert_eq!(
                map.options
                    .iter()
                    .map(|option| (option.value.as_str(), option.label.as_str()))
                    .collect::<Vec<(&str, &str)>>(),
                [("1", "alice  password"), ("2", "bob    password")]
            );
        }
    }

    /// `raw` is absent far more often than it is false, and an absent flag is not a map to read
    /// the answer against either way.
    #[test]
    fn a_prompt_without_a_map_has_none() {
        let prompt = parse_prompt(&Value::Struct(
            [
                ("prompt".to_owned(), text("Enter printer name")),
                ("default".to_owned(), text("hp-7")),
            ]
            .into_iter()
            .collect(),
        ));

        assert!(prompt.map.is_none());
        assert_eq!(prompt.default.as_deref(), Some("hp-7"));
        assert!(!prompt.last_arg);
    }

    /// A `methodResponse` exactly as Python's `xmlrpc.client.dumps` writes it, which is what
    /// bofhd answers with.
    const RESPONSE: &str = r"<?xml version='1.0'?>
<methodResponse>
<params>
<param>
<value><struct>
<member>
<name>entity_name</name>
<value><string>foo</string></value>
</member>
<member>
<name>uid</name>
<value><int>1234</int></value>
</member>
<member>
<name>expire_date</name>
<value><string>:None</string></value>
</member>
<member>
<name>np_type</name>
<value><string>::literal</string></value>
</member>
</struct></value>
</param>
</params>
</methodResponse>
";

    /// The same, for a fault.
    const FAULT: &str = r"<?xml version='1.0'?>
<methodResponse>
<fault>
<value><struct>
<member>
<name>faultCode</name>
<value><int>1</int></value>
</member>
<member>
<name>faultString</name>
<value><string>Cerebrum.modules.bofhd.errors.CerebrumError:Unknown account</string></value>
</member>
</struct></value>
</fault>
</methodResponse>
";

    /// Reading a response off the wire is the one thing no other test covers, since there is no
    /// bofhd to talk to: the body here is what Python's marshaller really produces.
    #[test]
    fn a_response_is_read_and_washed_in_one_go() {
        let response = read_response(RESPONSE).expect("the response should parse");
        let strct = crate::as_struct(&response).expect("a struct response");

        assert_eq!(strct.get("entity_name"), Some(&text("foo")));
        assert_eq!(strct.get("uid"), Some(&Value::Integer(1234)));
        // Washed on the way in, so the client never sees bofhd's encoding
        assert_eq!(strct.get("expire_date"), Some(&Value::Nil));
        assert_eq!(strct.get("np_type"), Some(&text(":literal")));
    }

    /// A fault is an ordinary 200 with a different body, so it has to be told apart by shape.
    #[test]
    fn a_fault_body_is_read_as_the_error_it_stands_for() {
        match read_response(FAULT) {
            Err(BofhError::CerebrumError(message)) => assert_eq!(message, "Unknown account"),
            other => panic!("expected a Cerebrum error, got {other:?}"),
        }
    }

    /// Only three error types keep their own name; everything else is flattened server-side.
    #[test]
    fn each_kind_of_fault_string_maps_to_its_own_error() {
        let fault = |string: &str| bofhd_fault(&Fault::new(1, string.to_owned()));

        assert!(matches!(
            fault("Cerebrum.modules.bofhd.errors.ServerRestartedError:"),
            BofhError::ServerRestartedError
        ));
        assert!(matches!(
            fault("Cerebrum.modules.bofhd.errors.SessionExpiredError:go away"),
            BofhError::SessionExpiredError
        ));
        assert!(matches!(
            fault("Cerebrum.modules.bofhd.errors.UnknownError:what"),
            BofhError::Fault(_)
        ));
        assert!(matches!(
            fault("NotImplementedError:no such method"),
            BofhError::NotImplementedError(_)
        ));
        // A fault from something that isn't bofhd at all
        assert!(matches!(fault("boom"), BofhError::Fault(_)));
    }

    /// The arguments of a call go out as strings, with the session identifier unescaped in front.
    #[test]
    fn a_call_serialises_to_the_shape_bofhd_reads() {
        let call = MethodCall {
            name: Cow::Borrowed("run_command"),
            params: vec![
                text("session-id"),
                escaped_argument("user_info"),
                escaped_argument(":foo"),
            ],
        };
        let body = call.to_xml().expect("the call should serialise");
        let parsed = MethodCall::from_xml(&body).expect("and parse back");

        assert_eq!(parsed.name, "run_command");
        assert_eq!(
            parsed.params,
            [text("session-id"), text("user_info"), text("::foo")]
        );
    }

    /// Encoding an argument and washing it again should get us back what the user typed.
    #[test]
    fn escaping_an_argument_round_trips() {
        for argument in ["foo", ":foo", "::foo", ":None", "", ":"] {
            assert_eq!(wash_value(escaped_argument(argument)), text(argument));
        }
    }
}
