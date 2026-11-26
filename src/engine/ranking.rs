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
    pub(crate) recent_choices: HashMap<String, (String, u64)>,
}

impl RankingSnapshot {
    pub fn word_bonus(&self, pinyin: &str, text: &str) -> i64 {
        self.words
            .get(&(pinyin.to_owned(), text.to_owned()))
            .map_or(0, |&(count, last)| {
                let preference = self
                    .recent_choices
                    .get(pinyin)
                    .filter(|(selected, _)| selected == text)
                    .map_or(0, |(_, sequence)| recency(self.sequence, *sequence, 256));
                count_bonus(count, 32) + recency(self.sequence, last, 320) + preference
            })
    }

    pub(crate) fn index_recent_choices(&mut self, recent: &[RecentWord]) {
        self.recent_choices.clear();
        // RecentWord order is commit/event order. Later entries win a tie
        // within one commit, independent of HashMap iteration order.
        for word in recent.iter().filter(|word| word.sequence != 0) {
            let choice = self
                .recent_choices
                .entry(word.pinyin.clone())
                .or_insert_with(|| (word.text.clone(), word.sequence));
            if word.sequence >= choice.1 {
                *choice = (word.text.clone(), word.sequence);
            }
        }
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

/// Project-curated conversation prior, not measured corpus frequency. Exact
/// reading/text identities avoid transferring preference to unrelated readings.
/// Keep this small, sorted list separate from user evidence and source weights.
pub(crate) fn dialogue_bonus(pinyin: &str, text: &str) -> i64 {
    if DIALOGUE_PAIRS.binary_search(&(pinyin, text)).is_ok() {
        128
    } else {
        0
    }
}

pub(crate) const DIALOGUE_PAIRS: &[(&str, &str)] = &[
    ("bao'qian", "抱歉"),
    ("bu'hao'yi'si", "不好意思"),
    ("dan'shi", "但是"),
    ("dian'hua", "电话"),
    ("er'qie", "而且"),
    ("fa'song", "发送"),
    ("fang'bian", "方便"),
    ("gan'jue", "感觉"),
    ("gao'xing", "高兴"),
    ("gong'zuo", "工作"),
    ("hao'de", "好的"),
    ("hui'fu", "回复"),
    ("ji'hua", "计划"),
    ("jin'tian", "今天"),
    ("jue'de", "觉得"),
    ("ke'yi", "刻意"),
    ("ke'yi", "可以"),
    ("ke'yi", "可疑"),
    ("lian'xi", "联系"),
    ("ma'fan", "麻烦"),
    ("mei'shi", "没事"),
    ("ming'bai", "明白"),
    ("ming'tian", "明天"),
    ("peng'you", "朋友"),
    ("qing'wen", "请问"),
    ("que'ren", "确认"),
    ("ran'hou", "然后"),
    ("ru'guo", "如果"),
    ("shen'me", "什么"),
    ("shi'jian", "事件"),
    ("shi'jian", "实践"),
    ("shi'jian", "时间"),
    ("shi'jie", "世界"),
    ("shi'jie", "师姐"),
    ("shi'jie", "时节"),
    ("shi'qing", "事情"),
    ("shi'shi", "事实"),
    ("shi'shi", "实时"),
    ("shi'shi", "试试"),
    ("shou'dao", "收到"),
    ("shou'ji", "手机"),
    ("shui'jiao", "睡觉"),
    ("sui'ran", "虽然"),
    ("suo'yi", "所以"),
    ("wei'shen'me", "为什么"),
    ("wen'ti", "问题"),
    ("xi'huan", "喜欢"),
    ("xi'wang", "希望"),
    ("xiao'xi", "消息"),
    ("xie'xie", "谢谢"),
    ("xu'yao", "需要"),
    ("yi'jing", "一经"),
    ("yi'jing", "已经"),
    ("yi'jing", "意境"),
    ("yi'qi", "一起"),
    ("yi'xia", "一下"),
    ("yi'yi", "一一"),
    ("yi'yi", "异议"),
    ("yi'yi", "意义"),
    ("yin'wei", "因为"),
    ("zai'jian", "再见"),
    ("zen'me", "怎么"),
    ("zhi'dao", "知道"),
];

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

#[cfg(test)]
mod dialogue_tests {
    use super::*;

    #[test]
    fn recent_choice_is_bounded_decays_and_is_independent_per_reading() {
        let mut snapshot = RankingSnapshot {
            sequence: 10,
            ..RankingSnapshot::default()
        };
        let mut recent = Vec::new();
        for (pinyin, text, sequence) in [("shi", "是", 9), ("shi", "市", 10), ("si", "四", 10)] {
            snapshot
                .words
                .insert((pinyin.into(), text.into()), (1, sequence));
            recent.push(RecentWord {
                pinyin: pinyin.into(),
                text: text.into(),
                sequence,
            });
        }
        snapshot.index_recent_choices(&recent);
        assert_eq!(snapshot.word_bonus("shi", "市"), 32 + 320 + 256);
        assert_eq!(snapshot.word_bonus("shi", "是"), 32 + 320);
        assert_eq!(snapshot.word_bonus("si", "四"), 32 + 320 + 256);
        for age in [3, 4, 1023, 1024, 1280] {
            snapshot.sequence = 10 + age;
            assert_eq!(
                snapshot.word_bonus("shi", "市"),
                32 + recency(snapshot.sequence, 10, 320) + recency(snapshot.sequence, 10, 256)
            );
        }
        // Empty/legacy recent records never invent a last choice.
        snapshot.index_recent_choices(&[]);
        assert!(snapshot.recent_choices.is_empty());
    }

    #[test]
    fn recent_choice_uses_sequence_then_commit_order_and_transfers_preference() {
        let mut snapshot = RankingSnapshot::default();
        let recent = |text: &str, sequence| RecentWord {
            pinyin: "shi".into(),
            text: text.into(),
            sequence,
        };
        snapshot.index_recent_choices(&[recent("是", 4), recent("市", 4)]);
        assert_eq!(snapshot.recent_choices["shi"].0, "市");
        snapshot.index_recent_choices(&[recent("市", 4), recent("是", 5)]);
        assert_eq!(snapshot.recent_choices["shi"].0, "是");
        snapshot.index_recent_choices(&[recent("是", 5), recent("市", 4)]);
        assert_eq!(
            snapshot.recent_choices["shi"].0, "是",
            "sequence wins over storage order"
        );
        snapshot.index_recent_choices(&[recent("市", 0)]);
        assert!(snapshot.recent_choices.is_empty());
    }

    #[test]
    fn conversation_prior_is_sorted_bounded_and_only_uses_real_readings() {
        assert!(DIALOGUE_PAIRS.windows(2).all(|pair| pair[0] < pair[1]));
        let lexicon = crate::engine::Lexicon::builtin().unwrap();
        for &(pinyin, text) in DIALOGUE_PAIRS {
            assert!(
                lexicon.contains_exact_pair(pinyin, text),
                "{pinyin}: {text}"
            );
            assert_eq!(dialogue_bonus(pinyin, text), 128);
            assert_eq!(dialogue_bonus("invalid", text), 0);
        }
        assert_eq!(
            dialogue_bonus("hui'yi", "回忆"),
            0,
            "feedback must not be an example-specific prior"
        );
    }
}
