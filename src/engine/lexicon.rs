use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use super::{Candidate, EngineError};

#[cfg(test)]
const BUILTIN_LEXICON: &str = flower_dict::DEFAULT_TSV;

include!(concat!(env!("OUT_DIR"), "/builtin-dictionary.rs"));
const BUILTIN_COMPRESSED: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/builtin-dictionary.zlib"));

fn builtin_dictionary_bytes(data: &[u8]) -> Result<Vec<u8>, EngineError> {
    use sha2::{Digest, Sha256};
    let bytes = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, BUILTIN_SIZE)
        .map_err(|_| EngineError::InvalidUtf8 {
            message: "compressed builtin dictionary is corrupt".into(),
        })?;
    if bytes.len() != BUILTIN_SIZE || <[u8; 32]>::from(Sha256::digest(&bytes)) != BUILTIN_SHA256 {
        return Err(EngineError::InvalidUtf8 {
            message: "builtin dictionary size or digest does not match".into(),
        });
    }
    Ok(bytes)
}

const EVERYDAY_PHRASES: &str = flower_dict::EVERYDAY_TSV;

// Project-maintained usage prior, NOT corpus frequency or new readings.
// Exact pairs prevent a future additional reading inheriting the classification.
const EVERYDAY_PAIRS: &str = flower_dict::COMMON_USAGE_PAIRS;

const EVERYDAY_RANKING_FLOOR: u32 = 16384;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LexiconEntry {
    text: String,
    weight: u32,
    reading: String,
    everyday: bool,
}

impl LexiconEntry {
    fn choice(&self, consumed: usize) -> super::decoder::Choice {
        let mut choice = super::decoder::Choice::word(
            Candidate::new(&self.text, &self.reading),
            consumed,
            self.weight,
        );
        // This prior belongs only to the supplemental graph, never legacy indexes.
        if self.everyday || (self.reading == "shu'ru" && self.text == "输入") {
            choice.supplemental_score +=
                super::decoder::lexical_score(
                    self.weight.max(EVERYDAY_RANKING_FLOOR),
                    self.text.chars().count(),
                ) - super::decoder::lexical_score(self.weight, self.text.chars().count());
        }
        choice
    }
}

fn rank_entries(left: &LexiconEntry, right: &LexiconEntry) -> std::cmp::Ordering {
    let score = |entry: &LexiconEntry| {
        if entry.everyday {
            entry.weight.max(EVERYDAY_RANKING_FLOOR)
        } else {
            entry.weight
        }
    };
    score(right)
        .cmp(&score(left))
        .then_with(|| right.weight.cmp(&left.weight))
        .then_with(|| left.text.cmp(&right.text))
        .then_with(|| left.reading.cmp(&right.reading))
}

/// A deterministic, exact-match Pinyin lexicon.
///
/// TSV rows use three tab-separated UTF-8 fields:
/// `lowercase-pinyin<TAB>candidate-text<TAB>weight`.
/// Empty lines and lines whose first non-space character is `#` are ignored.
#[derive(Debug, Clone)]
pub struct Lexicon {
    dormant: bool,
    entries: Arc<HashMap<String, Vec<LexiconEntry>>>,
    initials: Arc<HashMap<String, Vec<LexiconEntry>>>,
    syllables: Arc<SyllableTrie>,
}

#[cfg(test)]
mod everyday_pair_tests {
    use super::*;

    #[test]
    fn fixed_everyday_pairs_exist_with_their_exact_reading() {
        let lexicon = Lexicon::builtin().unwrap();
        let pairs: Vec<_> = EVERYDAY_PAIRS
            .split_whitespace()
            .map(|pair| pair.split_once('=').unwrap())
            .collect();
        assert_eq!(pairs.len(), 135);
        for (reading, text) in pairs {
            assert!(lexicon
                .lookup(&reading.replace('\'', ""))
                .iter()
                .any(|candidate| candidate.text == text && candidate.comment == reading));
        }
    }
}

const MIXED_BUDGET: usize = 4096;

#[derive(Debug, Clone, Default)]
struct SyllableNode {
    edges: std::collections::BTreeMap<u8, Vec<(String, usize)>>,
    words: Vec<LexiconEntry>,
}

#[derive(Debug, Clone)]
struct SyllableTrie {
    nodes: Vec<SyllableNode>,
}

impl SyllableTrie {
    fn build(entries: &HashMap<String, Vec<LexiconEntry>>) -> Self {
        let mut trie = Self {
            nodes: vec![SyllableNode::default()],
        };
        // Sort before insertion so the bounded traversal is independent of HashMap seeds.
        let mut words: Vec<_> = entries.values().flatten().collect();
        words.sort_by(|a, b| a.reading.cmp(&b.reading).then_with(|| rank_entries(a, b)));
        for word in words {
            let syllables: Vec<_> = word.reading.split('\'').collect();
            if syllables.len() < 2 || syllables.len() != word.text.chars().count() {
                continue;
            }
            let mut node = 0;
            for syllable in syllables {
                let first = syllable.as_bytes()[0];
                let next = trie.nodes[node]
                    .edges
                    .get(&first)
                    .and_then(|edges| edges.iter().find(|(s, _)| s == syllable))
                    .map(|(_, next)| *next);
                node = match next {
                    Some(next) => next,
                    None => {
                        let next = trie.nodes.len();
                        trie.nodes.push(SyllableNode::default());
                        trie.nodes[node]
                            .edges
                            .entry(first)
                            .or_default()
                            .push((syllable.to_owned(), next));
                        next
                    }
                };
            }
            trie.nodes[node].words.push(word.clone());
        }
        for node in &mut trie.nodes {
            node.words.sort_by(rank_entries);
        }
        trie
    }
}

static BUILTIN: OnceLock<Result<Lexicon, EngineError>> = OnceLock::new();

#[cfg(test)]
mod completion_budget_tests {
    use super::*;

