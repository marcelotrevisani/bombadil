use std::collections::BTreeMap;

/// Parse the output of a NUL-separated environment dump (`env -0`, or the
/// PowerShell equivalent).
///
/// Records are separated by NUL and each splits on its **first** `=`. Values
/// may contain spaces, further `=` signs and newlines — verified against a
/// real `env -0`, where a multiline value survives as `line1\nline2`. This is
/// precisely why the dump is NUL-separated rather than line-separated.
///
/// Records that are not valid UTF-8, or that carry no `=`, are dropped: one
/// undecodable variable must not cost the caller the whole environment.
pub fn parse_dump(bytes: &[u8]) -> BTreeMap<String, String> {
    bytes
        .split(|b| *b == 0)
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let text = std::str::from_utf8(record).ok()?;
            let (key, value) = text.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_records_on_nul_and_keys_on_the_first_equals() {
        let raw = b"SIMPLE=one\0EQUALS=k=v\0SPACED=two words\0";
        let got = parse_dump(raw);
        assert_eq!(got.get("SIMPLE").map(String::as_str), Some("one"));
        assert_eq!(
            got.get("EQUALS").map(String::as_str),
            Some("k=v"),
            "splitting on the last '=' would give 'k' and lose the rest"
        );
        assert_eq!(got.get("SPACED").map(String::as_str), Some("two words"));
    }

    #[test]
    fn a_multiline_value_survives_intact() {
        // Verified against a real `env -0`: this is exactly why the dump is
        // NUL-separated. Splitting on newlines would truncate it to "line1".
        let raw = b"MULTI=line1\nline2\0OTHER=x\0";
        let got = parse_dump(raw);
        assert_eq!(got.get("MULTI").map(String::as_str), Some("line1\nline2"));
        assert_eq!(got.get("OTHER").map(String::as_str), Some("x"));
    }

    #[test]
    fn an_empty_value_is_kept_not_dropped() {
        // An exported-but-empty variable is meaningfully different from an
        // unset one, and some tools rely on the distinction.
        let got = parse_dump(b"EMPTY=\0");
        assert_eq!(got.get("EMPTY").map(String::as_str), Some(""));
    }

    #[test]
    fn records_without_an_equals_are_skipped() {
        let got = parse_dump(b"NOEQUALS\0GOOD=yes\0");
        assert!(!got.contains_key("NOEQUALS"));
        assert_eq!(got.get("GOOD").map(String::as_str), Some("yes"));
    }

    #[test]
    fn a_trailing_separator_does_not_produce_an_empty_entry() {
        let got = parse_dump(b"A=1\0");
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn an_empty_dump_is_an_empty_map() {
        assert!(parse_dump(b"").is_empty());
    }

    #[test]
    fn undecodable_records_are_dropped_and_the_rest_survive() {
        // One bad variable must not cost us the whole environment.
        let mut raw = b"GOOD=yes\0BAD=".to_vec();
        raw.push(0xff);
        raw.push(0);
        let got = parse_dump(&raw);
        assert_eq!(got.get("GOOD").map(String::as_str), Some("yes"));
        assert!(!got.contains_key("BAD"));
    }
}
