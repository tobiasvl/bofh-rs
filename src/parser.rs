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

#[cfg(test)]
mod tests {
    use super::split_words;

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
    fn unterminated_quotes_and_backslashes_are_errors() {
        assert!(split_words(r#"user info "foo"#).is_err());
        assert!(split_words(r"user info foo\").is_err());
        // An escaped quote doesn't open a quoted section
        assert!(split_words(r#"user info \""#).is_ok());
    }
}