    #[test]
    fn three_short_syllables_and_shared_edge_budget_are_respected() {
        let lexicon = Lexicon::from_tsv("a'o'e'he\t阿哦额和\t100").unwrap();
        let mut budget = (512, 4096);
        let choice = lexicon.completion_choice("aoe", &mut budget).unwrap();
        assert_eq!(choice.candidate.text, "阿哦额和");
        assert_eq!(choice.consumed, 3);
        assert!(choice.events.is_empty() && choice.candidate.is_completion());
        let mut exhausted = (3, 4096);
        assert!(lexicon.completion_choice("aoe", &mut exhausted).is_none());
        assert_eq!(exhausted.0, 0);
        assert!(lexicon.completion_choice("a'o'e", &mut exhausted).is_none());
    }

    #[test]
    fn descendant_edges_and_words_share_one_budget_without_full_table_scan() {
        let mut tsv = String::new();
        for i in 0..400u32 {
            let a = char::from(b'a' + (i / 26) as u8);
            let b = char::from(b'a' + (i % 26) as u8);
            let text = char::from_u32(0x4e00 + i).unwrap();
            tsv.push_str(&format!("jun'bu'jian'h{a}{b}\t君不见{text}\t{}\n", i + 1));
        }
        let lexicon = Lexicon::from_tsv(&tsv).unwrap();
        let mut budget = (512, 12);
        let _ = lexicon.completion_choice("junbujian", &mut budget);
        assert_eq!(budget.1, 0);
        // A fallback table cannot silently reset the refresh's exhausted budget.
        assert!(lexicon
            .completion_choice("junbujian", &mut budget)
            .is_none());
    }
}

impl Lexicon {
    /// Whole-syllable full-Pinyin prefix completion using the existing shared
    /// trie. Both prefix exploration and descendant inspection are bounded;
    /// there is no per-key scan of the full table or duplicate completion index.
    pub(crate) fn completion_choice(
        &self,
        raw: &str,
        budget: &mut (usize, usize),
    ) -> Option<super::decoder::Choice> {
        const PREFIX_BUDGET: usize = 512;
        const DESCENDANT_BUDGET: usize = 4096;
        if raw.len() < 3
            || !raw.bytes().all(|c| c.is_ascii_lowercase() || c == b'\'')
            || raw.starts_with('\'')
            || raw.contains("''")
        {
            return None;
        }
        let mut prefixes = vec![(0usize, 0usize, 0usize)];
        let mut roots = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut inspected = 0;
        while let Some((node, offset, syllables)) = prefixes.pop() {
            if !visited.insert((node, offset, syllables)) {
                continue;
            }
            if budget.0 == 0 {
                break;
            }
            budget.0 -= 1;
            inspected += 1;
            if offset == raw.len() {
                if syllables >= 3 {
                    roots.push(node);
                }
                continue;
            }
            if let Some(edges) = self.syllables.nodes[node]
                .edges
                .get(&raw.as_bytes()[offset])
            {
                for (syllable, next) in edges.iter().rev() {
                    if budget.0 == 0 {
                        break;
                    }
                    budget.0 -= 1;
                    if raw[offset..].starts_with(syllable) {
                        let mut end = offset + syllable.len();
                        if raw.as_bytes().get(end) == Some(&b'\'') {
                            end += 1;
                        }
                        if prefixes.len() + inspected < PREFIX_BUDGET {
                            prefixes.push((*next, end, syllables + 1));
                        }
                    }
                }
            }
        }
        let mut stack: Vec<_> = roots.into_iter().map(|n| (n, false)).collect();
        let mut seen = std::collections::HashSet::new();
        let mut best: Option<&LexiconEntry> = None;
        inspected = 0;
        while let Some((node, extended)) = stack.pop() {
            if !seen.insert((node, extended)) {
                continue;
            }
            if budget.1 == 0 {
                break;
            }
            budget.1 -= 1;
            inspected += 1;
            if extended {
                for entry in &self.syllables.nodes[node].words {
                    if budget.1 == 0 {
                        break;
                    }
                    budget.1 -= 1;
                    inspected += 1;
                    if entry.text.chars().count() <= 64
                        && best.map_or(true, |old| rank_entries(entry, old).is_lt())
                    {
                        best = Some(entry);
                    }
                }
            }
            for (_, next) in self.syllables.nodes[node]
                .edges
                .values()
                .rev()
                .flat_map(|edges| edges.iter().rev())
            {
                if budget.1 == 0 {
                    break;
                }
                budget.1 -= 1;
                if stack.len() + inspected < DESCENDANT_BUDGET {
                    stack.push((*next, true));
                }
            }
        }
        best.map(|entry| {
            let mut choice = entry.choice(raw.len());
            choice.candidate.comment = format!("{}{}", Candidate::COMPLETION_PREFIX, entry.reading);
            choice.events.clear();
            choice
        })
    }
    #[cfg(test)]
    pub(crate) fn builtin_initialized() -> bool {
        BUILTIN.get().is_some()
    }
    pub(crate) fn spelling_entries(&self) -> impl Iterator<Item = (&str, &str, u32)> {
        self.entries
            .values()
            .flatten()
            .map(|entry| (entry.reading.as_str(), entry.text.as_str(), entry.weight))
    }

    /// Loads shared base/core/everyday and all curated category tables.
    pub fn builtin() -> Result<Self, EngineError> {
        BUILTIN
            .get_or_init(|| {
                let bytes = builtin_dictionary_bytes(BUILTIN_COMPRESSED)?;
                let text =
                    std::str::from_utf8(&bytes).map_err(|error| EngineError::InvalidUtf8 {
                        message: error.to_string(),
                    })?;
                let mut entries = Self::parse_entries(text)?;
                drop(bytes);
                let words: std::collections::HashSet<_> = EVERYDAY_PAIRS
                    .split_whitespace()
                    .filter_map(|pair| pair.split_once('='))
                    .collect();
                let phrases: std::collections::HashSet<_> = EVERYDAY_PHRASES
                    .lines()
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .filter_map(|line| {
                        let mut fields = line.split('\t');
                        Some((fields.next()?, fields.next()?))
                    })
                    .collect();
                for candidates in entries.values_mut() {
                    for entry in candidates.iter_mut() {
                        let syllables = entry.reading.split('\'').count();
                        entry.everyday = syllables >= 2
                            && syllables == entry.text.chars().count()
                            && (words.contains(&(entry.reading.as_str(), entry.text.as_str()))
                                || phrases
                                    .contains(&(entry.reading.as_str(), entry.text.as_str())));
                    }
                    candidates.sort_by(rank_entries);
                }
                Ok(Self::from_entries(entries))
            })
            .clone()
    }

