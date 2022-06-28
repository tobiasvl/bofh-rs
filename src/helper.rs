use rustyline::hint::{Hint, Hinter};
use rustyline::Context;
use rustyline_derive::{Completer, Helper, Highlighter, Validator};
use std::collections::BTreeMap;
#[derive(Completer, Helper, Validator, Highlighter)]
pub(crate) struct BofhHelper {
    pub(crate) commands: BTreeMap<String, bofh::CommandGroup>,
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

impl Hinter for BofhHelper {
    type Hint = CommandHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<CommandHint> {
        if line.is_empty() || pos < line.len() {
            return None;
        }

        let words: Vec<&str> = line.splitn(3, ' ').collect();

        // Hint arguments
        if words.len() > 2 {
            // TODO hint arguments
        }

        // Hint commands
        let mut pos = pos;
        let candidates: Vec<&str> = if words.len() == 1 {
            // Complete command group
            self.commands
                .keys()
                .filter_map(|command| {
                    if command.starts_with(words[0]) {
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
                        if command.starts_with(words[1]) {
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

        if candidates.len() != 1 {
            return None;
        }

        Some(
            CommandHint {
                display: String::from(candidates[0]),
            }
            .suffix(pos),
        )
    }
}
