//! Rendering of bofhd responses according to the server's format suggestions.
//!
//! Not every command has a format suggestion. A command either returns a pre-formatted string
//! (handled by [`render_plain`]) or it returns a struct (or a list of structs) together with
//! a suggestion on how to format it (handled by [`FormatSuggestion::format`]).

use std::fmt::Write as _;
use xmlrpc::Value;

/// How a field value should be rendered, from the `<name>:<type>:<params>` field reference syntax.
///
/// `date` is the only currently supported type in bofhd.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldType {
    /// A date, with a [`java.text.SimpleDateFormat`][sdf] pattern.
    ///
    /// [sdf]: https://docs.oracle.com/javase/8/docs/api/java/text/SimpleDateFormat.html
    Date(String),
}

/// A reference to one field of a bofhd response.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldRef {
    name: String,
    field_type: Option<FieldType>,
}

impl FieldRef {
    /// Parse a field reference, which is either `<name>` or `<name>:<type>:<params>`.
    fn parse(reference: &str) -> Self {
        let parts: Vec<&str> = reference.splitn(3, ':').collect();
        if let [name, "date", pattern] = parts[..] {
            Self {
                name: name.to_owned(),
                field_type: Some(FieldType::Date(pattern.to_owned())),
            }
        } else {
            Self {
                name: reference.to_owned(),
                field_type: None,
            }
        }
    }

    /// Render this field of `entry`, or `None` if the entry doesn't have it.
    fn render(&self, entry: &Value) -> Option<String> {
        let value = entry.get(self.name.as_str())?;
        Some(match (&self.field_type, value) {
            (Some(FieldType::Date(pattern)), Value::DateTime(datetime)) => {
                format_datetime(*datetime, pattern)
            }
            _ => render_value(value),
        })
    }
}

/// One line of a format suggestion: a format string, the fields to interpolate into it, and an
/// optional header printed above it.
#[derive(Debug, Clone)]
struct FormatItem {
    format: String,
    fields: Vec<FieldRef>,
    header: Option<String>,
}

impl FormatItem {
    /// Parse one `(format, fields)` or `(format, fields, header)` row of `str_vars`.
    fn parse(row: &Value) -> Option<Self> {
        let row = row.as_array()?;
        let mut format = row.first()?.as_str()?.to_owned();
        let fields = match row.get(1) {
            // The field list is allowed to be absent or nil, for a row with no interpolation
            None | Some(Value::Nil) => Vec::new(),
            Some(fields) => fields
                .as_array()?
                .iter()
                .map(|field| Some(FieldRef::parse(field.as_str()?)))
                .collect::<Option<_>>()?,
        };
        let mut header = row.get(2).and_then(Value::as_str).map(ToOwned::to_owned);

        // TODO: Apparently, some format suggestions in the past have swapped the header and the format
        // string. This code will probably never execute, but pybofh and jbofh does the same check
        if header.as_ref().is_some_and(|header| header.contains('%')) {
            format = header.take().expect("just checked that the header is set");
        }

        Some(Self {
            format,
            fields,
            header,
        })
    }

    /// Render `entry` with this item, or `None` if it doesn't apply.
    ///
    /// An entry that's missing any of the referenced fields is skipped, rather than rendered with
    /// a blank in its place.
    fn render(&self, entry: &Value) -> Option<String> {
        let values: Vec<String> = self
            .fields
            .iter()
            .map(|field| field.render(entry))
            .collect::<Option<_>>()?;
        apply_format(&self.format, &values)
    }
}

/// A bofhd server's suggestion for how to format the response to a command.
#[derive(Debug, Clone)]
pub struct FormatSuggestion {
    header: Option<String>,
    items: Vec<FormatItem>,
}

impl FormatSuggestion {
    /// Parse the response to a `get_format_suggestion` call.
    ///
    /// Returns `None` if the command has no format suggestion (the server sends an empty string),
    /// or if the suggestion is malformed, in which case the caller should fall back to
    /// [`render_plain`] rather than show the user nothing.
    #[must_use]
    pub fn parse(suggestion: &Value) -> Option<Self> {
        let suggestion = suggestion.as_struct()?;
        let header = suggestion
            .get("hdr")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let items = match suggestion.get("str_vars")? {
            // A bare string is printed as-is, with nothing interpolated into it
            Value::String(string) => vec![FormatItem {
                format: string.clone(),
                fields: Vec::new(),
                header: None,
            }],
            Value::Array(rows) => rows.iter().map(FormatItem::parse).collect::<Option<_>>()?,
            _ => return None,
        };

        Some(Self { header, items })
    }

