//! Pure ordering operations shared by activation memory and the local store.
use super::{PromotionEvent, RankingCandidate, RankingOrder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Personal word identity is canonical reading plus unprojected dictionary text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentWord {
    pub pinyin: String,
    pub text: String,
    pub sequence: u64,
}

/// Bounded successful-selection evidence, not a saved host paragraph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WordLink {
    pub prefix: String,
    pub pinyin: String,
    pub text: String,
    pub count: u32,
    pub sequence: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RankingSnapshot {
    pub sequence: u64,
    pub words: HashMap<(String, String), (u32, u64)>,
    pub links: HashMap<(String, String, String), (u32, u64)>,
}

impl RankingSnapshot {
    pub fn word_bonus(&self, pinyin: &str, text: &str) -> i64 {
        self.words
            .get(&(pinyin.to_owned(), text.to_owned()))
            .map_or(0, |&(count, last)| {
                count_bonus(count, 24) + recency(self.sequence, last, 96)
            })
    }

    pub fn link_bonus(&self, prefix: &str, pinyin: &str, text: &str) -> i64 {
        let prefix = short_suffix(prefix);
        // Confirmed words may follow an arbitrary verified host prefix. Match
        // a saved word suffix there without persisting longer host passages.
        let personal = prefix
            .char_indices()
            .filter_map(|(offset, _)| {
                self.links
                    .get(&(
                        prefix[offset..].to_owned(),
                        pinyin.to_owned(),
                        text.to_owned(),
                    ))
                    .map(|&(count, last)| count_bonus(count, 48) + recency(self.sequence, last, 96))
            })
            .max()
            .unwrap_or(0);
        personal + public_link_bonus(&prefix, text)
    }
}

fn count_bonus(count: u32, scale: i64) -> i64 {
    if count == 0 {
        0
    } else {
        i64::from(32 - count.min(8192).leading_zeros()) * scale
    }
}

fn recency(now: u64, last: u64, maximum: i64) -> i64 {
    if last == 0 {
        0
    } else {
        maximum.saturating_sub((now.saturating_sub(last) / 4).min(maximum as u64) as i64)
    }
}

pub fn short_suffix(text: &str) -> String {
    text.chars()
        .rev()
        .take_while(|c| is_han(*c))
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub fn is_han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff
        | 0x20000..=0x2a6df | 0x2a700..=0x2b73f | 0x2b740..=0x2b81f
        | 0x2b820..=0x2ceaf | 0x2ceb0..=0x2ebef | 0x2ebf0..=0x2ee5f
        | 0x2f800..=0x2fa1f | 0x30000..=0x3134f | 0x31350..=0x323af)
}

fn public_link_bonus(prefix: &str, text: &str) -> i64 {
    let Some(left) = prefix.chars().last() else {
        return 0;
    };
    let Some(right) = text.chars().next() else {
        return 0;
    };
    let key = ((left as u64) << 32) | right as u64;
    // Sorted, bounded word-list-derived adjacent Han pairs. Not corpus counts.
    const PAIRS: &[u64] = include!(concat!(env!("OUT_DIR"), "/word-list-links.rs"));
    if PAIRS.binary_search(&key).is_ok() {
        24
    } else {
        0
    }
}

/// Reconcile an active candidate set with a saved order, without inventing choices.
pub fn ranking_indices(candidates: &[RankingCandidate], saved: &RankingOrder) -> Vec<usize> {
    let lookup: HashMap<_, _> = candidates.iter().enumerate().map(|(i, c)| (c, i)).collect();
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(candidates.len());
    for candidate in &saved.candidates {
        if let Some(&index) = lookup.get(candidate) {
            if seen.insert(index) {
                result.push(index);
            }
        }
    }
    result.extend((0..candidates.len()).filter(|i| !seen.contains(i)));
    result
}

/// Call only after validation and a successful safe commit. Returns whether a
/// choice moved; the newest domain is retained even for a first-position choice.
pub fn promote_order(orders: &mut Vec<RankingOrder>, event: &PromotionEvent) -> bool {
    let mut order = if let Some(index) = orders.iter().position(|o| o.input == event.order.input) {
        let previous = orders.remove(index);
        let indices = ranking_indices(&event.order.candidates, &previous);
        RankingOrder {
            input: event.order.input.clone(),
            candidates: indices
                .into_iter()
                .map(|i| event.order.candidates[i].clone())
                .collect(),
        }
    } else {
        event.order.clone()
    };
    let mut moved = false;
    if let Some(index) = order.candidates.iter().position(|c| c == &event.selected) {
        if index > 0 {
            order.candidates.swap(index - 1, index);
            moved = true;
        }
    }
    if orders.len() >= 256 {
        orders.remove(0);
    }
    orders.push(order);
    moved
}