    /// Parses and validates a UTF-8 TSV lexicon.
    pub fn from_tsv(tsv: &str) -> Result<Self, EngineError> {
        Ok(Self::from_entries(Self::parse_entries(tsv)?))
    }

    /// Used only before the first real composing key, never as an input fallback.
    pub(crate) fn dormant() -> Self {
        let mut lexicon = Self::from_entries(HashMap::new());
        lexicon.dormant = true;
        lexicon
    }

    pub(crate) fn is_dormant(&self) -> bool {
        self.dormant
    }

    fn parse_entries(tsv: &str) -> Result<HashMap<String, Vec<LexiconEntry>>, EngineError> {
        let tsv = tsv.strip_prefix('\u{feff}').unwrap_or(tsv);
        let mut entries: HashMap<String, Vec<LexiconEntry>> = HashMap::new();

        for (index, raw_line) in tsv.lines().enumerate() {
            let line_number = index + 1;
            let line = raw_line.trim_end_matches('\r');
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let mut fields = line.split('\t');
            let key = fields.next().unwrap_or_default().trim();
            let text = match fields.next() {
                Some(value) => value.trim(),
                None => {
                    return Err(EngineError::invalid_line(
                        line_number,
                        "expected pinyin, text, and weight fields",
                    ))
                }
            };
            let weight = match fields.next() {
                Some(value) => value.trim(),
                None => {
                    return Err(EngineError::invalid_line(
                        line_number,
                        "missing weight field",
                    ))
                }
            };
            if fields.next().is_some() {
                return Err(EngineError::invalid_line(
                    line_number,
                    "expected exactly three tab-separated fields",
                ));
            }

            if !super::segment::valid_reading(key) {
                return Err(EngineError::invalid_line(
                    line_number,
                    "pinyin must contain lowercase ASCII syllables separated by apostrophes",
                ));
            }
            if text.is_empty() {
                return Err(EngineError::invalid_line(
                    line_number,
                    "candidate text must not be empty",
                ));
            }
            if text.chars().any(char::is_control) {
                return Err(EngineError::invalid_line(
                    line_number,
                    "candidate text must not contain control characters",
                ));
            }

            let weight: u32 = weight.parse().map_err(|_| {
                EngineError::invalid_line(line_number, "weight must be an unsigned 32-bit integer")
            })?;
            if weight == 0 {
                return Err(EngineError::invalid_line(
                    line_number,
                    "weight must be greater than zero",
                ));
            }

            entries
                .entry(key.replace('\'', ""))
                .or_default()
                .push(LexiconEntry {
                    text: text.to_owned(),
                    weight,
                    reading: key.to_owned(),
                    everyday: false,
                });
        }

        if entries.is_empty() {
            return Err(EngineError::EmptyLexicon);
        }

        for candidates in entries.values_mut() {
            // Group equal text before weight ordering: equal text with different
            // weights need not be adjacent in a weight-first sorted list.
            candidates.sort_by(|left, right| {
                left.text
                    .cmp(&right.text)
                    .then_with(|| left.reading.cmp(&right.reading))
                    .then_with(|| right.weight.cmp(&left.weight))
            });
            candidates
                .dedup_by(|left, right| left.text == right.text && left.reading == right.reading);
            candidates.sort_by(rank_entries);
        }

        Ok(entries)
    }

    fn from_entries(entries: HashMap<String, Vec<LexiconEntry>>) -> Self {
        Self {
            dormant: false,
            initials: Arc::new(Self::build_initials(&entries)),
            syllables: Arc::new(SyllableTrie::build(&entries)),
            entries: Arc::new(entries),
        }
    }

    fn build_initials(
        entries: &HashMap<String, Vec<LexiconEntry>>,
    ) -> HashMap<String, Vec<LexiconEntry>> {
        let mut initials: HashMap<String, Vec<LexiconEntry>> = HashMap::new();
        for entry in entries.values().flatten() {
            let syllables: Vec<_> = entry.reading.split('\'').collect();
            if syllables.len() != entry.text.chars().count() {
                continue;
            }
            let key: String = syllables.iter().map(|s| s.as_bytes()[0] as char).collect();
            initials.entry(key).or_default().push(entry.clone());
        }
        for words in initials.values_mut() {
            words.sort_by(rank_entries);
        }
        initials
    }

    pub(crate) fn initial_choices(&self, raw: &str) -> Vec<super::decoder::Choice> {
        self.initial_choices_limited(raw, usize::MAX)
    }

    fn initial_choices_limited(&self, raw: &str, limit: usize) -> Vec<super::decoder::Choice> {
        // Single-character initials belong only to the standalone input entry,
        // never to the multi-syllable graph or whole-word abbreviation lane.
        if raw.len() < 2 || raw.contains('\'') {
            return Vec::new();
        }
        self.initials
            .get(raw)
            .into_iter()
            .flatten()
            .take(limit)
            .map(|entry| entry.choice(raw.len()))
            .collect()
    }

    pub(crate) fn single_initial_choices(&self, raw: &str) -> Vec<super::decoder::Choice> {
        if raw.len() != 1 || !raw.as_bytes()[0].is_ascii_lowercase() {
            return Vec::new();
        }
        self.initials
            .get(raw)
            .into_iter()
            .flatten()
            .map(|entry| {
                let mut choice = entry.choice(1);
                choice.abbreviated = usize::from(entry.reading.len() > 1);
                choice.penalize_abbreviations();
                choice
            })
            .collect()
    }

