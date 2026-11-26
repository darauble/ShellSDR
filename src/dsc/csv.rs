//! The CSV subset of seed spec/05-data (RFC 4180): comma-separated,
//! `"…"` quoting with `""` for a quote, CR ignored, empty lines skipped.

/// All non-empty records of a CSV text, each a list of fields.
pub fn parse(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let end_row = |row: &mut Vec<String>, field: &mut String, rows: &mut Vec<Vec<String>>| {
        row.push(std::mem::take(field));
        let r = std::mem::take(row);
        if !(r.len() == 1 && r[0].is_empty()) {
            rows.push(r);
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\r' => {}
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\n' if !quoted => end_row(&mut row, &mut field, &mut rows),
            c => field.push(c),
        }
    }
    end_row(&mut row, &mut field, &mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn rows(t: &str) -> Vec<Vec<String>> {
        parse(t)
    }

    #[test]
    fn plain_fields_and_lines() {
        assert_eq!(
            rows("mid,allocated_to\n201,Albania\n"),
            [["mid", "allocated_to"], ["201", "Albania"]]
        );
    }

    #[test]
    fn quoted_field_with_comma_and_doubled_quote() {
        assert_eq!(
            rows("306,\"Bonaire, Saba\"\n1,\"say \"\"hi\"\"\"\n"),
            [["306", "Bonaire, Saba"], ["1", "say \"hi\""]]
        );
    }

    #[test]
    fn cr_ignored_empty_lines_skipped_last_line_without_newline() {
        assert_eq!(rows("a,b\r\n\r\n\nc,d"), [["a", "b"], ["c", "d"]]);
    }

    #[test]
    fn empty_fields_kept() {
        assert_eq!(
            rows("258999119,,,Norway,,62.466667\n"),
            [["258999119", "", "", "Norway", "", "62.466667"]]
        );
    }

    #[test]
    fn unterminated_quote_never_panics() {
        assert_eq!(rows("1,\"abc"), [["1", "abc"]]);
    }
}
