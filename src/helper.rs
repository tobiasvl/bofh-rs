use crate::internal;
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

/// Whether `command` begins with `prefix`, whatever case either was typed in.
fn starts_with_ignoring_case(command: &str, prefix: &str) -> bool {
    command
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Resolve `word` to itself or any candidates it is a prefix of.
fn resolve<'a>(candidates: &[&'a str], word: &str) -> Option<&'a str> {
    match candidates {
        [command] => Some(command),
        candidates => candidates
            .iter()
            .find(|&&command| command.eq_ignore_ascii_case(word))
            .copied(),
    }
}

/// Explain to the user why a word didn't name exactly one command.
fn ambiguity(candidates: &[&str], word: &str) -> String {
    if candidates.is_empty() {
        format!("Unknown command '{word}'")
    } else {
        format!(
            "Ambiguous command '{word}', could be: {}",
            candidates.join(", ")
        )
    }
}

impl BofhHelper {
    /// Every command group (internal and from the server) that starts with `prefix`, sorted.
    pub(crate) fn command_candidates(&self, prefix: &str) -> Vec<&str> {
        let mut candidates: Vec<&str> = internal::INTERNAL_COMMANDS
            .iter()
            .map(|command| command.name)
            .chain(self.commands.keys().map(String::as_str))
            .filter(|command| starts_with_ignoring_case(command, prefix))
            .collect();
        candidates.sort_unstable();
        candidates
    }

    /// Expand an abbreviated first word into the one command it names.
    ///
    /// # Errors
    ///
    /// Returns a message meant for the user if the word names no command, or more than one.
    pub(crate) fn resolve_command_name(&self, word: &str) -> Result<&str, String> {
        let candidates = self.command_candidates(word);
        resolve(&candidates, word).ok_or_else(|| ambiguity(&candidates, word))
    }

    /// Expand an abbreviated subcommand of a command `group` into the one command it names.
    ///
    /// # Errors
    ///
    /// Returns a message meant for the user if the word names no command, or more than one.
    pub(crate) fn resolve_subcommand_name(&self, group: &str, word: &str) -> Result<&str, String> {
        let candidates = self.subcommand_candidates(group, word);
        resolve(&candidates, word).ok_or_else(|| ambiguity(&candidates, &format!("{group} {word}")))
    }

    /// Every subcommand of `command` that starts with `prefix`.
    /// Where the command a `help` line asks about begins, if this is one.
    ///
    /// `help user info` asks about `user info`, so what follows `help` is completed, hinted and
    /// coloured as a command line of its own, offset by however long `help` and the whitespace
    /// after it were. `None` for any other line — and for `help` with nothing after it yet,
    /// since that is still a command name being typed.
    ///
    /// `help arg_help <reference>` is the exception: `arg_help` is bofhd's name for argument
    /// help, not a command, so that line is left to be read as an ordinary one, where the words
    /// after an internal command are arguments and aren't coloured as anything.
    fn topic_start(&self, line: &str) -> Option<usize> {
        let start = line.len() - line.trim_start().len();
        let rest = &line[start..];
        let end = rest.find(char::is_whitespace)?;
        let name = self.resolve_command_name(&rest[..end]).ok()?;
        if !internal::lookup(name).is_some_and(internal::InternalCommand::names_a_command) {
            return None;
        }
        let after = &rest[end..];
        let topic = start + end + (after.len() - after.trim_start().len());
        if line[topic..].split_whitespace().next() == Some("arg_help") {
            return None;
        }
        Some(topic)
    }

