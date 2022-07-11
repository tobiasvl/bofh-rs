use rustyline::hint::{Hint, Hinter};
use rustyline::Context;
use rustyline_derive::{Completer, Helper, Highlighter, Validator};
use std::collections::BTreeMap;
#[derive(Completer, Helper, Validator, Highlighter)]
pub(crate) struct BofhHelper<'a> {
    pub(crate) commands: &'a BTreeMap<String, bofh::CommandGroup>,
}

#[derive(Hash, Debug, PartialEq, Eq)]
pub(crate) struct CommandHint {
    display: String,
}

impl Hint for CommandHint {
    fn display(&self) -> &str {
        &self.display
    }

    fn completion(&self) -> Option<&str> {
        Some(&self.display)
    }
}

impl CommandHint {
    fn suffix(&self, strip_chars: usize) -> CommandHint {
        CommandHint {
            display: self.display[strip_chars..].to_owned(),
        }
    }
}

impl Hinter for BofhHelper<'_> {
    type Hint = CommandHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<CommandHint> {
        if line.is_empty() || pos < line.len() {
            return None;
        }

        let words: Vec<&str> = line.split_whitespace().collect();

        // Hint arguments
        if words.len() >= 2 {
            if let Some(command) = self.commands.get(words[0]) {
                if let Some(subcommand) = command.commands.get(words[1]) {
                    let args_to_hint = subcommand.args.len() - words.len() + 2;
                    if args_to_hint <= subcommand.args.len() {
                        return Some(CommandHint {
                            display: format!(
                                "{}{}",
                                if line.ends_with(char::is_whitespace) {
                                    ""
                                } else {
                                    " "
                                },
                                subcommand.args[subcommand.args.len() - args_to_hint..]
                                    .iter()
                                    .filter_map(|arg| arg.arg_type.clone())
                                    .collect::<Vec<String>>()
                                    .join(" ")
                            ),
                        });
                    }
                }
            }
        };

        // Hint commands
        let mut pos = pos;
        let candidates: Vec<&str> = if words.len() == 1 {
            // Complete command group
            self.commands
                .keys()
                .filter_map(|command| {
                    if command.starts_with(words[0]) && command != words[0] {
                        Some(command.as_str())
                    } else {
                        None
                    }
                })
                .collect()
        } else if words.len() == 2 {
            // Complete subcommand
            pos = pos - words[0].len() - 1;
            if let Some(command) = self.commands.get(words[0]) {
                command
                    .commands
                    .keys()
                    .filter_map(|command| {
                        if command.starts_with(words[1]) && command != words[1] {
                            Some(command.as_str())
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                vec![]
            }
        } else {
            return None;
        };

        if candidates.len() == 1 {
            return Some(
                CommandHint {
                    display: String::from(candidates[0]),
                }
                .suffix(pos),
            );
        }
        None
    }
}
