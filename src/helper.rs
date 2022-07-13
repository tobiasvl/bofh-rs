use rustyline::Context;
use rustyline::{
    completion::{Completer, Pair},
    hint::Hinter,
};
use rustyline_derive::{Helper, Highlighter, Validator};
use std::collections::BTreeMap;
#[derive(Helper, Validator, Highlighter)]
pub(crate) struct BofhHelper<'a> {
    pub(crate) commands: &'a BTreeMap<String, bofh::CommandGroup>,
}

impl Hinter for BofhHelper<'_> {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        if line.is_empty() || pos < line.len() {
            return None;
        }

        let words: Vec<&str> = line.split_whitespace().collect();
        let spaces = line.matches(char::is_whitespace).count();
        let mut word_pos = pos - spaces;

        // Hint arguments
        if words.len() >= 2 {
            if let Some(command) = self.commands.get(words[0]) {
                if let Some(subcommand) = command.commands.get(words[1]) {
                    let args_to_hint = subcommand.args.len() - words.len() + 2;
                    if args_to_hint <= subcommand.args.len() {
                        return Some(format!(
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
                        ));
                    }
                }
            }
        };

        // If we're not hinting arguments, and the line ends in a whitespace, we shouldn't hint.
        // This fixes a bug where inserting spaces when a hint has appeared will push the hint towards the right.
        //
        // TODO In the unlikely scenario that the server only supports one command, or it has a command
        // TODO which only supports one subcommand, this will erroneously cause that (sub)command not to
        // TODO be hinted! Should probably be fixed in a better way, just in case.
        if line.ends_with(char::is_whitespace) {
            return None;
        }

        // Hint commands
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
            word_pos -= words[0].len();
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

        // We only give unambiguous hints, ie. if there is one and only one hint
        if candidates.len() == 1 {
            Some(candidates[0][word_pos..].to_owned())
        } else {
            None
        }
    }
}

impl Completer for BofhHelper<'_> {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let spaces = line.matches(char::is_whitespace).count();
        let mut word_pos = pos - spaces;

        // Complete commands
        let candidates: Vec<&str> = if words.is_empty() {
            // Completing on an empty line shows all command groups
            self.commands.keys().map(String::as_str).collect()
        } else if words.len() == 1 {
            let candidates = if line.ends_with(char::is_whitespace) {
                // Complete subcommands
                if let Some(command_group) = self.commands.get(words[0]) {
                    word_pos -= words[0].len();
                    command_group.commands.keys().map(String::as_str).collect()
                } else {
                    vec![]
                }
            } else {
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
            };
            candidates
        } else if words.len() == 2 && !line.ends_with(char::is_whitespace) {
            word_pos -= words[0].len();
            // Complete subcommand
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
            vec![]
        };

        Ok((
            pos,
            candidates
                .iter()
                .map(|&candidate| Pair {
                    display: candidate.to_owned(),
                    replacement: if candidates.len() == 1 {
                        format!("{} ", &candidate[word_pos..])
                    } else {
                        candidate[word_pos..].to_owned()
                    },
                })
                .collect(),
        ))
    }
}