    /// Colour the words of a line that name a command by how many commands they could be.
    fn highlight_command<'l>(&self, line: &'l str) -> Cow<'l, str> {
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
        // What follows one of the client's own commands is an argument to it, not a subcommand,
        // so it says nothing about whether the command is right: `source ~/file` is a filename,
        // not a subcommand of `source` that doesn't exist
        let names = match command_candidates[..] {
            [command] if internal::lookup(command).is_some() => 1,
            _ => 2,
        };
        for (word, candidates) in words
            .iter()
            .zip([&command_candidates, &subcommand_candidates])
            .take(names)
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

    pub(crate) fn subcommand_candidates(&self, command: &str, prefix: &str) -> Vec<&str> {
        if let Some(command) = self.commands.get(command) {
            command
                .commands
                .keys()
                .filter_map(|command| {
                    if starts_with_ignoring_case(command, prefix) {
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
        match self.topic_start(line) {
            // `help` takes the name of a command, not that command's arguments, so only the name
            // is hinted
            Some(at) if pos >= at => self.hint_command(&line[at..], pos - at, false),
            _ => self.hint_command(line, pos, true),
        }
    }
}

impl BofhHelper {
    /// Hint the rest of a command line, the arguments it takes too if `with_arguments`.
    fn hint_command(&self, line: &str, pos: usize, with_arguments: bool) -> Option<String> {
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
        if with_arguments && words.len() >= 2 {
            // We can only hint arguments if we know the command and subcommand
            if command_candidates.len() == 1 && subcommand_candidates.len() == 1 {
                let command = self.commands.get(command_candidates[0]).unwrap();
                let subcommand = command.commands.get(subcommand_candidates[0]).unwrap();
                // Hint arguments if subcommand is complete or unambiguously partial
                if words[1].eq_ignore_ascii_case(&subcommand.name)
                    || line.ends_with(char::is_whitespace)
                {
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
                    if command.eq_ignore_ascii_case(words[0]) {
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
                        if command.eq_ignore_ascii_case(words[1]) {
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
        Ok(match self.topic_start(line) {
            Some(at) if pos >= at => {
                let (start, candidates) = self.complete_command(&line[at..], pos - at);
                (start + at, candidates)
            }
            _ => self.complete_command(line, pos),
        })
    }
}

impl BofhHelper {
    /// Complete the command named by a line: its group, or a subcommand of that group.
    fn complete_command(&self, line: &str, pos: usize) -> (usize, Vec<Pair>) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let spaces = line.matches(char::is_whitespace).count();
        let mut word_pos = pos - spaces;

        // Complete commands
        let candidates: Vec<&str> = if words.is_empty() {
            // Completing on an empty line shows all command groups
            self.command_candidates("")
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

        (
            pos - word_pos,
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
                        format!("{candidate} ")
                    } else {
                        candidate.to_owned()
                    },
                })
                .collect(),
        )
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
        match self.topic_start(line) {
            Some(at) => Owned(format!(
                "{}{}",
                self.highlight_command(&line[..at]),
                self.highlight_command(&line[at..])
            )),
            None => self.highlight_command(line),
        }
    }

    // TODO can highlighting be optimized?
    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{BofhHelper, colorize_command};
    use bofh::{Argument, Command, CommandGroup};
    use rustyline::Context;
    use rustyline::completion::Completer;
    use rustyline::highlight::Highlighter;
    use rustyline::hint::Hinter;
    use rustyline::history::MemHistory;
    use std::collections::BTreeMap;

    /// A helper over a handful of real command groups, chosen for the awkward cases: `history`
    /// collides with the internal `help` on the prefix `h`, and `group add` is spelled out in full
    /// while also being a prefix of `group add_admin`.
    pub(crate) fn helper() -> BofhHelper {
        // `colored` turns itself off when stdout isn't a terminal, which it isn't under `cargo test`
        colored::control::set_override(true);

        let mut commands = BTreeMap::new();
        for (group, subcommands) in [
            ("group", &["add", "add_admin", "info"][..]),
            ("history", &["show"][..]),
            ("person", &["find", "info"][..]),
            ("user", &["info", "password"][..]),
        ] {
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
                                    prompt_func: false,
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
    fn the_client_and_the_server_share_one_namespace() {
        // Sorted as one list, not the client's commands and then the server's
        assert_eq!(
            helper().command_candidates(""),
            [
                "commands", "group", "help", "history", "logout", "person", "quit", "script",
                "source", "user"
            ]
        );
        assert_eq!(helper().resolve_command_name("q"), Ok("quit"));
        assert_eq!(helper().resolve_command_name("pe"), Ok("person"));
        // Two of the client's own commands can be ambiguous with each other, not just with the
        // server's
        assert_eq!(
            helper().resolve_command_name("s"),
            Err(String::from(
                "Ambiguous command 's', could be: script, source"
            ))
        );
    }

    /// Command names are the server's, and it only has lowercase ones, so the case a user holds
    /// shift for carries nothing worth honouring.
    #[test]
    fn a_command_can_be_typed_in_any_case() {
        assert_eq!(helper().resolve_command_name("USER"), Ok("user"));
        assert_eq!(helper().resolve_command_name("User"), Ok("user"));
        assert_eq!(helper().resolve_command_name("Pe"), Ok("person"));
        assert_eq!(helper().resolve_subcommand_name("user", "I"), Ok("info"));
        assert_eq!(helper().resolve_subcommand_name("user", "InFo"), Ok("info"));
        // Case doesn't make an abbreviation any less ambiguous
        assert_eq!(
            helper().resolve_command_name("H"),
            Err(String::from(
                "Ambiguous command 'H', could be: help, history"
            ))
        );
        // Nor does it reach a command that isn't there
        assert_eq!(
            helper().resolve_command_name("ZZZ"),
            Err(String::from("Unknown command 'ZZZ'"))
        );
        // The rule that a word spelled out in full beats the longer commands it prefixes still
        // holds when it is shouted
        assert_eq!(helper().resolve_subcommand_name("group", "ADD"), Ok("add"));
        assert_eq!(helper().command_candidates("GR"), ["group"]);
    }

    /// A line that will run has to look like it will run, so the colouring folds case too — and
    /// it still colours the word the user typed, not the command's own spelling.
    #[test]
    fn highlighting_recognizes_a_command_in_any_case() {
        assert_eq!(
            helper().highlight("USER P", 6),
            format!(
                "{} {}",
                colorize_command(&["user"], "USER"),
                colorize_command(&["password"], "P")
            )
        );
        assert_eq!(
            strip_colors(&helper().highlight("User Info foo", 13)),
            "User Info foo"
        );
    }

    #[test]
    fn an_abbreviation_has_to_be_unambiguous() {
        // "h" is the interesting one: it is only ambiguous because the internal `help` sits in the
        // same namespace as the server's `history`
        assert_eq!(
            helper().resolve_command_name("h"),
            Err(String::from(
                "Ambiguous command 'h', could be: help, history"
            ))
        );
        assert_eq!(
            helper().resolve_command_name("zzz"),
            Err(String::from("Unknown command 'zzz'"))
        );
        assert_eq!(
            helper().resolve_subcommand_name("user", "i"),
            Ok("info") // only one candidate, so no exact match needed
        );
        assert_eq!(
            helper().resolve_subcommand_name("user", "z"),
            Err(String::from("Unknown command 'user z'"))
        );
    }

    /// bofhd has 34 commands whose name is also the start of a sibling's, `group add` among them.
    #[test]
    fn a_command_spelled_out_in_full_beats_the_longer_ones_it_prefixes() {
        assert_eq!(helper().resolve_subcommand_name("group", "add"), Ok("add"));
        assert_eq!(
            helper().resolve_subcommand_name("group", "add_"),
            Ok("add_admin")
        );
        assert_eq!(
            helper().resolve_subcommand_name("group", "a"),
            Err(String::from(
                "Ambiguous command 'group a', could be: add, add_admin"
            ))
        );
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

    /// Complete `line` with the cursor at its end, giving where the replacement starts and what
    /// each candidate would replace the word with.
    fn complete(helper: &BofhHelper, line: &str) -> (usize, Vec<String>) {
        let history = MemHistory::new();
        let (start, candidates) = helper
            .complete(line, line.len(), &Context::new(&history))
            .expect("completion should not fail");
        (
            start,
            candidates
                .into_iter()
                .map(|candidate| candidate.replacement)
                .collect(),
        )
    }

    /// Hint `line` with the cursor at its end.
    fn hint(helper: &BofhHelper, line: &str) -> Option<String> {
        let history = MemHistory::new();
        helper.hint(line, line.len(), &Context::new(&history))
    }

    /// `help` is followed by the command it explains, so that command is coloured as one, rather
    /// than as a subcommand of `help` — which has none, so it used to be red whatever it was.
    #[test]
    fn the_command_help_is_asked_about_is_highlighted_as_a_command() {
        assert_eq!(
            helper().highlight("help pers", 9),
            format!(
                "{} {}",
                colorize_command(&["help"], "help"),
                colorize_command(&["person"], "pers")
            )
        );
        assert_eq!(
            helper().highlight("he person fi", 12),
            format!(
                "{} {} {}",
                colorize_command(&["help"], "he"),
                colorize_command(&["person"], "person"),
                colorize_command(&["find"], "fi")
            )
        );
        assert_eq!(
            helper().highlight("help person zz", 14),
            format!(
                "{} {} {}",
                colorize_command(&["help"], "help"),
                colorize_command(&["person"], "person"),
                colorize_command(&[], "zz")
            )
        );
        for line in [
            "help",
            "help ",
            "  help   person  find  ",
            "help arg_help foo",
        ] {
            assert_eq!(strip_colors(&helper().highlight(line, line.len())), line);
        }
        // Argument help is asked for by a reference, not a command, so none of it is coloured
        // as a command that doesn't exist
        assert_eq!(
            helper().highlight("help arg_help foo", 17),
            format!("{} arg_help foo", colorize_command(&["help"], "help"))
        );
    }

    /// What follows the client's other commands is an argument, which can't be wrong the way a
    /// subcommand can, so it isn't coloured as though it were one.
    #[test]
    fn an_argument_to_an_internal_command_is_not_coloured_as_a_subcommand() {
        assert_eq!(
            helper().highlight("source ~/commands", 17),
            format!("{} ~/commands", colorize_command(&["source"], "source"))
        );
        // A server command's second word still is a subcommand
        assert_eq!(
            helper().highlight("user zz", 7),
            format!(
                "{} {}",
                colorize_command(&["user"], "user"),
                colorize_command(&[], "zz")
            )
        );
    }

    #[test]
    fn the_command_help_is_asked_about_is_completed_as_a_command() {
        assert_eq!(
            complete(&helper(), "help pers"),
            (5, vec![String::from("person ")])
        );
        assert_eq!(
            complete(&helper(), "help person f"),
            (12, vec![String::from("find ")])
        );
        assert_eq!(
            complete(&helper(), "help "),
            (
                5,
                [
                    "commands", "group", "help", "history", "logout", "person", "quit", "script",
                    "source", "user"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        // `help` itself is still a command being typed until there is a space after it
        assert_eq!(complete(&helper(), "hel"), (0, vec![String::from("help ")]));
    }

    /// `help` is followed by a command name and nothing more, so the name is hinted but the
    /// arguments that command would take are not.
    #[test]
    fn help_hints_the_command_name_but_not_its_arguments() {
        let mut helper = helper();
        helper
            .commands
            .get_mut("user")
            .and_then(|group| group.commands.get_mut("info"))
            .expect("the fixture has user info")
            .args = vec![Argument {
            arg_type: Some(String::from("accountName")),
            ..Argument::default()
        }];

        assert_eq!(hint(&helper, "help pers"), Some(String::from("on")));
        assert_eq!(hint(&helper, "help person fi"), Some(String::from("nd")));
        assert_eq!(
            hint(&helper, "user info "),
            Some(String::from("accountName"))
        );
        assert_eq!(hint(&helper, "help user info "), None);
    }
}