    /// Bounded traversal of canonical syllables, never a product of spellings.
    /// Every transition consumes either the entire syllable or its entered initial.
    #[cfg(test)]
    pub(crate) fn mixed_choices(&self, raw: &str) -> Vec<super::decoder::Choice> {
        let mut expanded = 0;
        self.mixed_prefix_choices(raw, &mut expanded, MIXED_BUDGET)
            .into_iter()
            .filter(|c| c.consumed == raw.len())
            .collect()
    }

    pub(crate) fn mixed_prefix_choices(
        &self,
        raw: &str,
        expanded: &mut usize,
        total_limit: usize,
    ) -> Vec<super::decoder::Choice> {
        if raw.is_empty()
            || !raw.bytes().all(|b| b.is_ascii_lowercase() || b == b'\'')
            || raw.starts_with('\'')
            || raw.contains("''")
        {
            return Vec::new();
        }
        if *expanded >= total_limit {
            return Vec::new();
        }
        *expanded += 1;
        let mut stack = vec![(0usize, 0usize, 0usize, 0usize)];
        let mut states = std::collections::HashSet::new();
        let mut matches = Vec::new();
        let mut admitted = 1;
        while let Some((node, offset, full, abbreviated)) = stack.pop() {
            if !states.insert((node, offset, full, abbreviated)) {
                continue;
            }
            if abbreviated > 0 && matches.len() < MIXED_BUDGET {
                for entry in &self.syllables.nodes[node].words {
                    if matches.len() == MIXED_BUDGET {
                        break;
                    }
                    matches.push((entry, abbreviated, offset));
                }
            }
            if offset == raw.len() {
                continue;
            }
            let Some(edges) = self.syllables.nodes[node]
                .edges
                .get(&raw.as_bytes()[offset])
            else {
                continue;
            };
            for (syllable, next) in edges {
                for abbreviated_edge in [true, false] {
                    let token = if abbreviated_edge {
                        &syllable[..1]
                    } else {
                        syllable.as_str()
                    };
                    // A one-letter syllable is full, not an abbreviation.
                    if abbreviated_edge && syllable.len() == 1 {
                        continue;
                    }
                    if !raw[offset..].starts_with(token) {
                        continue;
                    }
                    let mut end = offset + token.len();
                    if raw.as_bytes().get(end) == Some(&b'\'') {
                        end += 1;
                    }
                    if admitted == MIXED_BUDGET || *expanded == total_limit {
                        continue;
                    }
                    admitted += 1;
                    *expanded += 1;
                    stack.push((
                        *next,
                        end,
                        full + usize::from(!abbreviated_edge),
                        abbreviated + usize::from(abbreviated_edge),
                    ));
                }
            }
        }
        matches.sort_by(|(a, ac, ae), (b, bc, be)| {
            ae.cmp(be)
                .then_with(|| rank_entries(a, b))
                .then_with(|| ac.cmp(bc))
        });
        let mut seen = std::collections::HashSet::new();
        matches
            .into_iter()
            .filter(|(entry, _, end)| seen.insert((&entry.text, &entry.reading, *end)))
            .map(|(entry, abbreviated, end)| {
                let mut choice = entry.choice(end);
                choice.abbreviated = abbreviated;
                choice.penalize_abbreviations();
                choice
            })
            .collect()
    }

    pub(crate) fn graph_choices(&self, raw: &str, per_end: usize) -> Vec<super::decoder::Choice> {
        let mut choices = self.prefix_choices_limited(raw, per_end);
        for end in 1..=raw.len() {
            if !raw.is_char_boundary(end) || raw.as_bytes().get(end) == Some(&b'\'') {
                continue;
            }
            choices.extend(
                self.initial_choices_limited(&raw[..end], per_end)
                    .into_iter()
                    .map(|mut choice| {
                        choice.abbreviated = choice
                            .candidate
                            .comment
                            .split('\'')
                            .filter(|syllable| syllable.len() > 1)
                            .count();
                        choice.penalize_abbreviations();
                        choice
                    }),
            );
        }
        choices
    }

    /// Parses a byte slice, reporting invalid UTF-8 as an engine error.
    pub fn from_tsv_bytes(bytes: &[u8]) -> Result<Self, EngineError> {
        let text = std::str::from_utf8(bytes).map_err(|error| EngineError::InvalidUtf8 {
            message: error.to_string(),
        })?;
        Self::from_tsv(text)
    }

    /// Returns ranked candidates for an exact Pinyin key.
    pub fn lookup(&self, pinyin: &str) -> Vec<Candidate> {
        self.exact_choices(pinyin)
            .into_iter()
            .map(|choice| choice.candidate)
            .collect()
    }

