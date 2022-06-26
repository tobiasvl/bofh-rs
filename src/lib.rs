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
    #[error("Unknown command")]
    NotImplementedError,
    #[error("{0}")]
    Fault(String),
}

pub struct Bofh {
    /// The URL to the bofhd server
    pub url: String,
    session: Option<String>,
    /// The Message Of The Day provided by the bofhd server after connection
    pub motd: Option<String>,
}

impl Bofh {
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
                    } else if fault
                        .fault_string
                        .strip_prefix("NotImplementedError:")
                        .is_some()
                    {
                        Err(BofhError::NotImplementedError)
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

    pub fn run_command(&self, command: &str, args: &[&str]) -> Result<Value, BofhError> {
        self.run_raw_sess_command(command, args)
    }

    pub fn login(
        &mut self,
        username: &str,
        password: String,
        _init: bool,
    ) -> Result<(), BofhError> {
        self.session = Some(
            self.run_raw_command("login", &[username, &password])?
                .as_str()
                .unwrap()
                .to_string(),
        );
        Ok(())
    }

    pub fn get_motd(&self) -> Result<String, BofhError> {
        Ok(self
            .run_raw_command("get_motd", &[])?
            .as_str()
            .unwrap()
            .to_string())
    }
}

impl Drop for Bofh {
    fn drop(&mut self) {
        if self.session.is_some() {
            let _ = self.run_raw_sess_command("logout", &[]);
        }
        self.session = None;
        // TODO bring down all commands
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
