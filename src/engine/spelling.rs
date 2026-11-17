//! Bounded, background-only full-pinyin repair candidate generation.
use std::collections::{BTreeMap, BTreeSet};

use super::{decoder::lexical_score, EngineError, Lexicon, SpellingCandidate};
use sha2::{Digest, Sha256};

#[allow(dead_code)] // Shared with build.rs; encoding is used only there and by tests.
#[path = "../../build_support/spelling_seed.rs"]
mod spelling_seed;

const STATE_LIMIT: usize = 16_384;
const BEAM: usize = 8;

#[derive(Default)]
struct Node {
    children: BTreeMap<u8, usize>,
    words: Vec<Word>,
}
struct Word {
    text: String,
    reading: String,
    score: i64,
}

pub struct SpellingIndex {
    nodes: Vec<Node>,
}

/// Construction-only cache: path[i] is the node after i compact bytes.
struct BuildCursor {
    previous: Vec<u8>,
    current: Vec<u8>,
    path: Vec<usize>,
}

impl BuildCursor {
    fn new() -> Self {
        Self {
            previous: Vec::new(),
            current: Vec::new(),
            path: vec![0],
        }
    }

    fn terminal(&mut self, nodes: &mut Vec<Node>, reading: &str) -> usize {
        self.current.clear();
        self.current.extend(reading.bytes().filter(|&b| b != b'\''));
        let common = self
            .previous
            .iter()
            .zip(&self.current)
            .take_while(|(a, b)| a == b)
            .count();
        self.path.truncate(common + 1);
        let mut node = self.path[common];
        for &byte in &self.current[common..] {
            let next = if let Some(&next) = nodes[node].children.get(&byte) {
                next
            } else {
                let next = nodes.len();
                nodes.push(Node::default());
                nodes[node].children.insert(byte, next);
                next
            };
            node = next;
            self.path.push(node);
        }
        std::mem::swap(&mut self.previous, &mut self.current);
        node
    }
}

#[derive(Clone)]
struct Path {
    text: String,
    reading: String,
    score: i64,
    characters: usize,
}

fn valid_raw(raw: &str) -> bool {
    (4..=128).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_lowercase())
}

fn canonical_reading(reading: &str) -> bool {
    reading.split('\'').all(|s| {
        !s.is_empty()
            && s.bytes().all(|b| b.is_ascii_lowercase())
            && s.bytes().any(|b| b"aeiouv".contains(&b))
    })
}

fn retain_path(beam: &mut Vec<Path>, path: Path) {
    beam.push(path);
    beam.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.text.cmp(&b.text))
            .then(a.reading.cmp(&b.reading))
    });
    beam.dedup_by(|a, b| a.text == b.text && a.reading == b.reading);
    beam.truncate(BEAM);
}

fn append_word(prefix: &Path, word: &Word) -> Option<Path> {
    let characters = prefix.characters + word.text.chars().count();
    (characters <= 64).then(|| Path {
        text: format!("{}{}", prefix.text, word.text),
        reading: if prefix.reading.is_empty() {
            word.reading.clone()
        } else {
            format!("{}'{}", prefix.reading, word.reading)
        },
        score: prefix.score + word.score,
        characters,
    })
}

/// Checks exactly one substitution, insertion, deletion or adjacent transposition.
pub fn single_spelling_edit(raw: &str, reading: &str) -> bool {
    if !valid_raw(raw) || !canonical_reading(reading) {
        return false;
    }
    let target: Vec<u8> = reading.bytes().filter(|&b| b != b'\'').collect();
    let source = raw.as_bytes();
    if source.len().abs_diff(target.len()) > 1 || source == target {
        return false;
    }
    let first = source
        .iter()
        .zip(&target)
        .position(|(a, b)| a != b)
        .unwrap_or(source.len().min(target.len()));
    match source.len().cmp(&target.len()) {
        std::cmp::Ordering::Less => source[first..] == target[first + 1..],
        std::cmp::Ordering::Greater => source[first + 1..] == target[first..],
        std::cmp::Ordering::Equal => {
            source[first + 1..] == target[first + 1..]
                || (first + 1 < source.len()
                    && source[first] == target[first + 1]
                    && source[first + 1] == target[first]
                    && source[first + 2..] == target[first + 2..])
        }
    }
}