    pub(crate) fn contains_exact_pair(&self, reading: &str, text: &str) -> bool {
        self.entries
            .get(&reading.replace('\'', ""))
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.reading == reading && entry.text == text)
            })
    }

    /// Bounded exact-key probes for standalone short-word spelling suggestions.
    pub(crate) fn extra_syllable_choices(
        &self,
        raw: &str,
        source: &Candidate,
    ) -> Vec<super::decoder::Choice> {
        if !(4..=128).contains(&raw.len())
            || !raw.bytes().all(|b| b.is_ascii_lowercase())
            || self.entries.contains_key(raw)
            || source.is_ai()
            || source.is_spelling()
            || !crate::ai::eligible_spelling_text(&source.text)
        {
            return Vec::new();
        }
        let syllables: Vec<_> = source.comment.split('\'').collect();
        let chars: Vec<_> = source.text.chars().collect();
        if syllables.len() != chars.len() || syllables.concat() != raw {
            return Vec::new();
        }
        let mut queries = 0;
        let mut choices = Vec::new();
        for offset in 0..syllables.len() {
            if !["a", "e", "o"].contains(&syllables[offset]) {
                continue;
            }
            let text: String = chars
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != offset)
                .map(|(_, c)| *c)
                .collect();
            let reading = syllables
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != offset)
                .map(|(_, s)| *s)
                .collect::<Vec<_>>()
                .join("'");
            if crate::ai::spelling_change_span(raw, &source.text, &source.comment, &text, &reading)
                .is_none()
                || !self.exact_text_coverage(&text, &reading, &mut queries)
            {
                continue;
            }
            let mut choice = super::decoder::Choice::word(
                Candidate::new(text, format!("{}{}", Candidate::SPELLING_PREFIX, reading)),
                raw.len(),
                1,
            );
            choice.events.clear();
            choices.push(choice);
            if choices.len() == 4 {
                break;
            }
        }
        choices
    }

    fn exact_text_coverage(&self, text: &str, reading: &str, queries: &mut usize) -> bool {
        let syllables: Vec<_> = reading.split('\'').collect();
        let chars: Vec<_> = text.chars().collect();
        let mut reached = vec![false; chars.len() + 1];
        reached[0] = true;
        for start in 0..chars.len() {
            if !reached[start] {
                continue;
            }
            let mut key = String::new();
            let mut word = String::new();
            for end in start..chars.len() {
                if *queries >= 1024 {
                    return false;
                }
                *queries += 1;
                key.push_str(syllables[end]);
                word.push(chars[end]);
                if self.entries.get(&key).is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry.text == word && entry.reading == syllables[start..=end].join("'")
                    })
                }) {
                    reached[end + 1] = true;
                }
            }
        }
        reached[chars.len()]
    }

    /// Bounded exact-key probes for standalone short-word spelling suggestions.
    pub(crate) fn short_spelling_choices(&self, raw: &str) -> Vec<super::decoder::Choice> {
        if !(4..=24).contains(&raw.len())
            || !raw.bytes().all(|b| b.is_ascii_lowercase())
            || self.entries.contains_key(raw)
        {
            return Vec::new();
        }
        self.edited_spelling_entries(raw, &mut 0, 1321)
            .into_iter()
            .map(|entry| {
                let mut choice = entry.choice(raw.len());
                choice.candidate.comment =
                    format!("{}{}", Candidate::SPELLING_PREFIX, entry.reading);
                choice.events.clear();
                choice
            })
            .collect()
    }

    fn edited_spelling_entries(
        &self,
        raw: &str,
        queries: &mut usize,
        limit: usize,
    ) -> Vec<&LexiconEntry> {
        if !(4..=24).contains(&raw.len())
            || !raw.bytes().all(|b| b.is_ascii_lowercase())
            || *queries >= limit
        {
            return Vec::new();
        }
        let mut keys = std::collections::BTreeSet::new();
        for i in 0..raw.len() {
            let mut key = raw.as_bytes().to_vec();
            key.remove(i);
            keys.insert(key);
            for byte in b'a'..=b'z' {
                if byte != raw.as_bytes()[i] {
                    let mut key = raw.as_bytes().to_vec();
                    key[i] = byte;
                    keys.insert(key);
                }
            }
        }
        for i in 0..=raw.len() {
            for byte in b'a'..=b'z' {
                let mut key = raw.as_bytes().to_vec();
                key.insert(i, byte);
                keys.insert(key);
            }
        }
        for i in 0..raw.len() - 1 {
            let mut key = raw.as_bytes().to_vec();
            key.swap(i, i + 1);
            if key != raw.as_bytes() {
                keys.insert(key);
            }
        }
        let mut words = Vec::new();
        for key in &keys {
            if *queries >= limit {
                break;
            }
            *queries += 1;
            let key = std::str::from_utf8(key).expect("lowercase ASCII spelling");
            if let Some(entries) = self.entries.get(key) {
                words.extend(entries.iter().filter(|entry| {
                    (2..=4).contains(&entry.text.chars().count())
                        && entry
                            .text
                            .chars()
                            .all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
                        && entry.reading.split('\'').count() == entry.text.chars().count()
                        && super::single_spelling_edit(raw, &entry.reading)
                }));
            }
        }
        words.sort_by(|a, b| rank_entries(a, b));
        let mut seen = std::collections::HashSet::new();
        words
            .into_iter()
            .filter(|entry| seen.insert(entry.text.as_str()))
            .take(4)
            .collect()
    }

    /// One edited dictionary word joined to one exact dictionary word.
    pub(crate) fn composed_spelling_choices(&self, raw: &str) -> Vec<super::decoder::Choice> {
        const LIMIT: usize = 4096;
        if !(4..=48).contains(&raw.len()) || !raw.bytes().all(|b| b.is_ascii_lowercase()) {
            return Vec::new();
        }
        let mut queries = 1;
        if self.entries.contains_key(raw) {
            return Vec::new();
        }
        let eligible = |entry: &&LexiconEntry| {
            (1..=4).contains(&entry.text.chars().count())
                && entry
                    .text
                    .chars()
                    .all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
                && entry.reading.split('\'').count() == entry.text.chars().count()
                && entry
                    .reading
                    .split('\'')
                    .all(|s| s.bytes().any(|b| b"aeiouv".contains(&b)))
        };
        // Validate an unchanged two-word parse independently of repair lengths.
        for split in 1..raw.len() {
            queries += 1;
            if self
                .entries
                .get(&raw[..split])
                .is_some_and(|words| words.iter().any(|word| eligible(&word)))
            {
                queries += 1;
                if self
                    .entries
                    .get(&raw[split..])
                    .is_some_and(|words| words.iter().any(|word| eligible(&word)))
                {
                    return Vec::new();
                }
            }
        }
        let mut splits = Vec::new();
        for split in 1..raw.len() {
            for exact_first in [true, false] {
                let (exact, edited) = if exact_first {
                    (&raw[..split], &raw[split..])
                } else {
                    (&raw[split..], &raw[..split])
                };
                if !(4..=24).contains(&edited.len()) {
                    continue;
                }
                queries += 1;
                if let Some(entries) = self.entries.get(exact) {
                    let entries: Vec<_> = entries.iter().filter(eligible).take(4).collect();
                    if !entries.is_empty() {
                        splits.push((exact.len(), split, exact_first, entries));
                    }
                }
            }
        }
        splits
            .sort_by_key(|(length, split, first, _)| (std::cmp::Reverse(*length), *split, *first));
        let mut candidates = Vec::new();
        for (_, split, exact_first, entries) in splits {
            if queries >= LIMIT {
                break;
            }
            let edited = if exact_first {
                &raw[split..]
            } else {
                &raw[..split]
            };
            let repairs = self.edited_spelling_entries(edited, &mut queries, LIMIT);
            for exact in entries {
                for repair in &repairs {
                    let (left, right) = if exact_first {
                        (exact, *repair)
                    } else {
                        (*repair, exact)
                    };
                    let text = format!("{}{}", left.text, right.text);
                    let reading = format!("{}'{}", left.reading, right.reading);
                    if !super::single_spelling_edit(raw, &reading) {
                        continue;
                    }
                    let score = |entry: &LexiconEntry| {
                        super::decoder::lexical_score(
                            if entry.everyday {
                                entry.weight.max(EVERYDAY_RANKING_FLOOR)
                            } else {
                                entry.weight
                            },
                            entry.text.chars().count(),
                        )
                    };
                    candidates.push((score(left) + score(right), text, reading));
                }
            }
        }
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut seen = std::collections::HashSet::new();
        candidates
            .into_iter()
            .filter(|(_, text, _)| seen.insert(text.clone()))
            .take(4)
            .map(|(_, text, reading)| {
                let mut choice = super::decoder::Choice::word(
                    Candidate::new(text, format!("{}{}", Candidate::SPELLING_PREFIX, reading)),
                    raw.len(),
                    1,
                );
                choice.events.clear();
                choice
            })
            .collect()
    }

    /// Make one immutable activation snapshot with explicitly supplied local words.
    pub fn with_overlay(&self, tsv: &str) -> Result<Self, EngineError> {
        if tsv.trim().is_empty() {
            return Ok(self.clone());
        }
        let overlay = Self::from_tsv(tsv)?;
        let mut result = self.clone();
        let table = Arc::make_mut(&mut result.entries);
        for (key, additions) in overlay.entries.iter() {
            let entries = table.entry(key.clone()).or_default();
            for addition in additions {
                if let Some(existing) = entries
                    .iter_mut()
                    .find(|entry| entry.text == addition.text && entry.reading == addition.reading)
                {
                    existing.weight = existing.weight.max(addition.weight);
                } else {
                    entries.push(addition.clone());
                }
            }
            entries.sort_by(rank_entries);
        }
        result.initials = Arc::new(Self::build_initials(table));
        result.syllables = Arc::new(SyllableTrie::build(table));
        Ok(result)
    }

    /// Bounded local adjustment; never creates a word absent from the active dictionary.
    pub fn with_learning(&self, events: &[(super::LearningEvent, u32)]) -> Self {
        if events.is_empty() {
            return self.clone();
        }
        let mut result = self.clone();
        let table = Arc::make_mut(&mut result.entries);
        for (event, count) in events {
            if let Some(entries) = table.get_mut(&event.pinyin.replace('\'', "")) {
                for entry in entries
                    .iter_mut()
                    .filter(|e| e.text == event.text && e.reading == event.pinyin)
                {
                    entry.weight = entry
                        .weight
                        .saturating_add(count.saturating_mul(512).min(8192));
                }
                entries.sort_by(rank_entries);
            }
        }
        result.initials = Arc::new(Self::build_initials(table));
        result.syllables = Arc::new(SyllableTrie::build(table));
        result
    }

    pub(crate) fn exact_choices(&self, raw: &str) -> Vec<super::decoder::Choice> {
        self.exact_choices_limited(raw, usize::MAX)
    }

    fn exact_choices_limited(&self, raw: &str, limit: usize) -> Vec<super::decoder::Choice> {
        let compact = raw.replace('\'', "");
        let mut seen = std::collections::HashSet::new();
        self.entries
            .get(&compact)
            .into_iter()
            .flatten()
            .filter(|entry| super::segment::boundaries_match(raw, &entry.reading))
            .filter(|entry| seen.insert(entry.text.clone()))
            .take(limit)
            .map(|entry| entry.choice(raw.len()))
            .collect()
    }

    pub(crate) fn prefix_choices(&self, raw: &str) -> Vec<super::decoder::Choice> {
        self.prefix_choices_limited(raw, usize::MAX)
    }

    pub(crate) fn prefix_choices_limited(
        &self,
        raw: &str,
        per_prefix: usize,
    ) -> Vec<super::decoder::Choice> {
        let mut choices = Vec::new();
        for end in 1..=raw.len() {
            // Raw composition is restricted to ASCII before this function is called.
            if !raw.is_char_boundary(end) {
                continue;
            }
            // The delimiter belongs to the preceding span. Never offer a
            // prefix whose selection strands a leading apostrophe in the tail.
            if raw.as_bytes().get(end) == Some(&b'\'') {
                continue;
            }
            choices.extend(self.exact_choices_limited(&raw[..end], per_prefix));
        }
        choices
    }

    /// Number of distinct Pinyin keys in this lexicon.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_builtin_preserves_exact_bytes_and_rejects_corruption() {
        let bytes = builtin_dictionary_bytes(BUILTIN_COMPRESSED).unwrap();
        assert_eq!(bytes, BUILTIN_LEXICON.as_bytes());
        assert!(BUILTIN_COMPRESSED.len() < bytes.len() / 2);
        assert!(
            builtin_dictionary_bytes(&BUILTIN_COMPRESSED[..BUILTIN_COMPRESSED.len() / 2]).is_err()
        );
        let wrong = miniz_oxide::deflate::compress_to_vec_zlib(b"wrong dictionary", 1);
        assert!(builtin_dictionary_bytes(&wrong).is_err());
        let mut corrupt = BUILTIN_COMPRESSED.to_vec();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        assert!(builtin_dictionary_bytes(&corrupt).is_err());
    }

    #[test]
    fn spelling_key_budget_is_shared_and_never_reset_between_segments() {
        let lexicon = Lexicon::from_tsv("jin'tian\t今天\t100\nzao'shang\t早上\t100").unwrap();
        let mut queries = 31;
        lexicon.edited_spelling_entries("jintain", &mut queries, 32);
        assert_eq!(queries, 32);
        assert!(lexicon
            .edited_spelling_entries("zaosahng", &mut queries, 32)
            .is_empty());
        assert_eq!(queries, 32);
        assert!(lexicon
            .composed_spelling_choices(&"q".repeat(49))
            .is_empty());
        assert!(lexicon
            .composed_spelling_choices("jin'tainzaoshang")
            .is_empty());
    }

    #[test]
    fn supplemental_metadata_penalizes_vowelless_syllables_without_changing_legacy() {
        let lexicon =
            Lexicon::from_tsv("n\t嗯\t100\nshu'ru\t输入\t100\nni'hao\t你好\t100").unwrap();
        let n = &lexicon.exact_choices("n")[0];
        assert_eq!(
            n.supplemental_score,
            super::super::decoder::lexical_score(100, 1) - 1024
        );
        let input = &lexicon.exact_choices("shuru")[0];
        assert_eq!(
            input.supplemental_score,
            super::super::decoder::lexical_score(16384, 2)
        );
        let hello = &lexicon.exact_choices("nihao")[0];
        assert_eq!(
            hello.supplemental_score,
            super::super::decoder::lexical_score(100, 2)
        );
        assert!(lexicon
            .entries
            .values()
            .flatten()
            .all(|entry| !entry.everyday && entry.weight == 100));
        let mut mixed = lexicon.mixed_choices("shur").remove(0);
        assert_eq!(mixed.abbreviated, 1);
        assert_eq!(mixed.supplemental_score, input.supplemental_score - 512);
        // Keep the existing score/abbreviation method semantics.
        mixed.penalize_abbreviations();
        assert_eq!(mixed.supplemental_score, input.supplemental_score - 1024);
    }

    #[test]
    fn everyday_layer_only_classifies_existing_builtin_words_without_changing_weights() {
        let raw = Lexicon::from_tsv(BUILTIN_LEXICON).unwrap();
        let builtin = Lexicon::builtin().unwrap();
        assert_eq!(raw.len(), builtin.len());
        let mut classified = std::collections::HashSet::new();
        for (key, entries) in raw.entries.iter() {
            let ranked = &builtin.entries[key];
            assert_eq!(entries.len(), ranked.len());
            for entry in entries {
                let actual = ranked
                    .iter()
                    .find(|e| e.text == entry.text && e.reading == entry.reading)
                    .unwrap();
                assert_eq!(actual.weight, entry.weight);
                assert!(!entry.everyday, "custom TSV must remain unclassified");
                if actual.everyday {
                    assert!(actual.reading.contains('\''));
                    assert_eq!(
                        actual.reading.split('\'').count(),
                        actual.text.chars().count()
                    );
                    classified.insert(actual.text.clone());
                }
            }
            assert!(ranked
                .windows(2)
                .all(|w| !rank_entries(&w[0], &w[1]).is_gt()));
        }
        let expected: std::collections::HashSet<_> = EVERYDAY_PAIRS
            .split_whitespace()
            .map(|pair| pair.split_once('=').unwrap().1)
            .chain(
                EVERYDAY_PHRASES
                    .lines()
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(|line| line.split('\t').nth(1).unwrap()),
            )
            .filter(|text| {
                raw.entries.values().flatten().any(|entry| {
                    entry.text == *text
                        && entry.reading.contains('\'')
                        && entry.reading.split('\'').count() == entry.text.chars().count()
                })
            })
            .collect();
        assert_eq!(
            classified
                .iter()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
        for words in builtin.initials.values() {
            assert!(words
                .windows(2)
                .all(|w| !rank_entries(&w[0], &w[1]).is_gt()));
        }
    }

    #[test]
    fn builtin_snapshots_keep_classes_and_user_weights_separate() {
        let base = Lexicon::builtin().unwrap();
        let before = base.lookup("nihao");
        let overlay = base
            .with_overlay("ni'hao\t你好\t4294967295\nni'hao\t私词\t4294967295")
            .unwrap();
        let entries = &overlay.entries["nihao"];
        let hello = entries.iter().find(|e| e.text == "你好").unwrap();
        let private = entries.iter().find(|e| e.text == "私词").unwrap();
        assert!(hello.everyday);
        assert!(!private.everyday);
        assert_eq!(hello.weight, u32::MAX);
        assert_eq!(private.weight, u32::MAX);
        assert_eq!(overlay.lookup("nihao")[0].text, "你好");
        let learned = overlay.with_learning(&[(
            super::super::LearningEvent {
                pinyin: "ni'hao".into(),
                text: "私词".into(),
            },
            u32::MAX,
        )]);
        assert_eq!(learned.entries["nihao"], overlay.entries["nihao"]);
        assert_eq!(base.lookup("nihao"), before);
        assert_eq!(Lexicon::builtin().unwrap().lookup("nihao"), before);
    }

    #[test]
    fn builtin_and_clones_share_the_read_only_table() {
        let first = Lexicon::builtin().unwrap();
        let second = Lexicon::builtin().unwrap();
        let snapshot = first.clone();
        assert!(Arc::ptr_eq(&first.entries, &second.entries));
        assert!(Arc::ptr_eq(&first.entries, &snapshot.entries));
        assert!(Arc::ptr_eq(&first.initials, &second.initials));
        assert!(Arc::ptr_eq(&first.initials, &snapshot.initials));
        assert!(Arc::ptr_eq(&first.syllables, &second.syllables));
        assert!(Arc::ptr_eq(&first.syllables, &snapshot.syllables));
        let custom = Lexicon::from_tsv("ni\t你\t1").unwrap();
        assert!(!Arc::ptr_eq(&first.entries, &custom.entries));
    }

    #[test]
    fn duplicate_text_keeps_highest_weight_even_when_separated_by_other_weights() {
        let lexicon = Lexicon::from_tsv("ni\t你\t100\nni\t尼\t80\nni\t你\t50").unwrap();
        let candidates = lexicon.lookup("ni");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].text, "你");
        assert_eq!(lexicon.entries["ni"][0].weight, 100);
    }

    #[test]
    fn mixed_index_snapshots_keep_overlay_and_learning_isolated() {
        let base = Lexicon::from_tsv("ni'hao\t你好\t100\nni'hao\t拟好\t1").unwrap();
        assert_eq!(base.mixed_choices("nih")[0].candidate.text, "你好");
        let overlay = base.with_overlay("ni'hao\t私好\t200").unwrap();
        assert_eq!(overlay.mixed_choices("nih")[0].candidate.text, "私好");
        let learned = base.with_learning(&[(
            super::super::LearningEvent {
                pinyin: "ni'hao".into(),
                text: "拟好".into(),
            },
            1,
        )]);
        assert_eq!(learned.mixed_choices("nih")[0].candidate.text, "拟好");
        assert_eq!(base.mixed_choices("nih")[0].candidate.text, "你好");
    }

    #[test]
    fn limiting_initial_materialization_preserves_rank_and_complete_whole_choices() {
        let lexicon = Lexicon::builtin().unwrap();
        for raw in ["ys", "nh", "sm", "zg", "n'h", "unknown"] {
            let complete = lexicon.initial_choices(raw);
            for limit in [0, 1, 4, usize::MAX] {
                let expected: Vec<_> = complete.iter().take(limit).cloned().collect();
                let actual = lexicon.initial_choices_limited(raw, limit);
                // Include score, canonical reading, consumed and learning identity.
                assert_eq!(format!("{actual:?}"), format!("{expected:?}"), "{raw}");
            }
        }
        assert!(lexicon.initial_choices("ys").len() > 4);
        for raw in ["shenmeys", "ninh", "shi", "n'h"] {
            let mut expected = lexicon.prefix_choices_limited(raw, 4);
            for end in 1..=raw.len() {
                if raw.as_bytes().get(end) == Some(&b'\'') {
                    continue;
                }
                expected.extend(
                    lexicon
                        .initial_choices(&raw[..end])
                        .into_iter()
                        .take(4)
                        .map(|mut choice| {
                            choice.abbreviated = choice
                                .candidate
                                .comment
                                .split('\'')
                                .filter(|s| s.len() > 1)
                                .count();
                            choice.penalize_abbreviations();
                            choice
                        }),
                );
            }
            assert_eq!(
                format!("{:?}", lexicon.graph_choices(raw, 4)),
                format!("{expected:?}"),
                "{raw}"
            );
        }
    }

    #[test]
    fn mixed_trie_is_linear_not_a_spelling_product_and_query_is_bounded() {
        let reading = vec!["shen"; 40].join("'");
        let text = "什".repeat(40);
        let lexicon = Lexicon::from_tsv(&format!("{reading}\t{text}\t1")).unwrap();
        assert_eq!(lexicon.syllables.nodes.len(), 41);
        assert_eq!(lexicon.mixed_choices(&"s".repeat(40)).len(), 1);
        assert!(lexicon.mixed_choices(&"s".repeat(128)).is_empty());
        let mut tsv = String::new();
        for length in 2..=60 {
            tsv.push_str(&format!(
                "{}\t{}\t1\n",
                vec!["a"; length].join("'"),
                "啊".repeat(length)
            ));
        }
        let ambiguous = Lexicon::from_tsv(&tsv).unwrap();
        assert!(ambiguous.mixed_choices(&"a".repeat(60)).len() <= MIXED_BUDGET);
    }

    #[test]
    fn every_baseline_character_is_reachable_through_the_actual_lookup() {
        use std::collections::HashSet;
        let lexicon = Lexicon::builtin().unwrap();
        let reachable: HashSet<_> = lexicon
            .entries
            .keys()
            .flat_map(|key| lexicon.lookup(key))
            .filter(|candidate| candidate.text.chars().count() == 1)
            .map(|candidate| candidate.text.chars().next().unwrap())
            .collect();
        let mut baseline = HashSet::new();
        for line in std::str::from_utf8(
            flower_dict::asset("data/pinyin/sources/kMandarin_8105.txt").unwrap(),
        )
        .unwrap()
        .lines()
        {
            if !line.starts_with("U+") {
                continue;
            }
            let token = line.split(':').next().unwrap();
            let char = char::from_u32(u32::from_str_radix(&token[2..], 16).unwrap()).unwrap();
            assert!(baseline.insert(char), "duplicate baseline character");
            assert!(
                reachable.contains(&char),
                "unreachable baseline character {token}"
            );
        }
        assert_eq!(baseline.len(), 8105);
        assert!(reachable.len() > 40000);
    }

    #[test]
    fn multi_readings_umlaut_and_supplementary_characters_are_available() {
        let lexicon = Lexicon::builtin().unwrap();
        for (key, text) in [
            ("chong", "重"),
            ("zhong", "重"),
            ("hang", "行"),
            ("xing", "行"),
            ("lv", "吕"),
            ("nv", "女"),
            ("lve", "略"),
            ("nve", "虐"),
            ("he", "𠀀"),
        ] {
            assert!(
                lexicon
                    .lookup(key)
                    .iter()
                    .any(|candidate| candidate.text == text),
                "missing {text} for {key}"
            );
        }
    }
}
