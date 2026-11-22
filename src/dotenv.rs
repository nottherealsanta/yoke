//! Minimal `.env` parsing: `KEY=VALUE` lines, nothing more.
//!
//! The parser is pure; `main` applies the result to the process environment
//! once at startup, and only for keys that are not already set.

/// Parse `KEY=VALUE` lines. Whole-line `#` comments, blank lines, an optional
/// `export ` prefix and optional single or double quotes around the value are
/// accepted. Lines without `=` or a key are skipped.
pub fn parse(contents: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2 {
            let first = value.as_bytes()[0];
            if (first == b'"' || first == b'\'') && value.as_bytes()[value.len() - 1] == first {
                value = &value[1..value.len() - 1];
            }
        }
        pairs.push((key.to_string(), value.to_string()));
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn reads_keys_values_comments_export_and_quotes() {
        let text = r#"# a comment
OPENCODE_API_KEY=secret

export OTHER = 'quoted value'
  SPACED="double quoted"
EMPTY=
WITH_EQUALS=a=b
not a pair
=novalue
"#;
        assert_eq!(
            parse(text),
            [
                ("OPENCODE_API_KEY".to_string(), "secret".to_string()),
                ("OTHER".to_string(), "quoted value".to_string()),
                ("SPACED".to_string(), "double quoted".to_string()),
                ("EMPTY".to_string(), String::new()),
                ("WITH_EQUALS".to_string(), "a=b".to_string()),
            ]
        );
    }

    #[test]
    fn inline_hashes_stay_in_the_value() {
        assert_eq!(
            parse("TOKEN=abc#def\n#TOKEN=hidden\n"),
            [("TOKEN".to_string(), "abc#def".to_string())]
        );
    }
}
