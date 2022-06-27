use std::collections::BTreeMap;
use thiserror::Error;
use xmlrpc::{Request, Value};

#[derive(Error, Debug)]
pub enum BofhError {
    #[error("{0}")]
    XmlRpcError(#[from] xmlrpc::Error),
    #[error("Attempted to run session command before session was established")]
    NoSessionError,
    #[error("{0}")]
    CerebrumError(String),
    #[error("Server restarted")]
    ServerRestartedError,
    #[error("Session expired")]
    SessionExpiredError,
    #[error("{0}")]
    NotImplementedError(String),
    #[error("Incomplete command, possible subcommands: [{0}]")]
    IncompleteCommandError(String),
    #[error("Unknown command")]
    UnknownCommandError,
    #[error("{0}")]
    Fault(String),
}

#[derive(Debug)]
struct Command {
    fullname: String,
    args: Vec<Argument>,
    format_suggestion: Option<String>,
    help: Option<String>,
}

#[derive(Debug, Default)]
struct Argument {
    optional: bool,
    repeat: bool,
    default: Option<String>,
    arg_type: Option<String>,
    help_ref: Option<String>,
    prompt: Option<String>,
}

#[derive(Debug)]
enum ArgType {}

#[derive(Debug)]
struct CommandGroup {
    name: String,
    commands: BTreeMap<String, Command>,
}

pub struct Bofh {
    /// The URL to the bofhd server
    pub url: String,
    /// The Message Of The Day provided by the bofhd server after connection
    pub motd: Option<String>,
    session: Option<String>,
    commands: Option<BTreeMap<String, CommandGroup>>,
}

impl Bofh {
    /// Creates a new connection to a bofhd server, and tests the connection by requesting the server's Message of the Day
    pub fn new(url: String) -> Result<Self, BofhError> {
        let mut bofh = Self {
            url,
            session: None,
            motd: None,
            commands: None,
        };
        bofh.motd = Some(bofh.get_motd()?);
        Ok(bofh)
    }

    fn run_request(&self, request: Request) -> Result<Value, BofhError> {
        match request.call_url(&self.url) {
            Ok(result) => Ok(result),
            Err(err) => {
                if let Some(fault) = err.fault() {
                    if let Some(bofhd_error) = fault
                        .fault_string
                        .strip_prefix("Cerebrum.modules.bofhd.errors.")
                    {
                        if let Some(cerebrum_error) = bofhd_error.strip_prefix("CerebrumError:") {
                            Err(BofhError::CerebrumError(cerebrum_error.to_string()))
                        } else if bofhd_error.strip_prefix("ServerRestartedError:").is_some() {
                            //Err(BofhError::ServerRestartedError)
                            //self.init_commands(True);
                            self.run_request(request)
                        } else if bofhd_error.strip_prefix("SessionExpiredError:").is_some() {
                            //Err(BofhError::SessionExpiredError(request))
                            todo!() // TODO
                        } else {
                            unimplemented!()
                        }
                    } else if let Some(not_implemented_error) =
                        fault.fault_string.strip_prefix("NotImplementedError:")
                    {
                        Err(BofhError::NotImplementedError(
                            not_implemented_error.to_string(),
                        ))
                    } else {
                        Err(BofhError::Fault(fault.fault_string.to_owned()))
                    }
                } else {
                    Err(BofhError::XmlRpcError(err))
                }
            }
        }
    }

    fn run_raw_command(&self, command: &str, args: &[&str]) -> Result<Value, BofhError> {
        let mut request = Request::new(command);
        for arg in args {
            request = request.arg(*arg);
        }
        self.run_request(request)
    }

