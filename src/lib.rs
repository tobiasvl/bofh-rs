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
    #[error("{0}")]
    Fault(String),
}

#[derive(Debug, Clone)]
pub struct Command {
    pub fullname: String,
    pub args: Vec<Argument>,
    pub format_suggestion: Option<String>,
    pub help: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Argument {
    pub optional: bool,
    pub repeat: bool,
    pub default: Option<String>,
    pub arg_type: Option<String>,
    pub help_ref: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Debug)]
enum ArgType {}

#[derive(Debug, Clone)]
pub struct CommandGroup {
    pub name: String,
    pub commands: BTreeMap<String, Command>,
}

/// The bofh client communicating with the bofhd server
pub struct Bofh {
    /// The URL to the bofhd server
    pub url: String,
    /// The Message Of The Day provided by the bofhd server after connection
    pub motd: Option<String>,
    session: Option<String>,
}

impl Bofh {
    /// Creates a new connection to a bofhd server, and tests the connection by requesting the server's Message of the Day.
    ///
    /// # Errors
    ///
    /// Will return a [`BofhError`] if the connection to the bofhd server fails, or it doesn't respond to the [`Self::get_motd`] command.
    pub fn new(url: String) -> Result<Self, BofhError> {
        let mut bofh = Self {
            url,
            session: None,
            motd: None,
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
                        Err(BofhError::Fault(fault.fault_string.clone()))
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
            let mut request = Request::new(command).arg(session.clone());
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

    fn get_commands(&mut self) -> Result<BTreeMap<String, CommandGroup>, BofhError> {
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
        Ok(commands)
    }

    /// Run a bofh command on the bofhd server.
    ///
    /// Note that this function actually runs the bofhd command `run_command bofh_command`, and can't be used to run raw bofhd commands. Those are all exposed through separate functions.
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason.
    ///
    /// If the bofhd session has expired and this function returns a [`BofhError::SessionExpiredError`], the client might want to reauthenticate using [`Self::login`] and then retry the command.
    pub fn run_command(&self, command: &str, args: &[&str]) -> Result<Value, BofhError> {
        // TODO: Return a formatted value?
        let args: Vec<&str> = {
            let mut command_args = vec![command];
            for &arg in args {
                command_args.push(arg);
            }
            command_args
        };
        self.run_raw_sess_command("run_command", &args)
    }

    /// Authenticate with the bofhd server. Sets up a session, and optionally populates `self` with the commands that the bofhd server reports as supported.
    ///
    /// Note that this consumes `password` to discourage user-facing clients to hold onto the user's password.
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
        self.session = Some(
            self.run_raw_command("login", &[username, &password])?
                .as_str()
                .expect("Invalid bofhd session identifier")
                .to_string(),
        );
        self.get_commands()
    }

    /// Get the current Message of the Day from the bofhd server
    ///
    /// # Errors
    ///
    /// Returns a [`BofhError`] if the command fails for some reason.
    ///
    /// # Panics
    ///
    /// Will normally never panic, unless the Message of the Day returned by the bofhd server is in an invalid format.
    pub fn get_motd(&self) -> Result<String, BofhError> {
        Ok(self
            .run_raw_command("get_motd", &[])?
            .as_str()
            .expect("Invalid bofhd response")
            .to_string())
    }
}

impl Drop for Bofh {
    #[allow(clippy::let_underscore_drop)]
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
        let _bofh = Bofh::new(String::from("https://cerebrum-uio-test.uio.no:8000"));
    }
}
