//! Explicit syllable boundaries; ASCII byte offsets are also raw input offsets.
#[cfg(test)]
pub(crate) fn valid_reading(reading: &str) -> bool {
    !reading.is_empty()
        && reading.len() <= 256
        && reading
            .split('\'')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
}

pub(crate) fn boundaries_match(raw: &str, canonical: &str) -> bool {
    if raw.is_empty() || raw.starts_with('\'') || raw.contains("''") {
        return false;
    }
    let mut canonical_boundaries = Vec::new();
    let mut count = 0;
    for byte in canonical.bytes() {
        if byte == b'\'' {
            canonical_boundaries.push(count);
        } else {
            count += 1;
        }
    }
    let total = count;
    count = 0;
    for byte in raw.bytes() {
        if byte == b'\'' {
            if count != total && !canonical_boundaries.contains(&count) {
                return false;
            }
        } else {
            count += 1;
        }
    }
    raw.replace('\'', "") == canonical.replace('\'', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_boundary_never_turns_xian_into_xi_an() {
        assert!(!boundaries_match("xi'an", "xian"));
        assert!(boundaries_match("xi'an", "xi'an"));
        assert!(boundaries_match("xian", "xi'an"));
        assert!(boundaries_match("xi'", "xi"));
        assert!(!valid_reading("ni''hao"));
    }
}
