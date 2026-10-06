//! Lexing of bofh command lines.

/// Split a command line into words.
///
/// Words are separated by whitespace, unless the whitespace is quoted or escaped. A double quote
/// starts and ends a quoted section, and a backslash makes the next character literal, including a
/// quote or another backslash. Quoting groups without separating, the way a shell does, so
/// `foo" "bar` is the single word `foo bar`, and `""` is an empty word.
///
/// # Errors
///
/// Returns a message meant for the user if the line ends inside a quoted section, or on a
/// backslash with nothing left to escape.
pub(crate) fn split_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // A word can be empty and still be a word, if the user wrote `""`, so we can't just ask whether
    // `word` has anything in it
    let mut in_word = false;
    let mut in_quotes = false;
    let mut chars = line.chars();

    while let Some(char) = chars.next() {
        match char {
            '\\' => {
                let Some(escaped) = chars.next() else {
                    return Err(String::from("Trailing backslash"));
                };
                word.push(escaped);
                in_word = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                in_word = true;
            }
            char if char.is_whitespace() && !in_quotes => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            char => {
                word.push(char);
                in_word = true;
            }
        }
    }

    if in_quotes {
        return Err(String::from("Unbalanced quotes"));
    }
    if in_word {
        words.push(word);
    }

    Ok(words)
}

/// Join words back into a command line that [`split_words`] would split the same way again.
///
/// Used when only part of a line may be kept, so that what is kept is still a line the user can
/// recall and run. Everything that isn't plain gets quoted, whether or not it had to be: this is
/// a line to be read back by the lexer, not a reconstruction of what was typed.
pub(crate) fn requote(words: &[String]) -> String {
    words
        .iter()
        .map(|word| {
            if !word.is_empty()
                && !word
                    .chars()
                    .any(|char| char.is_whitespace() || char == '"' || char == '\\')
            {
                return word.clone();
            }
            format!(r#""{}""#, word.replace('\\', r"\\").replace('"', r#"\""#))
        })
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{requote, split_words};

    fn words(line: &str) -> Vec<String> {
        split_words(line).unwrap()
    }

    #[test]
    fn words_are_split_on_whitespace() {
        assert_eq!(words(""), Vec::<String>::new());
        assert_eq!(words("   "), Vec::<String>::new());
        assert_eq!(words("user info foo"), ["user", "info", "foo"]);
        assert_eq!(words("  user\tinfo \n foo  "), ["user", "info", "foo"]);
    }

    #[test]
    fn quotes_group_without_separating() {
        assert_eq!(words(r#"user info "foo bar""#), ["user", "info", "foo bar"]);
        assert_eq!(words(r#"foo" "bar"#), ["foo bar"]);
        assert_eq!(words(r#""foo"bar"#), ["foobar"]);
        assert_eq!(words(r#"a "" b"#), ["a", "", "b"]);
        assert_eq!(words(r#""""#), [""]);
    }

    #[test]
    fn backslash_escapes_the_next_character() {
        assert_eq!(words(r"foo\ bar"), ["foo bar"]);
        assert_eq!(words(r#"foo\"bar"#), [r#"foo"bar"#]);
        assert_eq!(words(r"foo\\bar"), [r"foo\bar"]);
        assert_eq!(words(r#""foo\"bar""#), [r#"foo"bar"#]);
        // A backslash escapes whitespace even inside quotes, where it isn't needed
        assert_eq!(words(r#""foo\ bar""#), ["foo bar"]);
    }

    #[test]
    fn requoting_words_gives_a_line_that_lexes_back_into_them() {
        for line in [
            "user info foo",
            r#"user info "foo bar""#,
            r#"user info "foo\"bar""#,
            r#"user info "foo\\bar""#,
            r#"a "" b"#,
        ] {
            let lexed = words(line);
            assert_eq!(words(&requote(&lexed)), lexed, "{line}");
        }
    }

    #[test]
    fn requoting_leaves_ordinary_words_alone() {
        assert_eq!(requote(&words("user info foo")), "user info foo");
        assert_eq!(requote(&words(r#""foo"bar"#)), "foobar");
    }

    #[test]
    fn unterminated_quotes_and_backslashes_are_errors() {
        assert!(split_words(r#"user info "foo"#).is_err());
        assert!(split_words(r"user info foo\").is_err());
        // An escaped quote doesn't open a quoted section
        assert!(split_words(r#"user info \""#).is_ok());
    }
}