    fn run_raw_sess_command(&self, command: &str, args: &[&str]) -> Result<Value, BofhError> {
        if let Some(session) = &self.session {
            let mut request = Request::new(command).arg(session.to_owned());
            for arg in args {
                request = request.arg(*arg);
            }
            self.run_request(request)
        } else {
            // TODO Maybe just panic here instead, this should never happen
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

    fn init_commands(&mut self) -> Result<(), BofhError> {
        let response = self.run_raw_sess_command("get_commands", &[])?;
        let mut commands = BTreeMap::<String, CommandGroup>::new();
        for (cmd, array) in response.as_struct().unwrap() {
            let cmd_group = array[0].as_array().unwrap()[0].as_str().unwrap();
            if !commands.contains_key(cmd_group) {
                commands.insert(
                    cmd_group.into(),
                    CommandGroup {
                        name: cmd_group.into(),
                        commands: BTreeMap::new(),
                    },
                );
            }
            commands.get_mut(cmd_group).unwrap().commands.insert(
                array[0].as_array().unwrap()[1].as_str().unwrap().into(),
                Command {
                    fullname: cmd.into(),
                    args: match &array[1] {
                        Value::Array(array) => {
                            let mut vector = vec![];
                            for strct in array {
                                let strct = strct.as_struct().unwrap();
                                vector.push(Argument {
                                    optional: match strct
                                        .get("optional")
                                        .or(Some(&Value::Bool(false)))
                                    {
                                        Some(Value::Bool(value)) => value.to_owned(),
                                        Some(Value::String(value)) => {
                                            matches!(value.as_str(), "True")
                                        }
                                        _ => false,
                                    },
                                    repeat: match strct.get("repeat").or(Some(&Value::Bool(false)))
                                    {
                                        Some(Value::Bool(value)) => value.to_owned(),
                                        Some(Value::String(value)) => {
                                            matches!(value.as_str(), "True")
                                        }
                                        _ => false,
                                    },
                                    default: strct
                                        .get("default")
                                        .map(|x| x.as_str().unwrap().to_string()),
                                    arg_type: strct
                                        .get("type")
                                        .map(|x| x.as_str().unwrap().to_string()),
                                    help_ref: strct
                                        .get("help_ref")
                                        .map(|x| x.as_str().unwrap().to_string()),
                                    prompt: strct
                                        .get("prompt")
                                        .map(|x| x.as_str().unwrap().to_string()),
                                });
                            }
                            vector
                        }
                        Value::String(_) => vec![Argument::default()], // prompt_func
                        _ => vec![],
                    },
                    format_suggestion: None,
                    help: None,
                },
            );
        }
        self.commands = Some(commands);
        Ok(())
    }

    /// Run a command
    pub fn run_command(&self, args: &[&str]) -> Result<Value, BofhError> {
        let mut request = Request::new("run_command").arg(self.session.to_owned());
        if let Some(commands) = &self.commands {
            if let Some(command_group) = commands.get(args[0]) {
                if args.len() == 1 {
                    return Err(BofhError::IncompleteCommandError(
                        command_group
                            .commands
                            .keys()
                            .cloned()
                            .collect::<Vec<String>>()
                            .join(", "),
                    ));
                }
                if let Some(subcommand) = command_group.commands.get(args[1]) {
                    request = request.arg(subcommand.fullname.as_str());
                } else {
                    return Err(BofhError::UnknownCommandError);
                }
                for arg in &args[2..] {
                    request = request.arg(*arg);
                }
            } else {
                return Err(BofhError::UnknownCommandError);
            }
            self.run_request(request)
        } else {
            Err(BofhError::NoSessionError)
        }
    }

    /// Authenticate with the bofhd server. Sets up a session, and optionally populates `self` with the commands that the bofhd server reports as supported.
    pub fn login(&mut self, username: &str, password: String, init: bool) -> Result<(), BofhError> {
        self.session = Some(
            self.run_raw_command("login", &[username, &password])?
                .as_str()
                .unwrap()
                .to_string(),
        );
        if init {
            self.init_commands()?;
        }
        Ok(())
    }

    /// Get the current Message of the Day from the bofhd server
    pub fn get_motd(&self) -> Result<String, BofhError> {
        Ok(self
            .run_raw_command("get_motd", &[])?
            .as_str()
            .unwrap()
            .to_string())
    }

    /// Gets the commands that the bofhd server reports that it supports.
    /// Note that the server might have hidden commands.
    pub fn get_commands(&self) -> Result<Value, BofhError> {
        self.run_raw_sess_command("get_commands", &[])
    }
}

impl Drop for Bofh {
    fn drop(&mut self) {
        if self.session.is_some() {
            let _ = self.run_raw_sess_command("logout", &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Bofh;
    #[test]
    fn connect() {
        let _ = Bofh::new(String::from("https://cerebrum-uio-test.uio.no:8000"));
    }
}
