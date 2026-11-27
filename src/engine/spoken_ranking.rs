//! Context diversity of multi-character words in the SUBTLEX-CH subtitles.

const WORD_DIVERSITY: &[u8] = include_bytes!("../../data/spoken_ranking/word-diversity.bin");
pub(super) const MAX_BONUS: u32 = 600;

fn offset(group: usize) -> usize {
    let start = group * 4;
    u32::from_le_bytes(WORD_DIVERSITY[start..start + 4].try_into().unwrap()) as usize
}

fn diversity(text: &str) -> Option<u32> {
    // The generated table contains only two to eight basic Han characters.
    // Comparing bytes to the complete key also rejects every other string.
    let width = text.len();
    if !(6..=24).contains(&width) || width % 3 != 0 {
        return None;
    }
    let group = width / 3 - 2;
    let start = offset(group);
    let stride = width + 2;
    let mut left = 0;
    let mut right = (offset(group + 1) - start) / stride;
    while left < right {
        let mid = left + (right - left) / 2;
        let position = start + mid * stride;
        match WORD_DIVERSITY[position..position + width].cmp(text.as_bytes()) {
            std::cmp::Ordering::Less => left = mid + 1,
            std::cmp::Ordering::Greater => right = mid,
            std::cmp::Ordering::Equal => {
                return Some(u32::from(u16::from_le_bytes([
                    WORD_DIVERSITY[position + width],
                    WORD_DIVERSITY[position + width + 1],
                ])))
            }
        }
    }
    None
}

pub(super) fn bonus(text: &str) -> i64 {
    let Some(count) = diversity(text) else {
        return 0;
    };
    let logarithm = count.ilog2();
    let bucket = 1 << logarithm;
    // Preserve variation inside each power-of-two band. Cap the signal so
    // corpus counts do not turn into absolute ranks or discard other matches.
    i64::from((64 * logarithm + 64 * (count - bucket) / bucket).min(MAX_BONUS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_diversity_matches_published_counts_without_single_character_priors() {
        assert_eq!(diversity("刚才"), Some(3161));
        assert_eq!(diversity("钢材"), Some(7));
        assert_eq!(diversity("远远"), Some(524));
        assert_eq!(diversity("袁媛"), None);
        assert_eq!(bonus("刚才"), 600);
        assert_eq!(bonus("钢材"), 176);
        for text in ["的", "", "sample", "我🙂", "一一一一一一一一一"] {
            assert_eq!(bonus(text), 0, "{text}");
        }
    }
}
