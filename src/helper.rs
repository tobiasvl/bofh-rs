use colored::Colorize;
use rustyline::Context;
use rustyline::{
    completion::{Completer, Pair},
    highlight::Highlighter,
    hint::Hinter,
};
use rustyline_derive::{Helper, Validator};
use std::borrow::Cow;
use std::collections::BTreeMap;
use Cow::{Borrowed, Owned};
#[derive(Helper, Validator)]
pub(crate) struct BofhHelper<'a> {
    pub(crate) commands: &'a BTreeMap<String, bofh::CommandGroup>,
}

impl BofhHelper<'_> {
    pub(crate) fn command_candidates(&self, prefix: &str) -> Vec<&str> {
        self.commands
            .keys()
            .filter_map(|command| {
                if command.starts_with(prefix) {
                    Some(command.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    pub(crate) fn subcommand_candidates(&self, command: &str, prefix: &str) -> Vec<&str> {
        if let Some(command) = self.commands.get(command) {
            command
                .commands
                .keys()
                .filter_map(|command| {
                    if command.starts_with(prefix) {
                        Some(command.as_str())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            vec![]
        }
    }
}

impl Hinter for BofhHelper<'_> {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        let words: Vec<&str> = line.split_whitespace().collect();

        if words.is_empty() || pos < line.len() {
            return None;
        }

        let spaces = line.matches(char::is_whitespace).count();
        let mut word_pos = pos - spaces;

        let command_candidates = self.command_candidates(words[0]);
        let subcommand_candidates = if words.len() > 1 && command_candidates.len() == 1 {
            self.subcommand_candidates(command_candidates[0], words[1])
        } else {
            vec![]
        };

        // Hint arguments
        if words.len() >= 2 {
            // We can only hint arguments if we know the command and subcommand
            if command_candidates.len() == 1 && subcommand_candidates.len() == 1 {
                let command = self.commands.get(command_candidates[0]).unwrap();
                let subcommand = command.commands.get(subcommand_candidates[0]).unwrap();
                // Hint arguments if subcommand is complete or unambiguously partial
                if words[1] == subcommand.name || line.ends_with(char::is_whitespace) {
                    // TODO reduce this:
                    if words.len() >= 2 {
                        return Some(format!(
                            "{}{}",
                            if line.ends_with(char::is_whitespace) {
                                ""
                            } else {
                                " "
                            },
                            subcommand.args[words.len() - 2..]
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
            command_candidates
                .iter()
                .filter_map(|&command| {
                    if command == words[0] {
                        None
                    } else {
                        Some(command)
                    }
                })
                .collect()
        } else if words.len() == 2 {
            word_pos -= words[0].len();
            if command_candidates.len() == 1 {
                subcommand_candidates
                    .iter()
                    .filter_map(|&command| {
                        if command == words[1] {
                            None
                        } else {
                            Some(command)
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
        } else {
            let command_candidates = self.command_candidates(words[0]);

            if words.len() == 1 {
                if line.ends_with(char::is_whitespace) {
                    // Complete subcommands
                    if command_candidates.len() == 1 {
                        if let Some(command_group) = self.commands.get(command_candidates[0]) {
                            word_pos -= words[0].len();
                            command_group.commands.keys().map(String::as_str).collect()
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    }
                } else {
                    // Complete command group
                    command_candidates
                }
            } else if words.len() == 2 && !line.ends_with(char::is_whitespace) {
                word_pos -= words[0].len();
                // Complete subcommand
                if command_candidates.len() == 1 {
                    self.subcommand_candidates(command_candidates[0], words[1])
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
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

impl Highlighter for BofhHelper<'_> {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Owned(format!("{}", hint.bright_black()))
    }

    fn highlight<'l>(&self, line: &'l str, _: usize) -> Cow<'l, str> {
        let words: Vec<&str> = line.split_whitespace().collect();

        if words.is_empty() {
            return Borrowed(line);
        }

        let command_candidates = self.command_candidates(words[0]);
        let subcommand_candidates = if words.len() > 1 && command_candidates.len() == 1 {
            self.subcommand_candidates(command_candidates[0], words[1])
        } else {
            vec![]
        };

        let mut line = line.replace(
            words[0],
            &format!(
                "{}",
                match command_candidates.len() {
                    0 => words[0].red(),
                    1 => words[0].green(),
                    _ => words[0].yellow(),
                }
            ),
        );

        if words.len() > 1 {
            line = line.replace(
                words[1],
                &format!(
                    "{}",
                    match subcommand_candidates.len() {
                        0 => words[1].red(),
                        1 => words[1].green(),
                        _ => words[1].yellow(),
                    }
                ),
            );
        }

        Owned(line)
    }

    // TODO can highlighting be optimized?
    fn highlight_char(&self, _line: &str, _pos: usize) -> bool {
        true
    }
}
