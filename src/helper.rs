use Cow::{Borrowed, Owned};
use colored::Colorize;
use rustyline::Context;
use rustyline::{
    completion::{Completer, Pair},
    highlight::{CmdKind, Highlighter},
    hint::Hinter,
};
use rustyline_derive::{Helper, Validator};
use std::borrow::Cow;
use std::collections::BTreeMap;

#[derive(Helper, Validator)]
pub(crate) struct BofhHelper {
    pub(crate) commands: BTreeMap<String, bofh::CommandGroup>,
}

impl BofhHelper {
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

impl Hinter for BofhHelper {
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
                    if words.len() >= 2 && words.len() < subcommand.args.len() + 2 {
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
        }

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

impl Completer for BofhHelper {
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
                    // FIXME move this to highlight_candidate when that accepts a completion::Candidate
                    // See https://github.com/kkawakam/rustyline/issues/642
                    display: format!(
                        "{}{}",
                        candidate[..word_pos].green(),
                        candidate[word_pos..].bright_green().bold()
                    ),
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

/// Colorize a (sub)command by how many commands it could still refer to: none, exactly one, or
/// several.
fn colorize_command(candidates: &[&str], word: &str) -> String {
    match candidates.len() {
        0 => word.bright_red().bold(),
        1 => word.bright_green().bold(),
        _ => word.bright_yellow().bold(),
    }
    .to_string()
}

impl Highlighter for BofhHelper {
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

        // The words are spliced in by byte offset rather than substring replacement, because the
        // subcommand is usually also a prefix of the command group (eg. "person p"), so searching
        // for it would find and colorize the wrong occurrence.
        let mut highlighted = String::with_capacity(line.len());
        let mut cursor = 0;
        for (word, candidates) in words
            .iter()
            .zip([&command_candidates, &subcommand_candidates])
        {
            let start = cursor
                + line[cursor..]
                    .find(word)
                    .expect("Failed to locate a word we just split out of the line");
            highlighted.push_str(&line[cursor..start]);
            highlighted.push_str(&colorize_command(candidates, word));
            cursor = start + word.len();
        }
        highlighted.push_str(&line[cursor..]);

        Owned(highlighted)
    }

    // TODO can highlighting be optimized?
    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{BofhHelper, colorize_command};
    use bofh::{Command, CommandGroup};
    use rustyline::highlight::Highlighter;
    use std::collections::BTreeMap;

    fn helper() -> BofhHelper {
        // `colored` turns itself off when stdout isn't a terminal, which it isn't under `cargo test`
        colored::control::set_override(true);

        let mut commands = BTreeMap::new();
        for (group, subcommands) in [("person", ["info", "find"]), ("user", ["info", "password"])] {
            commands.insert(
                group.to_owned(),
                CommandGroup {
                    name: group.to_owned(),
                    commands: subcommands
                        .iter()
                        .map(|&name| {
                            (
                                name.to_owned(),
                                Command {
                                    fullname: format!("{group}_{name}"),
                                    name: name.to_owned(),
                                    args: vec![],
                                    help: None,
                                },
                            )
                        })
                        .collect(),
                },
            );
        }
        BofhHelper { commands }
    }

    /// Strip the ANSI escapes, so we can check that highlighting doesn't reorder the line itself.
    fn strip_colors(line: &str) -> String {
        let mut stripped = String::new();
        let mut chars = line.chars();
        while let Some(char) = chars.next() {
            if char == '\u{1b}' {
                for char in chars.by_ref() {
                    if char == 'm' {
                        break;
                    }
                }
            } else {
                stripped.push(char);
            }
        }
        stripped
    }

    #[test]
    fn highlighting_preserves_the_line() {
        for line in [
            "person",
            "person p",
            "person info",
            "user u",
            "user info foo",
            "  person   info  ",
            "nonsense x y",
        ] {
            assert_eq!(strip_colors(&helper().highlight(line, line.len())), line);
        }
    }

    #[test]
    fn highlighting_colorizes_whole_words() {
        // "p" also occurs inside "person", so searching for it would colorize the wrong occurrence
        assert_eq!(
            helper().highlight("person p", 8),
            format!(
                "{} {}",
                colorize_command(&["person"], "person"),
                colorize_command(&[], "p")
            )
        );
        assert_eq!(
            helper().highlight("user p", 6),
            format!(
                "{} {}",
                colorize_command(&["user"], "user"),
                colorize_command(&["password"], "p")
            )
        );
    }
}
