//! Shared, std-only background spelling seed format.

pub fn eligible(reading: &str, text: &str) -> bool {
    let characters = text.chars().count();
    (1..=64).contains(&characters)
        && text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        && reading.split('\'').count() == characters
        && reading.split('\'').all(|s| {
            !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_lowercase())
                && s.bytes().any(|b| b"aeiouv".contains(&b))
        })
}

/// Preserve every eligible input record, its order and its original weight.
pub fn encode<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str, u32)>) -> Vec<u8> {
    use std::io::Write;
    let mut bytes = Vec::new();
    for (reading, text, weight) in rows {
        if eligible(reading, text) {
            writeln!(bytes, "{reading}\t{text}\t{weight}").expect("writing to Vec");
        }
    }
    bytes
}

/// Decode one LF-stripped row without allocating or accepting extra fields.
pub fn parse_row(row: &str) -> Option<(&str, &str, u32)> {
    let mut fields = row.split('\t');
    let reading = fields.next()?;
    let text = fields.next()?;
    let weight = fields.next()?;
    if fields.next().is_some()
        || weight.is_empty()
        || !weight.bytes().all(|b| b.is_ascii_digit())
        || !eligible(reading, text)
    {
        return None;
    }
    let weight = weight.parse::<u32>().ok().filter(|&w| w > 0)?;
    Some((reading, text, weight))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligibility_matches_original_filter() {
        for (reading, text) in [("ni'hao", "你好"), ("v", "女"), ("a", "一")] {
            assert!(eligible(reading, text));
        }
        for (reading, text) in [
            ("", ""),
            ("ni", "你好"),
            ("Ni", "你"),
            ("n", "你"),
            ("ni'", "你好"),
            ("ni", "A"),
            ("a", "㐀"),
            ("a", "𠀀"),
        ] {
            assert!(!eligible(reading, text));
        }
        assert!(eligible(&vec!["a"; 64].join("'"), &"一".repeat(64)));
        assert!(!eligible(&vec!["a"; 65].join("'"), &"一".repeat(65)));
    }

    #[test]
    fn encoding_keeps_order_duplicates_and_weights() {
        let rows = [("ni", "你", 1), ("ni", "你", u32::MAX), ("n", "你", 2)];
        let bytes = encode(rows);
        assert_eq!(bytes, "ni\t你\t1\nni\t你\t4294967295\n".as_bytes());
        assert_eq!(
            parse_row("ni\t你\t4294967295"),
            Some(("ni", "你", u32::MAX))
        );
    }

    #[test]
    fn malformed_rows_are_rejected() {
        for row in [
            "",
            "ni\t你",
            "ni\t你\t1\textra",
            "ni\t你\t0",
            "ni\t你\t-1",
            "ni\t你\t+1",
            "ni\t你\t4294967296",
            "ni\t你\t1\r",
            "n\t你\t1",
            "ni\tA\t1",
        ] {
            assert_eq!(parse_row(row), None, "{row:?}");
        }
    }
}