impl SpellingIndex {
    /// Background-only factory. Embedded seed bytes are referenced only here.
    pub fn builtin() -> Result<Self, EngineError> {
        mod metadata {
            include!(concat!(env!("OUT_DIR"), "/builtin-spelling.rs"));
            pub fn values() -> (usize, [u8; 32]) {
                (BUILTIN_SPELLING_SIZE, BUILTIN_SPELLING_SHA256)
            }
        }
        let (size, sha) = metadata::values();
        Self::from_raw_seed(
            include_str!(concat!(env!("OUT_DIR"), "/builtin-spelling.tsv")),
            size,
            &sha,
        )
    }

    #[cfg(test)]
    fn from_seed(compressed: &[u8], size: usize, sha: &[u8; 32]) -> Result<Self, EngineError> {
        let invalid = || EngineError::invalid_line(0, "invalid builtin spelling seed");
        let bytes = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, size)
            .map_err(|_| invalid())?;
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
        Self::from_raw_seed(text, size, sha)
    }

    fn from_raw_seed(text: &str, size: usize, sha: &[u8; 32]) -> Result<Self, EngineError> {
        let invalid = || EngineError::invalid_line(0, "invalid builtin spelling seed");
        if text.len() != size || Sha256::digest(text.as_bytes()).as_slice() != sha {
            return Err(invalid());
        }
        if text.is_empty() || !text.ends_with('\n') {
            return Err(invalid());
        }
        let mut result = Self {
            nodes: vec![Node::default()],
        };
        let mut cursor = BuildCursor::new();
        for row in text[..text.len() - 1].split('\n') {
            let (reading, text, weight) = spelling_seed::parse_row(row).ok_or_else(invalid)?;
            result.insert(&mut cursor, reading, text, weight);
        }
        Ok(result)
    }

    pub fn new(lexicon: &Lexicon) -> Self {
        let mut result = Self {
            nodes: vec![Node::default()],
        };
        let mut cursor = BuildCursor::new();
        for (reading, text, weight) in lexicon.spelling_entries() {
            if !spelling_seed::eligible(reading, text) {
                continue;
            }
            result.insert(&mut cursor, reading, text, weight);
        }
        result
    }

    fn insert(&mut self, cursor: &mut BuildCursor, reading: &str, text: &str, weight: u32) {
        let characters = text.chars().count();
        let node = cursor.terminal(&mut self.nodes, reading);
        let words = &mut self.nodes[node].words;
        words.push(Word {
            text: text.into(),
            reading: reading.into(),
            score: lexical_score(weight, characters),
        });
        words.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(a.text.cmp(&b.text))
                .then(a.reading.cmp(&b.reading))
        });
        words.dedup_by(|a, b| a.text == b.text && a.reading == b.reading);
        words.truncate(4);
    }

    pub fn candidates(&self, raw: &str, limit: usize) -> Vec<SpellingCandidate> {
        self.search_guided(raw, limit, STATE_LIMIT, None, false).0
    }

    pub fn candidates_with_source(
        &self,
        raw: &str,
        limit: usize,
        source: &str,
        reading: &str,
    ) -> Vec<SpellingCandidate> {
        if !crate::ai::eligible_spelling_text(source)
            || !(reading.len() <= 512
                && reading
                    .split('\'')
                    .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
                && reading.split('\'').count() == source.chars().count())
        {
            return Vec::new();
        }
        self.search_guided(raw, limit, STATE_LIMIT, Some((source, reading)), true)
            .0
    }

    fn search_guided(
        &self,
        raw: &str,
        limit: usize,
        budget: usize,
        guide: Option<(&str, &str)>,
        delete_first: bool,
    ) -> (Vec<SpellingCandidate>, usize) {
        if !valid_raw(raw) || limit == 0 {
            return (Vec::new(), 0);
        }
        let budget = budget.min(STATE_LIMIT);
        let n = raw.len();
        let mut paths = vec![vec![Vec::<Path>::new(); 2]; n + 1];
        paths[0][0].push(Path {
            text: String::new(),
            reading: String::new(),
            score: 0,
            characters: 0,
        });
        let mut states = 0;
        let mut priority = Vec::new();
        if delete_first && guide.is_some() {
            for i in 0..n {
                if states == budget {
                    break;
                }
                let mut deleted = raw.as_bytes().to_vec();
                deleted.remove(i);
                for path in self.exact_paths(&deleted, &mut states, budget, guide, raw) {
                    if complete_ok(&path, guide, raw) {
                        retain_path(&mut priority, path);
                    }
                }
            }
        }
        for start in 0..n {
            if paths[start].iter().all(Vec::is_empty) {
                continue;
            }
            let input = &raw.as_bytes()[start..];
            let initial: Vec<u8> = (0..=input.len()).map(|i| i.min(2) as u8).collect();
            let mut stack = vec![(0usize, initial, Vec::<u8>::new(), None::<u8>)];
            let mut edges: BTreeMap<(usize, u8), Vec<&Word>> = BTreeMap::new();
            while let Some((node, row, previous, last)) = stack.pop() {
                if states == budget {
                    break;
                }
                states += 1;
                if !self.nodes[node].words.is_empty() {
                    for (consumed, &cost) in row.iter().enumerate().skip(1) {
                        if cost <= 1 {
                            let words = edges.entry((start + consumed, cost)).or_default();
                            words.extend(&self.nodes[node].words);
                            words.sort_by(|a, b| {
                                b.score
                                    .cmp(&a.score)
                                    .then(a.text.cmp(&b.text))
                                    .then(a.reading.cmp(&b.reading))
                            });
                            words.dedup_by(|a, b| a.text == b.text && a.reading == b.reading);
                            words.truncate(4);
                        }
                    }
                }
                if states == budget {
                    break;
                }
                for (&letter, &child) in self.nodes[node].children.iter().rev() {
                    let mut next = vec![(row[0] + 1).min(2); input.len() + 1];
                    for j in 1..=input.len() {
                        next[j] = (row[j] + 1)
                            .min(next[j - 1] + 1)
                            .min(row[j - 1] + u8::from(letter != input[j - 1]))
                            .min(2);
                        if j > 1 && last == Some(input[j - 1]) && letter == input[j - 2] {
                            next[j] = next[j].min(previous[j - 2] + 1);
                        }
                    }
                    if next.iter().any(|&cost| cost <= 1) {
                        stack.push((child, next, row.clone(), Some(letter)));
                    }
                }
            }
            for ((end, cost), words) in edges {
                for spent in 0..=1 - cost as usize {
                    let prefixes = paths[start][spent].clone();
                    for prefix in prefixes {
                        for word in &words {
                            if let Some(path) = append_word(&prefix, word) {
                                if prefix_ok(&path, guide)
                                    && (end != n || complete_ok(&path, guide, raw))
                                {
                                    retain_path(&mut paths[end][spent + cost as usize], path);
                                }
                            }
                        }
                    }
                }
            }
            if states == budget {
                break;
            }
        }
        let mut complete = std::mem::take(&mut paths[n][1]);
        for path in priority {
            retain_path(&mut complete, path);
        }
        // A word-local DP cannot see a swap straddling two dictionary edges.
        // Exact trie walks of the adjacent swaps cover these cases without
        // invoking the ordinary decoder or resetting the shared state budget.
        let mut swapped = raw.as_bytes().to_vec();
        for offset in 0..n - 1 {
            if states == budget {
                break;
            }
            if swapped[offset] == swapped[offset + 1] {
                continue;
            }
            swapped.swap(offset, offset + 1);
            for path in self.exact_paths(&swapped, &mut states, budget, guide, raw) {
                if complete_ok(&path, guide, raw) {
                    retain_path(&mut complete, path);
                }
            }
            swapped.swap(offset, offset + 1);
        }
        let mut seen = BTreeSet::new();
        let result = complete
            .iter()
            .filter(|p| p.characters >= 2 && single_spelling_edit(raw, &p.reading))
            .filter(|p| seen.insert(p.text.clone()))
            .take(limit.min(BEAM))
            .map(|p| SpellingCandidate {
                text: p.text.clone(),
                reading: p.reading.clone(),
                edits: 1,
            })
            .collect();
        (result, states)
    }

    fn exact_paths(
        &self,
        raw: &[u8],
        states: &mut usize,
        budget: usize,
        guide: Option<(&str, &str)>,
        original_raw: &str,
    ) -> Vec<Path> {
        let mut paths = vec![Vec::new(); raw.len() + 1];
        paths[0].push(Path {
            text: String::new(),
            reading: String::new(),
            score: 0,
            characters: 0,
        });
        for start in 0..raw.len() {
            if paths[start].is_empty() {
                continue;
            }
            let prefixes = paths[start].clone();
            let mut node = 0;
            for (end, byte) in raw.iter().enumerate().skip(start) {
                if *states == budget {
                    return std::mem::take(&mut paths[raw.len()]);
                }
                *states += 1;
                let Some(&child) = self.nodes[node].children.get(byte) else {
                    break;
                };
                node = child;
                for prefix in &prefixes {
                    for word in &self.nodes[node].words {
                        if let Some(path) = append_word(prefix, word) {
                            if prefix_ok(&path, guide)
                                && (end + 1 != raw.len() || complete_ok(&path, guide, original_raw))
                            {
                                retain_path(&mut paths[end + 1], path);
                            }
                        }
                    }
                }
            }
        }
        std::mem::take(&mut paths[raw.len()])
    }
}

