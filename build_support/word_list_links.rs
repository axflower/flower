//! Bounded adjacent-Han evidence from distinct dictionary words, not a corpus.
use std::collections::{BTreeSet, HashMap, HashSet};

pub fn select(tsv: &str, capacity: usize) -> Vec<u64> {
    let mut words = HashSet::new();
    let mut support = HashMap::<u64, u32>::new();
    for row in tsv.lines() {
        if let Some(text) = row.split('\t').nth(1) {
            // Polyphonic rows represent the same lexical word, not independent
            // usage observations. A repeated pair within a word also counts once.
            if !words.insert(text) {
                continue;
            }
            let mut pairs = BTreeSet::new();
            let chars: Vec<_> = text.chars().collect();
            for pair in chars.windows(2) {
                if pair.iter().all(|c| matches!(c, '\u{4e00}'..='\u{9fff}')) {
                    pairs.insert(((pair[0] as u64) << 32) | pair[1] as u64);
                }
            }
            for pair in pairs {
                let count = support.entry(pair).or_default();
                *count = count.saturating_add(1);
            }
        }
    }
    let mut ranked: Vec<_> = support.into_iter().collect();
    ranked.sort_unstable_by(|(ka, sa), (kb, sb)| sb.cmp(sa).then_with(|| ka.cmp(kb)));
    ranked.truncate(capacity);
    let mut selected: Vec<_> = ranked.into_iter().map(|(key, _)| key).collect();
    selected.sort_unstable();
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair(text: &str) -> u64 {
        let mut chars = text.chars();
        ((chars.next().unwrap() as u64) << 32) | chars.next().unwrap() as u64
    }

    #[test]
    fn stronger_high_codepoint_evidence_survives_capacity_selection() {
        let rows = "yi'ding\t一丁\t99999\nlong'ma\t龙马\t1\nlong'ma'jing'shen\t龙马精神\t1\n";
        assert_eq!(select(rows, 1), vec![pair("龙马")]);
    }

    #[test]
    fn polyphonic_rows_and_repeated_adjacencies_do_not_inflate_word_support() {
        let rows = "yi'ding\t一丁\t100\nyi'deng\t一丁\t100\nyi'ding'yi'ding\t一丁一丁\t100\nlong'ma\t龙马\t1\nlong'ma'jing'shen\t龙马精神\t1\nlong'ma'jin'qu\t龙马进取\t1\n";
        assert_eq!(select(rows, 1), vec![pair("龙马")]);
        let mut reversed: Vec<_> = rows.lines().collect();
        reversed.reverse();
        assert_eq!(select(rows, 5), select(&reversed.join("\n"), 5));
        let selected = select(rows, 5);
        assert!(selected.windows(2).all(|p| p[0] < p[1]));
        assert!(select(rows, 0).is_empty());
    }
}