    /// Render a command response.
    ///
    /// A response is a list of entries, or a single entry, which are usually structs but can also
    /// be strings that are passed through untouched.
    #[must_use]
    pub fn format(&self, response: &Value) -> String {
        let entries: Vec<&Value> = match response {
            Value::Array(entries) => entries.iter().collect(),
            entry => vec![entry],
        };

        let mut lines: Vec<String> = self.header.iter().cloned().collect();

        for item in &self.items {
            lines.extend(item.header.iter().cloned());

            // A suggestion that interpolates nothing describes the whole response, not each entry
            // in it, so it's printed once. Note that this follows bofhd's "reference spec", but
            // pybofh prints it once per entry instead... TODO
            if item.fields.is_empty() {
                lines.extend(apply_format(&item.format, &[]));
                continue;
            }

            for entry in &entries {
                if let Value::String(string) = entry {
                    lines.push(string.clone());
                } else {
                    lines.extend(item.render(entry));
                }
            }
        }

        lines.join("\n")
    }
}

/// Render a response from a command that has no format suggestion.
#[must_use]
pub fn render_plain(response: &Value) -> String {
    match response {
        Value::String(string) => string.clone(),
        response => format!("{response:?}"),
    }
}

/// Render a single field value for interpolation into a format suggestion.
fn render_value(value: &Value) -> String {
    match value {
        Value::Nil => String::from("<not set>"),
        Value::String(string) => string.clone(),
        Value::Int(int) => int.to_string(),
        Value::Int64(int) => int.to_string(),
        Value::Double(double) => double.to_string(),
        Value::Bool(bool) => bool.to_string(),
        Value::DateTime(datetime) => format_datetime(*datetime, "yyyy-MM-dd HH:mm:ss"),
        Value::Base64(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        value => format!("{value:?}"),
    }
}

/// Format a date with the subset of [`java.text.SimpleDateFormat`][sdf] that bofhd uses.
///
/// [sdf]: https://docs.oracle.com/javase/8/docs/api/java/text/SimpleDateFormat.html
fn format_datetime(datetime: iso8601::DateTime, pattern: &str) -> String {
    let (year, month, day) = match datetime.date {
        iso8601::Date::YMD { year, month, day } => (year, month, day),
        // bofhd only ever sends calendar dates, so anything else is rendered as the epoch rather
        // than by pulling in a full calendar implementation to convert it
        _ => (0, 0, 0),
    };
    let time = datetime.time;

    let mut formatted = pattern.to_owned();
    for (from, to) in [
        ("yyyy", format!("{year:04}")),
        ("MM", format!("{month:02}")),
        ("dd", format!("{day:02}")),
        ("HH", format!("{:02}", time.hour)),
        ("mm", format!("{:02}", time.minute)),
        ("ss", format!("{:02}", time.second)),
    ] {
        formatted = formatted.replace(from, &to);
    }
    formatted
}

/// Interpolate `values` into a printf-style format string.
///
/// bofhd only uses `%s`, `%d` and `%i`, each with an optional `-` flag and field width, plus `%%` —
/// the sole exception being `debug bytes`, a dev-only command that uses `%r`.
///
/// Returns `None` for anything outside that grammar, or if the suggestion asks for more values than
/// the entry has, so the caller can fall back.
fn apply_format(format: &str, values: &[String]) -> Option<String> {
    let mut formatted = String::with_capacity(format.len());
    let mut values = values.iter();
    let mut chars = format.chars().peekable();

    while let Some(char) = chars.next() {
        if char != '%' {
            formatted.push(char);
            continue;
        }
        if chars.next_if_eq(&'%').is_some() {
            formatted.push('%');
            continue;
        }

        let left_align = chars.next_if_eq(&'-').is_some();
        let mut width = String::new();
        while let Some(digit) = chars.next_if(char::is_ascii_digit) {
            width.push(digit);
        }
        if !matches!(chars.next(), Some('s' | 'd' | 'i')) {
            return None;
        }

        let value = values.next()?;
        let width: usize = width.parse().unwrap_or(0);
        let padding = width.saturating_sub(value.chars().count());
        if left_align {
            let _ = write!(formatted, "{value}{:padding$}", "");
        } else {
            let _ = write!(formatted, "{:padding$}{value}", "");
        }
    }

    Some(formatted)
}

#[cfg(test)]
mod tests {
    use super::{FormatSuggestion, apply_format, format_datetime, render_plain, render_value};
    use xmlrpc::Value;

    /// Build the `{hdr, str_vars}` struct a bofhd server would send.
    fn suggestion(header: Option<&str>, str_vars: Value) -> FormatSuggestion {
        let mut strct = vec![(String::from("str_vars"), str_vars)];
        if let Some(header) = header {
            strct.push((String::from("hdr"), Value::from(header)));
        }
        FormatSuggestion::parse(&Value::Struct(strct.into_iter().collect()))
            .expect("the suggestion should parse")
    }

    /// Build one `(format, fields)` row of `str_vars`.
    fn row(format: &str, fields: &[&str]) -> Value {
        Value::Array(vec![
            Value::from(format),
            Value::Array(fields.iter().map(|&f| Value::from(f)).collect()),
        ])
    }

    /// Build a `str_vars` list holding a single row. `str_vars` is always a list of rows, or a
    /// bare string; never a single unwrapped row.
    fn one_row(format: &str, fields: &[&str]) -> Value {
        Value::Array(vec![row(format, fields)])
    }

    /// Build one response entry.
    fn entry(fields: &[(&str, Value)]) -> Value {
        Value::Struct(
            fields
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        )
    }

    #[test]
    fn printf_subset_matches_python() {
        let values = [String::from("ab")];
        assert_eq!(apply_format("%s", &values).unwrap(), "ab");
        assert_eq!(apply_format("%4s", &values).unwrap(), "  ab");
        assert_eq!(apply_format("%-4s", &values).unwrap(), "ab  ");
        assert_eq!(apply_format("%1s", &values).unwrap(), "ab");
        assert_eq!(apply_format("%d", &values).unwrap(), "ab");
        assert_eq!(apply_format("%-4i|", &values).unwrap(), "ab  |");
        assert_eq!(apply_format("100%% of %s", &values).unwrap(), "100% of ab");
        assert_eq!(apply_format("no specifiers", &[]).unwrap(), "no specifiers");
        // Padding counts characters, not bytes
        assert_eq!(
            apply_format("%-4s|", &[String::from("æø")]).unwrap(),
            "æø  |"
        );
    }

    #[test]
    fn unsupported_formats_are_rejected_rather_than_guessed_at() {
        // bofhd never sends these, so seeing one means we've misread the suggestion
        assert!(apply_format("%f", &[String::from("1")]).is_none());
        assert!(apply_format("%.2s", &[String::from("1")]).is_none());
        assert!(apply_format("%r", &[String::from("1")]).is_none());
        // More specifiers than the entry has values
        assert!(apply_format("%s %s", &[String::from("1")]).is_none());
    }

    #[test]
    fn a_bare_string_suggestion_is_printed_once() {
        let suggestion = suggestion(None, Value::from("Done"));
        assert_eq!(suggestion.format(&entry(&[])), "Done");
        // Even when the response has several entries, unlike pybofh
        assert_eq!(
            suggestion.format(&Value::Array(vec![entry(&[]), entry(&[])])),
            "Done"
        );
    }

    #[test]
    fn headers_come_before_the_rows() {
        let suggestion = suggestion(Some("Name   Uid"), one_row("%-6s %s", &["name", "uid"]));
        let response = Value::Array(vec![
            entry(&[("name", Value::from("foo")), ("uid", Value::Int(1))]),
            entry(&[("name", Value::from("barbaz")), ("uid", Value::Int(22))]),
        ]);
        assert_eq!(
            suggestion.format(&response),
            "Name   Uid\nfoo    1\nbarbaz 22"
        );
    }

    #[test]
    fn rows_are_grouped_by_format_not_by_entry() {
        // Both reference implementations loop over formats on the outside
        let suggestion = suggestion(
            None,
            Value::Array(vec![row("a=%s", &["a"]), row("b=%s", &["b"])]),
        );
        let response = Value::Array(vec![
            entry(&[("a", Value::from("1")), ("b", Value::from("2"))]),
            entry(&[("a", Value::from("3")), ("b", Value::from("4"))]),
        ]);
        assert_eq!(suggestion.format(&response), "a=1\na=3\nb=2\nb=4");
    }

    #[test]
    fn an_entry_missing_a_field_is_skipped() {
        let suggestion = suggestion(None, one_row("%s %s", &["a", "b"]));
        let response = Value::Array(vec![
            entry(&[("a", Value::from("1")), ("b", Value::from("2"))]),
            // Missing "b" entirely: skipped, rather than rendered with a blank
            entry(&[("a", Value::from("3"))]),
        ]);
        assert_eq!(suggestion.format(&response), "1 2");
    }

    #[test]
    fn a_field_that_is_set_to_nil_renders_as_not_set() {
        // A present-but-nil field is not the same as a missing one
        let suggestion = suggestion(None, one_row("expires: %s", &["expire_date"]));
        assert_eq!(
            suggestion.format(&entry(&[("expire_date", Value::Nil)])),
            "expires: <not set>"
        );
    }

    #[test]
    fn strings_in_a_response_are_passed_through() {
        let suggestion = suggestion(None, one_row("a=%s", &["a"]));
        let response = Value::Array(vec![
            entry(&[("a", Value::from("1"))]),
            Value::from("a plain message"),
        ]);
        assert_eq!(suggestion.format(&response), "a=1\na plain message");
    }

    #[test]
    fn a_sub_header_containing_a_format_specifier_is_swapped_back() {
        // Bug-compatible with both reference implementations
        let swapped = Value::Array(vec![
            Value::from("Affiliations"),
            Value::Array(vec![Value::from("aff")]),
            Value::from("  %s"),
        ]);
        let suggestion = suggestion(None, Value::Array(vec![swapped]));
        assert_eq!(
            suggestion.format(&entry(&[("aff", Value::from("STUDENT"))])),
            "  STUDENT"
        );
    }

    #[test]
    fn sub_headers_are_printed_above_their_rows() {
        let with_header = Value::Array(vec![
            Value::from("%s"),
            Value::Array(vec![Value::from("aff")]),
            Value::from("Affiliations:"),
        ]);
        let suggestion = suggestion(None, Value::Array(vec![with_header]));
        assert_eq!(
            suggestion.format(&entry(&[("aff", Value::from("STUDENT"))])),
            "Affiliations:\nSTUDENT"
        );
    }

    #[test]
    fn dates_use_the_simple_date_format_subset() {
        let datetime: iso8601::DateTime = "2024-01-05T09:07:03+00:00".parse().unwrap();
        assert_eq!(format_datetime(datetime, "yyyy-MM-dd"), "2024-01-05");
        assert_eq!(
            format_datetime(datetime, "yyyy-MM-dd HH:mm:ss"),
            "2024-01-05 09:07:03"
        );
        // MM is the month and mm the minute, so the order of substitution matters
        assert_eq!(format_datetime(datetime, "MM/mm"), "01/07");
    }

    #[test]
    fn a_typed_date_field_is_formatted_with_its_pattern() {
        let datetime: iso8601::DateTime = "2024-01-05T09:07:03+00:00".parse().unwrap();
        let suggestion = suggestion(None, one_row("%s", &["when:date:yyyy-MM-dd"]));
        assert_eq!(
            suggestion.format(&entry(&[("when", Value::DateTime(datetime))])),
            "2024-01-05"
        );
    }

    #[test]
    fn a_field_name_with_one_colon_is_not_a_typed_reference() {
        // "id:target" splits into two parts, so pybofh treats the whole thing as the field name.
        // The server's reference spec would key on "id" instead and drop the row; see FieldRef.
        let suggestion = suggestion(None, one_row("%s", &["id:target"]));
        assert_eq!(
            suggestion.format(&entry(&[("id:target", Value::from("account:foo"))])),
            "account:foo"
        );
    }

    #[test]
    fn an_unknown_field_type_skips_the_entry() {
        // `date` is the only field type either reference implementation defines, so a reference
        // like this can only come from a future server. We drop the entry, as the server's own
        // reference spec does; pybofh raises instead and loses the whole response.
        let suggestion = suggestion(None, one_row("%s", &["cost:money:NOK"]));
        assert_eq!(suggestion.format(&entry(&[("cost", Value::from(10))])), "");
    }

    #[test]
    fn values_are_rendered_the_way_rust_would() {
        // pybofh would print "True" and "1.0" here, since it leaves the conversion to Python
        assert_eq!(render_value(&Value::Bool(true)), "true");
        assert_eq!(render_value(&Value::Double(1.0)), "1");
        assert_eq!(render_value(&Value::Int(42)), "42");
    }

    #[test]
    fn binary_fields_are_decoded_as_text() {
        assert_eq!(render_value(&Value::Base64(b"ab\xc3\xa6".to_vec())), "abæ");
        // A bad byte costs us that byte, not the response
        assert_eq!(render_value(&Value::Base64(b"a\xffb".to_vec())), "a\u{fffd}b");
    }

    #[test]
    fn commands_without_a_suggestion_fall_back_to_the_raw_response() {
        assert!(FormatSuggestion::parse(&Value::from("")).is_none());
        assert_eq!(
            render_plain(&Value::from("already formatted")),
            "already formatted"
        );
    }
}