fn prefix_ok(p: &Path, guide: Option<(&str, &str)>) -> bool {
    let Some((source, reading)) = guide else {
        return true;
    };
    for ((a, b), (x, y)) in p
        .reading
        .split('\'')
        .zip(reading.split('\''))
        .zip(p.text.chars().zip(source.chars()))
    {
        if a != b {
            break;
        }
        if x != y {
            return false;
        }
    }
    true
}
fn complete_ok(p: &Path, guide: Option<(&str, &str)>, raw: &str) -> bool {
    let Some((source, reading)) = guide else {
        return true;
    };
    crate::ai::spelling_change_span(raw, source, reading, &p.text, &p.reading).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(index: &SpellingIndex) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update((index.nodes.len() as u64).to_le_bytes());
        for node in &index.nodes {
            hash.update((node.children.len() as u64).to_le_bytes());
            for (&byte, &child) in &node.children {
                hash.update([byte]);
                hash.update((child as u64).to_le_bytes());
            }
            hash.update((node.words.len() as u64).to_le_bytes());
            for word in &node.words {
                for value in [&word.text, &word.reading] {
                    hash.update((value.len() as u64).to_le_bytes());
                    hash.update(value.as_bytes());
                }
                hash.update(word.score.to_le_bytes());
            }
        }
        hash.finalize().into()
    }

    fn fixture_seed(raw: &[u8]) -> Result<SpellingIndex, EngineError> {
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(raw, 6);
        SpellingIndex::from_seed(&compressed, raw.len(), &Sha256::digest(raw).into())
    }

    // Independent original algorithm, deliberately not using BuildCursor or insert.
    fn root_walk_oracle(rows: &[(&str, &str, u32)]) -> SpellingIndex {
        let mut result = SpellingIndex {
            nodes: vec![Node::default()],
        };
        for &(reading, text, weight) in rows {
            let characters = text.chars().count();
            if characters == 0
                || characters > 64
                || !text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
                || reading.split('\'').count() != characters
                || !canonical_reading(reading)
            {
                continue;
            }
            let mut node = 0;
            for byte in reading.bytes().filter(|&b| b != b'\'') {
                let next = if let Some(&next) = result.nodes[node].children.get(&byte) {
                    next
                } else {
                    let next = result.nodes.len();
                    result.nodes.push(Node::default());
                    result.nodes[node].children.insert(byte, next);
                    next
                };
                node = next;
            }
            let words = &mut result.nodes[node].words;
            words.push(Word {
                text: text.into(),
                reading: reading.into(),
                score: lexical_score(weight, characters),
            });
            words.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then(a.text.cmp(&b.text))
                    .then(a.reading.cmp(&b.reading))
            });
            words.dedup_by(|a, b| a.text == b.text && a.reading == b.reading);
            words.truncate(4);
        }
        result
    }

    #[test]
    fn build_cursor_matches_independent_root_walk_in_any_order() {
        let rows = [
            ("ni'hao", "你好", 100),
            ("ni", "你", 20),
            ("ni'hen", "你很", 30),
            ("ni", "尼", 25),
            ("ni", "你", 40),
            ("ni", "泥", 24),
            ("ni", "拟", 23),
            ("ni", "逆", 22),
            ("ni", "妮", 21),
            ("nian", "年", 15),
            ("ni'an", "你安", 16), // Same compact bytes, different reading.
            ("a", "啊", 5),
            ("zai", "在", 6),
            ("n", "你", 7), // Rejected row must not disturb the cursor.
            ("ni'hao", "你好", 101),
        ];
        let mut variants = vec![rows.to_vec()];
        variants.push(rows.iter().copied().rev().collect());
        let mut sorted = rows.to_vec();
        sorted.sort_by(|a, b| b.0.cmp(a.0));
        variants.push(sorted);
        for rows in variants {
            let expected = fingerprint(&root_walk_oracle(&rows));
            let mut index = SpellingIndex {
                nodes: vec![Node::default()],
            };
            let mut cursor = BuildCursor::new();
            for &(reading, text, weight) in &rows {
                if spelling_seed::eligible(reading, text) {
                    index.insert(&mut cursor, reading, text, weight);
                }
            }
            assert_eq!(fingerprint(&index), expected);
            let seed = spelling_seed::encode(rows.iter().copied());
            assert_eq!(fingerprint(&fixture_seed(&seed).unwrap()), expected);
            let tsv: String = rows
                .iter()
                .filter(|(r, t, _)| spelling_seed::eligible(r, t))
                .map(|(r, t, w)| format!("{r}\t{t}\t{w}\n"))
                .collect();
            let lexicon = Lexicon::from_tsv(&tsv).unwrap();
            let ordered: Vec<_> = lexicon.spelling_entries().collect();
            assert_eq!(
                fingerprint(&SpellingIndex::new(&lexicon)),
                fingerprint(&root_walk_oracle(&ordered))
            );
        }
    }

    #[test]
    fn seed_factory_matches_lexicon_factory() {
        let lexicon = Lexicon::from_tsv(
            "ni\t你\t100\nni\t尼\t90\nni\t泥\t80\nni\t拟\t70\nni\t逆\t60\n\
             ni'hao\t你好\t1000\nnv\t女\t20\n",
        )
        .unwrap();
        let seed = spelling_seed::encode(lexicon.spelling_entries());
        let old = SpellingIndex::new(&lexicon);
        let new = fixture_seed(&seed).unwrap();
        assert_eq!(fingerprint(&old), fingerprint(&new));
        assert_eq!(old.candidates("nihaoo", 8), new.candidates("nihaoo", 8));
    }

    #[test]
    fn raw_seed_factory_validates_and_matches_root_walk() {
        let rows = [
            ("ni'hao", "你好", 1000),
            ("ni", "你", 100),
            ("ni", "尼", 90),
        ];
        let bytes = spelling_seed::encode(rows);
        let text = std::str::from_utf8(&bytes).unwrap();
        let sha = Sha256::digest(&bytes).into();
        let index = SpellingIndex::from_raw_seed(text, text.len(), &sha).unwrap();
        assert_eq!(fingerprint(&index), fingerprint(&root_walk_oracle(&rows)));
        let error = || EngineError::invalid_line(0, "invalid builtin spelling seed");
        for (size, digest) in [
            (text.len() - 1, sha),
            (text.len() + 1, sha),
            (text.len(), [0; 32]),
        ] {
            assert_eq!(
                SpellingIndex::from_raw_seed(text, size, &digest).err(),
                Some(error())
            );
        }
        // Correct size and SHA must not bypass framing, fields or eligibility.
        for text in [
            "",
            "\n",
            "ni\t你\t1",
            "ni\t你\t1\r\n",
            "ni\t你\t1\n\n",
            "ni\t\t1\n",
            "ni\tbad\t1\n",
            "n\t你\t1\n",
            "ni'hao\t你\t1\n",
            "ni\t你\t0\n",
            "ni\t你\t4294967296\n",
            "ni\t你\t+1\n",
            "ni\t你\n",
            "ni\t你\t1\textra\n",
        ] {
            assert_eq!(
                SpellingIndex::from_raw_seed(
                    text,
                    text.len(),
                    &Sha256::digest(text.as_bytes()).into()
                )
                .err(),
                Some(error()),
                "{text:?}"
            );
        }
    }

    #[test]
    fn seed_factory_rejects_corruption() {
        let raw = b"ni\t\xe4\xbd\xa0\t1\n";
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(raw, 6);
        let sha = Sha256::digest(raw).into();
        let error = || EngineError::invalid_line(0, "invalid builtin spelling seed");
        for (data, size, digest) in [
            (&b"invalid"[..], raw.len(), sha),
            (&compressed[..], raw.len() - 1, sha),
            (&compressed[..], raw.len() + 1, sha),
            (&compressed[..], raw.len(), [0; 32]),
        ] {
            assert_eq!(
                SpellingIndex::from_seed(data, size, &digest).err(),
                Some(error())
            );
        }
        for raw in [
            &b""[..],
            &b"\xff\n"[..],
            &b"\n"[..],
            &b"ni\tbad\t1\n"[..],
            &b"ni\t\xe4\xbd\xa0\t0\n"[..],
            &b"ni\t\xe4\xbd\xa0\t1\textra\n"[..],
            &b"ni\t\xe4\xbd\xa0\t1"[..],
            &b"ni\t\xe4\xbd\xa0\t4294967296\n"[..],
        ] {
            assert_eq!(fixture_seed(raw).err(), Some(error()), "{raw:?}");
        }
    }

    #[test]
    #[ignore = "full builtin trie comparison; explicitly run with --ignored --nocapture"]
    fn builtin_seed_full_structure_matches_lexicon() {
        use std::time::Instant;
        const BASELINE: [u8; 32] = [
            0xe5, 0x4a, 0x1f, 0xdd, 0xe5, 0x8a, 0x82, 0x44, 0x7b, 0x5f, 0x7b, 0xc4, 0x49, 0x19,
            0x76, 0x74, 0x10, 0xae, 0x84, 0xad, 0x95, 0xf0, 0x85, 0x21, 0x00, 0xa8, 0x52, 0xde,
            0xf2, 0x64, 0xfc, 0x80,
        ];
        let start = Instant::now();
        let lexicon = Lexicon::builtin().unwrap();
        eprintln!("Lexicon builtin load: {:?}", start.elapsed());
        let start = Instant::now();
        let index = SpellingIndex::new(&lexicon);
        eprintln!("Lexicon spelling trie build only: {:?}", start.elapsed());
        let start = Instant::now();
        let expected = fingerprint(&index);
        eprintln!("Lexicon trie fingerprint only: {:?}", start.elapsed());
        let start = Instant::now();
        drop(index);
        drop(lexicon);
        eprintln!("Lexicon + trie drop only: {:?}", start.elapsed());
        let start = Instant::now();
        let index = SpellingIndex::builtin().unwrap();
        eprintln!(
            "Raw seed builtin build only (including validation): {:?}",
            start.elapsed()
        );
        let start = Instant::now();
        let actual = fingerprint(&index);
        eprintln!("Raw seed trie fingerprint only: {:?}", start.elapsed());
        let start = Instant::now();
        drop(index);
        eprintln!("Raw seed trie drop only: {:?}", start.elapsed());
        eprintln!("Spelling trie SHA256: {actual:02x?}");
        assert_eq!(expected, BASELINE);
        assert_eq!(actual, BASELINE);
        assert_eq!(expected, actual);
    }

    #[test]
    fn guided_delete_and_swap_share_budget() {
        let index = SpellingIndex::new(
            &Lexicon::from_tsv("ni'hao\t你好\t1000\nshi'jie\t世界\t1000\n").unwrap(),
        );
        for (raw, source, reading) in [
            ("nihaoshijiee", "你好世界鄂", "ni'hao'shi'jie'e"),
            ("nihasohijie", "你好嗖界", "ni'hao'sou'jie"),
        ] {
            let guide = Some((source, reading));
            let (full, states) = index.search_guided(raw, 8, usize::MAX, guide, true);
            assert!(states <= STATE_LIMIT);
            assert!(full.iter().any(|c| c.text == "你好世界"));
            for budget in 0..=states {
                let (found, used) = index.search_guided(raw, 8, budget, guide, true);
                assert!(used <= budget);
                assert!(found.iter().all(|c| crate::ai::spelling_change_span(
                    raw, source, reading, &c.text, &c.reading
                )
                .is_some()));
            }
        }
    }

    #[test]
    fn budget_is_shared_and_exhaustion_keeps_only_complete_paths() {
        let index =
            SpellingIndex::new(&Lexicon::from_tsv("ni'hao'shi'jie\t你好世界\t100\n").unwrap());
        let mut retained_on_exhaustion = false;
        for budget in 0..=100 {
            let (found, states) = index.search_guided("nihaoshijiee", 8, budget, None, false);
            assert!(states <= budget);
            for candidate in &found {
                assert_eq!(candidate.text, "你好世界");
                assert_eq!(candidate.reading, "ni'hao'shi'jie");
                assert!(single_spelling_edit("nihaoshijiee", &candidate.reading));
            }
            retained_on_exhaustion |= states == budget && !found.is_empty();
        }
        assert!(retained_on_exhaustion);

        // Two exact walks share one counter. They leave only one state for
        // the third walk, which must not return a
        // partial prefix as a complete path.
        let mut states = STATE_LIMIT - 23;
        let first = index.exact_paths(
            b"nihaoshijie",
            &mut states,
            STATE_LIMIT,
            None,
            "nihaoshijie",
        );
        assert_eq!(states, STATE_LIMIT - 12);
        assert_eq!(first[0].text, "你好世界");
        let second = index.exact_paths(
            b"nihaoshijie",
            &mut states,
            STATE_LIMIT,
            None,
            "nihaoshijie",
        );
        assert_eq!(states, STATE_LIMIT - 1);
        assert_eq!(second[0].text, "你好世界");
        let third = index.exact_paths(
            b"nihaoshijie",
            &mut states,
            STATE_LIMIT,
            None,
            "nihaoshijie",
        );
        assert!(third.is_empty());
        assert_eq!(states, STATE_LIMIT);
        assert!(index
            .exact_paths(
                b"nihaoshijie",
                &mut states,
                STATE_LIMIT,
                None,
                "nihaoshijie"
            )
            .is_empty());
        assert_eq!(states, STATE_LIMIT);
    }

    #[test]
    fn exact_swap_variants_share_fuzzy_budget() {
        let index = SpellingIndex::new(
            &Lexicon::from_tsv("ni'hao\t你好\t1000\nshi'jie\t世界\t1000\n").unwrap(),
        );
        let raw = "nihasohijie";
        let (full, states) = index.search_guided(raw, 8, STATE_LIMIT, None, false);
        assert!(full.iter().any(|c| c.text == "你好世界"));
        assert!(states < STATE_LIMIT);
        let (none, _) = index.search_guided(raw, 8, 0, None, false);
        assert!(none.is_empty());
        let mut first_success = None;
        for budget in 1..=states {
            let (found, used) = index.search_guided(raw, 8, budget, None, false);
            assert!(used <= budget);
            assert!(found
                .iter()
                .all(|c| { c.text == "你好世界" && single_spelling_edit(raw, &c.reading) }));
            if !found.is_empty() {
                first_success = Some(budget);
                break;
            }
        }
        let first_success = first_success.unwrap();
        assert!(first_success > raw.len());
        assert!(index
            .search_guided(raw, 8, first_success - 1, None, false)
            .0
            .is_empty());
        assert!(!index
            .search_guided(raw, 8, first_success, None, false)
            .0
            .is_empty());
    }

    #[test]
    fn hard_state_cap_retains_previously_completed_candidates() {
        let text = "阿".repeat(64);
        let mut tsv = String::from("a\t阿\t100\n");
        for changed in 0..64 {
            let mut syllables = vec!["a"; 64];
            syllables[changed] = "e";
            tsv.push_str(&format!("{}\t{text}\t100\n", syllables.join("'")));
        }
        let index = SpellingIndex::new(&Lexicon::from_tsv(&tsv).unwrap());
        let raw = "a".repeat(64);
        // Even a caller-supplied larger test budget cannot raise the cap.
        let (found, states) = index.search_guided(&raw, 8, usize::MAX, None, false);
        assert_eq!(states, STATE_LIMIT);
        assert!(!found.is_empty());
        for candidate in found {
            assert_eq!(candidate.text, text);
            assert_eq!(candidate.reading.split('\'').count(), 64);
            assert!(single_spelling_edit(&raw, &candidate.reading));
        }
    }
}
