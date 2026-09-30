//! Internal, local commands to the bofh client.
//!
//! They share the first-word namespace with the server's command groups, so they abbreviate,
//! complete and highlight the same way, and `help` lists them alongside the
//! server's own groups.

use crate::Outcome;
use crate::helper::BofhHelper;
use bofh::Bofh;
use std::fmt::Write;

/// What the client should do when an internal command is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Commands,
    Help,
    Quit,
}

/// A command the client answers locally.
pub(crate) struct InternalCommand {
    /// The command name.
    pub(crate) name: &'static str,
    /// How the command is invoked.
    usage: &'static str,
    /// Description of the command, used by `help`.
    description: &'static str,
    kind: Kind,
}

/// Every internal command, in the order they are listed in, which is the order they are shown in.
pub(crate) const INTERNAL_COMMANDS: &[InternalCommand] = &[
    InternalCommand {
        name: "commands",
        usage: "commands",
        description: "list every command, with its arguments",
        kind: Kind::Commands,
    },
    InternalCommand {
        name: "help",
        usage: "help | help <group> | help <group> <command> | help arg_help <reference>",
        description: "explain a command group, a command or an argument",
        kind: Kind::Help,
    },
    InternalCommand {
        name: "logout",
        usage: "logout",
        description: "log out and quit",
        kind: Kind::Quit,
    },
    InternalCommand {
        name: "quit",
        usage: "quit",
        description: "log out and quit",
        kind: Kind::Quit,
    },
];

/// Look up an internal command by its full name.
pub(crate) fn lookup(name: &str) -> Option<&'static InternalCommand> {
    INTERNAL_COMMANDS
        .iter()
        .find(|command| command.name == name)
}

/// Run an internal command, with the words following it as its arguments.
pub(crate) fn run(
    command: &InternalCommand,
    args: &[&str],
    bofh: &Bofh,
    helper: &BofhHelper,
) -> Outcome {
    match command.kind {
        Kind::Commands if args.is_empty() => Outcome::Output(commands(helper)),
        Kind::Quit if args.is_empty() => Outcome::Quit,
        Kind::Help => help(args, bofh, helper).map_or_else(Outcome::Failed, Outcome::Output),
        _ => Outcome::Failed(format!("Usage: {}", command.usage)),
    }
}

/// List every command the user can run, as they would type it.
fn commands(helper: &BofhHelper) -> String {
    let mut lines: Vec<String> = Vec::new();

    for group in helper.commands.values() {
        for command in group.commands.values() {
            let mut line = format!("{} {}", group.name, command.name);
            for argument in &command.args {
                // The type is what the hinter shows as well
                let name = argument
                    .arg_type
                    .as_deref()
                    .or(argument.help_ref.as_deref())
                    .unwrap_or("argument");
                let repeat = if argument.repeat { "..." } else { "" };
                let _ = if argument.optional {
                    write!(line, " [{name}{repeat}]")
                } else {
                    write!(line, " <{name}{repeat}>")
                };
            }
            lines.push(line);
        }
    }

    lines.push(String::from("\nCommands handled by the client itself:"));
    for command in INTERNAL_COMMANDS {
        lines.push(format!("   {}", command.usage));
    }

    lines.join("\n")
}

/// Explain a command group, a command, an argument type, or the whole command set.
fn help(args: &[&str], bofh: &Bofh, helper: &BofhHelper) -> Result<String, String> {
    match args {
        [] => general_help(bofh),
        // `arg_help` is not a command group but bofhd's way of naming argument types, so it isn't
        // resolved against anything; the reference comes from an argument's `help_ref`
        ["arg_help", help_ref] => bofh.help_argument(help_ref).map_err(|err| err.to_string()),
        ["arg_help", ..] => Err(String::from("Usage: help arg_help <reference>")),
        [topic] => {
            let topic = helper.resolve_command_name(topic)?;
            match lookup(topic) {
                Some(command) => Ok(format!("{} -- {}", command.usage, command.description)),
                None => bofh.help_group(topic).map_err(|err| err.to_string()),
            }
        }
        [group, command] => {
            let group = helper.resolve_command_name(group)?;
            if let Some(internal) = lookup(group) {
                return Err(format!(
                    "'{}' is handled by the client and has no subcommands",
                    internal.name
                ));
            }
            let command = helper.resolve_subcommand_name(group, command)?;
            bofh.help_command(group, command)
                .map_err(|err| err.to_string())
        }
        _ => Err(String::from(
            "Usage: help | help <group> | help <group> <command> | help arg_help <reference>",
        )),
    }
}

/// The server's general help, with the client's own commands appended to it.
///
/// The server lists one command group per line and cannot know about the internal commands, so
/// they are added here.
fn general_help(bofh: &Bofh) -> Result<String, String> {
    let mut help = bofh.help().map_err(|err| err.to_string())?;

    help.push_str("\n\nCommands handled by the client itself:\n");
    for command in INTERNAL_COMMANDS {
        // The same layout the server uses for its own command groups
        let _ = writeln!(help, "   {:<10} - {}", command.name, command.description);
    }

    Ok(help)
}

#[cfg(test)]
mod tests {
    use super::{INTERNAL_COMMANDS, commands, lookup};
    use crate::helper::BofhHelper;
    use bofh::{Argument, Command, CommandGroup};
    use std::collections::BTreeMap;

    /// One group whose arguments cover every way an argument can be written out.
    fn helper() -> BofhHelper {
        let arguments = [
            ("info", vec![argument("accountName", false, false)]),
            (
                "password",
                vec![
                    argument("accountName", false, false),
                    argument("accountPassword", true, false),
                ],
            ),
            ("set_spread", vec![argument("spread", false, true)]),
            ("nameless", vec![Argument::default()]),
        ];

        BofhHelper {
            commands: BTreeMap::from([(
                String::from("user"),
                CommandGroup {
                    name: String::from("user"),
                    commands: arguments
                        .into_iter()
                        .map(|(name, args)| {
                            (
                                name.to_owned(),
                                Command {
                                    fullname: format!("user_{name}"),
                                    name: name.to_owned(),
                                    args,
                                    help: None,
                                },
                            )
                        })
                        .collect(),
                },
            )]),
        }
    }

    fn argument(arg_type: &str, optional: bool, repeat: bool) -> Argument {
        Argument {
            optional,
            repeat,
            arg_type: Some(arg_type.to_owned()),
            ..Argument::default()
        }
    }

    #[test]
    fn commands_are_listed_as_they_would_be_typed() {
        // Not as a dump of the client's own command table, which is what the other two clients do
        let listing = commands(&helper());
        let listed: Vec<&str> = listing.lines().take(4).collect();
        assert_eq!(
            listed,
            [
                "user info <accountName>",
                // An argument with no type at all still has to be shown as taking something
                "user nameless <argument>",
                "user password <accountName> [accountPassword]",
                "user set_spread <spread...>",
            ]
        );
    }

    #[test]
    fn the_command_listing_includes_the_clients_own_commands() {
        let listing = commands(&helper());
        for command in INTERNAL_COMMANDS {
            assert!(
                listing.contains(command.usage),
                "'{}' is missing from the command listing",
                command.name
            );
        }
    }

    #[test]
    fn quit_and_logout_are_the_same_command_under_two_names() {
        assert_eq!(lookup("quit").unwrap().kind, lookup("logout").unwrap().kind);
        assert!(lookup("halt_and_catch_fire").is_none());
        // Abbreviations are resolved before lookup, so only full names are ever looked up
        assert!(lookup("q").is_none());
    }
}
