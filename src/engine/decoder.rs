//! Bounded word graph: exact results stay complete, composed alternatives are Top-K.
use super::{Candidate, LearningEvent, Lexicon};
use std::collections::HashSet;
use std::rc::Rc;

const BEAM: usize = 8;
const EDGE_ALTERNATIVES: usize = 4;
const MIXED_DECODE_BUDGET: usize = 16384;

pub(crate) fn lexical_score(weight: u32, characters: usize) -> i64 {
    let logarithm = 31 - weight.leading_zeros();
    let bucket = 1u64 << logarithm;
    let fraction = (u64::from(weight) - bucket) * 64 / bucket;
    i64::from(logarithm) * 64 + fraction as i64 + characters as i64 * 128 - 720
}

#[derive(Clone, Debug)]
pub(crate) struct Choice {
    pub candidate: Candidate,
    pub consumed: usize,
    pub events: Vec<LearningEvent>,
    score: i64,
    pub(crate) supplemental_score: i64,
    pub(crate) abbreviated: usize,
}

impl Choice {
    pub fn word(candidate: Candidate, consumed: usize, weight: u32) -> Self {
        let events = if candidate.comment.split('\'').count() == candidate.text.chars().count()
            && candidate
                .text
                .chars()
                .all(|c| !c.is_ascii() && !c.is_control())
        {
            vec![LearningEvent {
                pinyin: candidate.comment.clone(),
                text: candidate.text.clone(),
            }]
        } else {
            Vec::new()
        };
        let score = lexical_score(weight, candidate.text.chars().count());
        let supplemental_score = score
            - candidate
                .comment
                .split('\'')
                .filter(|s| !s.bytes().any(|c| b"aeiouv".contains(&c)))
                .count() as i64
                * 1024;
        Self {
            candidate,
            consumed,
            events,
            score,
            supplemental_score,
            abbreviated: 0,
        }
    }

    pub(crate) fn penalize_abbreviations(&mut self) {
        self.score -= self.abbreviated as i64 * 512;
        self.supplemental_score -= self.abbreviated as i64 * 512;
    }
}

#[derive(Clone)]
struct Path {
    text: String,
    reading: String,
    // Share immutable word events while pruning; materialize only surviving results.
    events: Vec<Rc<[LearningEvent]>>,
    score: i64,
    supplemental_score: i64,
    words: usize,
    abbreviated: usize,
}

fn trim(paths: &mut Vec<Path>) {
    paths.sort_by(|a, b| {
        a.abbreviated
            .cmp(&b.abbreviated)
            .then_with(|| a.words.cmp(&b.words))
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| a.reading.cmp(&b.reading))
    });
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.text.clone()));
    paths.truncate(BEAM);
}

pub(crate) fn decode(lexicon: &Lexicon, raw: &str) -> Vec<Choice> {
    decode_with_budget(lexicon, raw, &mut 0, MIXED_DECODE_BUDGET)
}

fn supplemental_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
    b.supplemental_score
        .cmp(&a.supplemental_score)
        .then_with(|| a.words.cmp(&b.words))
        .then_with(|| a.abbreviated.cmp(&b.abbreviated))
        .then_with(|| a.text.cmp(&b.text))
        .then_with(|| a.reading.cmp(&b.reading))
}

fn supplemental_step(graph: &mut [Vec<Path>], start: usize, edges: &[Choice]) {
    let mut edges: Vec<_> = edges.iter().collect();
    edges.sort_by(|a, b| {
        b.supplemental_score
            .cmp(&a.supplemental_score)
            .then_with(|| a.abbreviated.cmp(&b.abbreviated))
            .then_with(|| a.candidate.text.cmp(&b.candidate.text))
            .then_with(|| a.candidate.comment.cmp(&b.candidate.comment))
    });
    let mut per_end = std::collections::HashMap::new();
    let mut seen = HashSet::new();
    let paths = graph[start].clone();
    for edge in edges {
        let count = per_end.entry(edge.consumed).or_insert(0usize);
        if *count >= EDGE_ALTERNATIVES
            || !seen.insert((edge.consumed, &edge.candidate.text, &edge.candidate.comment))
        {
            continue;
        }
        *count += 1;
        let end = start + edge.consumed;
        let edge_events: Rc<[LearningEvent]> = edge.events.clone().into();
        for path in &paths {
            let mut events = path.events.clone();
            events.push(edge_events.clone());
            graph[end].push(Path {
                text: format!("{}{}", path.text, edge.candidate.text),
                reading: if path.reading.is_empty() {
                    edge.candidate.comment.clone()
                } else {
                    format!("{}'{}", path.reading, edge.candidate.comment)
                },
                events,
                score: path.score + edge.score,
                supplemental_score: path.supplemental_score + edge.supplemental_score,
                words: path.words + 1,
                abbreviated: path.abbreviated + edge.abbreviated,
            });
        }
        graph[end].sort_by(supplemental_cmp);
        let mut seen = HashSet::new();
        graph[end].retain(|p| seen.insert(p.text.clone()));
        graph[end].truncate(BEAM);
    }
}

fn decode_with_budget(
    lexicon: &Lexicon,
    raw: &str,
    expanded: &mut usize,
    limit: usize,
) -> Vec<Choice> {
    decode_lanes(lexicon, raw, expanded, limit, true)
}

fn decode_lanes(
    lexicon: &Lexicon,
    raw: &str,
    expanded: &mut usize,
    limit: usize,
    supplement: bool,
) -> Vec<Choice> {
    if raw.is_empty()
        || raw.len() > 128
        || !raw.bytes().all(|b| b.is_ascii_lowercase() || b == b'\'')
        || raw.starts_with('\'')
        || raw.contains("''")
    {
        return Vec::new();
    }
    let mut output = lexicon.exact_choices(raw);
    if raw.len() == 1 {
        if !b"aeiouv".contains(&raw.as_bytes()[0]) {
            let mut seen = HashSet::new();
            return lexicon
                .single_initial_choices(raw)
                .into_iter()
                .filter(|choice| seen.insert(choice.candidate.text.clone()))
                .collect();
        }
        let mut seen: HashSet<_> = output
            .iter()
            .map(|choice| choice.candidate.text.clone())
            .collect();
        output.extend(
            lexicon
                .single_initial_choices(raw)
                .into_iter()
                .filter(|choice| seen.insert(choice.candidate.text.clone())),
        );
        return output;
    }
    let mut whole_mixed = Vec::new();
    let mut graph: Vec<Vec<Path>> = vec![Vec::new(); raw.len() + 1];
    graph[0].push(Path {
        text: String::new(),
        reading: String::new(),
        events: Vec::new(),
        score: 0,
        supplemental_score: 0,
        words: 0,
        abbreviated: 0,
    });
    let mut supplemental = vec![Vec::new(); raw.len() + 1];
    if supplement {
        supplemental[0] = graph[0].clone();
    }
    for start in 0..raw.len() {
        if graph[start].is_empty() && supplemental[start].is_empty() {
            continue;
        }
        let mixed = lexicon.mixed_prefix_choices(&raw[start..], expanded, limit);
        if start == 0 {
            whole_mixed = mixed
                .iter()
                .filter(|c| c.consumed == raw.len())
                .cloned()
                .collect();
        }
        let mut edges = lexicon.graph_choices(&raw[start..], EDGE_ALTERNATIVES);
        edges.extend(mixed);
        if !supplemental[start].is_empty() {
            supplemental_step(&mut supplemental, start, &edges);
        }
        // Exact/initial candidates keep their existing rank within equal abbreviation
        // counts. Sort before the per-end cap so less abbreviated mixed edges survive.
        edges.sort_by_key(|edge| edge.abbreviated);
        let mut per_end = std::collections::HashMap::new();
        let mut edge_seen = HashSet::new();
        let paths = graph[start].clone();
        for edge in edges {
            let count = per_end.entry(edge.consumed).or_insert(0usize);
            if *count >= EDGE_ALTERNATIVES {
                continue;
            }
            if !edge_seen.insert((
                edge.consumed,
                edge.candidate.text.clone(),
                edge.candidate.comment.clone(),
            )) {
                continue;
            }
            *count += 1;
            let end = start + edge.consumed;
            let edge_events: Rc<[LearningEvent]> = edge.events.into();
            for path in &paths {
                let mut events = path.events.clone();
                events.push(edge_events.clone());
                graph[end].push(Path {
                    text: format!("{}{}", path.text, edge.candidate.text),
                    reading: if path.reading.is_empty() {
                        edge.candidate.comment.clone()
                    } else {
                        format!("{}'{}", path.reading, edge.candidate.comment)
                    },
                    events,
                    score: path.score + edge.score,
                    supplemental_score: path.supplemental_score + edge.supplemental_score,
                    words: path.words + 1,
                    abbreviated: path.abbreviated + edge.abbreviated,
                });
            }
            trim(&mut graph[end]);
        }
    }
    let mut seen: HashSet<_> = output
        .iter()
        .map(|c| (c.candidate.text.clone(), c.consumed))
        .collect();
    output.extend(
        lexicon
            .initial_choices(raw)
            .into_iter()
            .filter(|c| seen.insert((c.candidate.text.clone(), c.consumed))),
    );
    output.extend(
        whole_mixed
            .into_iter()
            .filter(|c| seen.insert((c.candidate.text.clone(), c.consumed))),
    );
    let legacy = &graph[raw.len()];
    let mut composed: Vec<_> = legacy.iter().filter(|p| p.abbreviated == 0).collect();
    let mut mixed: Vec<_> = legacy
        .iter()
        .filter(|p| p.abbreviated > 0)
        // A duplicate supplemental spelling must not replace legacy word-event
        // segmentation or its original score, even if the new path scores higher.
        .chain(
            supplemental[raw.len()]
                .iter()
                .filter(|p| p.abbreviated > 0 && !legacy.iter().any(|old| old.text == p.text)),
        )
        .collect();
    mixed.sort_by(|a, b| supplemental_cmp(a, b));
    composed.extend(mixed);
    for path in composed {
        if path.words > 1 && seen.insert((path.text.clone(), raw.len())) {
            output.push(Choice {
                candidate: Candidate::new(&path.text, &path.reading),
                consumed: raw.len(),
                events: path
                    .events
                    .iter()
                    .flat_map(|events| events.iter().cloned())
                    .collect(),
                score: path.score,
                supplemental_score: path.supplemental_score,
                abbreviated: path.abbreviated,
            });
        }
    }
    let mut partial: Vec<_> = lexicon
        .prefix_choices(raw)
        .into_iter()
        .filter(|c| c.consumed < raw.len())
        .collect();
    partial.sort_by(|a, b| {
        b.consumed
            .cmp(&a.consumed)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.candidate.text.cmp(&b.candidate.text))
    });
    output.extend(
        partial
            .into_iter()
            .filter(|c| seen.insert((c.candidate.text.clone(), c.consumed))),
    );
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplemental_ties_prefer_words_before_abbreviations_without_changing_legacy() {
        let path = |text: &str, reading: &str, words, abbreviated, supplemental_score| Path {
            text: text.to_owned(),
            reading: reading.to_owned(),
            events: Vec::new(),
            score: 0,
            supplemental_score,
            words,
            abbreviated,
        };
        // Same score: fewer words wins even with more abbreviated syllables.
        let fewer_words = path("z", "z", 2, 5, 100);
        let fewer_abbreviations = path("a", "a", 3, 0, 100);
        let mut final_paths = [fewer_abbreviations.clone(), fewer_words.clone()];
        final_paths.sort_by(supplemental_cmp);
        assert_eq!(final_paths[0].text, "z");

        // The exact comparator used for supplemental pruning keeps that path at
        // the beam boundary; legacy trimming still prefers fewer abbreviations.
        let mut starts: Vec<_> = (0..BEAM)
            .map(|i| path(&format!("a{i}"), "a", 3, 0, 100))
            .collect();
        starts.push(fewer_words);
        let mut legacy = starts.clone();
        trim(&mut legacy);
        assert!(!legacy.iter().any(|p| p.text == "z"));
        let mut graph = vec![starts, Vec::new()];
        let edge = Choice::word(Candidate::new("好", "hao"), 1, 100);
        supplemental_step(&mut graph, 0, &[edge]);
        assert_eq!(graph[1].len(), BEAM);
        assert_eq!(graph[1][0].text, "z好");

        // Score is still primary; remaining equal-score ties are deterministic.
        let mut ties = [
            path("b", "b", 2, 1, 100),
            path("a", "b", 2, 1, 100),
            path("a", "a", 2, 1, 100),
            path("z", "z", 2, 0, 100),
            path("high", "high", 9, 9, 101),
        ];
        ties.sort_by(supplemental_cmp);
        let actual: Vec<_> = ties
            .iter()
            .map(|p| (p.text.as_str(), p.reading.as_str()))
            .collect();
        assert_eq!(
            actual,
            [
                ("high", "high"),
                ("z", "z"),
                ("a", "a"),
                ("a", "b"),
                ("b", "b")
            ]
        );
    }

    #[test]
    fn supplemental_preserves_legacy_candidates_and_fixed_bounds() {
        let lexicon = Lexicon::builtin().unwrap();
        for raw in [
            "zheshism",
            "zhesm",
            "shenmeys",
            "womensr",
            "weixinxx",
            "jintianxx",
            "mingtiangz",
            "dakaisj",
            "jintianxw",
            "mingtiansb",
            "womenxx",
            "dakaidn",
            "shurufasz",
            "nihao",
            "nh",
            "s",
        ] {
            let mut old_states = 0;
            let old = decode_lanes(&lexicon, raw, &mut old_states, MIXED_DECODE_BUDGET, false);
            let mut states = 0;
            let new = decode_lanes(&lexicon, raw, &mut states, MIXED_DECODE_BUDGET, true);
            assert_eq!(states, old_states, "{raw}");
            assert!(states <= MIXED_DECODE_BUDGET);
            for c in &old {
                assert!(
                    new.iter().any(|n| n.candidate == c.candidate
                        && n.consumed == c.consumed
                        && n.events == c.events
                        && n.score == c.score
                        && n.abbreviated == c.abbreviated),
                    "{raw}: {c:?}"
                );
            }
            assert!(new.len() <= old.len() + BEAM, "{raw}");
            let whole = lexicon.exact_choices(raw).len()
                + lexicon.initial_choices(raw).len()
                + lexicon.single_initial_choices(raw).len()
                + lexicon
                    .mixed_prefix_choices(raw, &mut 0, MIXED_DECODE_BUDGET)
                    .iter()
                    .filter(|c| c.consumed == raw.len())
                    .count();
            assert!(
                new.iter().filter(|c| c.consumed == raw.len()).count() <= whole + BEAM * 2,
                "{raw}"
            );
        }
        let mut states = 0;
        assert!(
            decode_with_budget(&lexicon, &"s".repeat(129), &mut states, MIXED_DECODE_BUDGET)
                .is_empty()
        );
        assert_eq!(states, 0);
    }

    #[test]
    fn decode_shares_one_instrumented_mixed_budget_and_preserves_exact_initials() {
        let lexicon = Lexicon::builtin().unwrap();
        for raw in ["shenmeys".to_owned(), "shi".repeat(42), "s".repeat(128)] {
            let mut expanded = 0;
            decode_with_budget(&lexicon, &raw, &mut expanded, MIXED_DECODE_BUDGET);
            assert!(expanded <= MIXED_DECODE_BUDGET, "{raw}: {expanded}");
            let mut exhausted = 0;
            decode_with_budget(&lexicon, &raw, &mut exhausted, 7);
            assert_eq!(exhausted, 7, "{raw}");
        }
        for (raw, expected) in [("nihao", "你好"), ("nh", "你好")] {
            let choices = decode_with_budget(&lexicon, raw, &mut 0, 0);
            assert!(choices
                .iter()
                .any(|c| c.candidate.text == expected && c.consumed == raw.len()));
        }
    }

    #[test]
    fn mixed_rejects_missing_initials_partial_syllables_and_false_boundaries() {
        let lexicon = Lexicon::from_tsv("shen'me'yi'si\t什么意思\t100").unwrap();
        for raw in [
            "shenme",
            "shenmey",
            "shenmeyi",
            "sheme ys",
            "shenme!ys",
            "'shenmeys",
            "shen''meys",
            "she'nmeys",
            "shen'm'eys",
            "shenme'yi'sh",
        ] {
            assert!(
                !decode(&lexicon, raw)
                    .iter()
                    .any(|c| c.candidate.text == "什么意思"),
                "{raw}"
            );
        }
        for raw in ["shenmeys", "shen'me'y's", "shenmeys'"] {
            let choices = decode(&lexicon, raw);
            assert_eq!(choices[0].candidate.text, "什么意思", "{raw}");
            assert_eq!(choices[0].consumed, raw.len());
        }
    }

    #[test]
    fn every_full_initial_product_is_matched_without_storing_products() {
        let lexicon = Lexicon::from_tsv("shen'me'yi'si\t什么意思\t100").unwrap();
        for mask in 0..16 {
            let raw: String = ["shen", "me", "yi", "si"]
                .iter()
                .enumerate()
                .map(|(i, s)| if mask & (1 << i) == 0 { *s } else { &s[..1] })
                .collect();
            assert_eq!(
                decode(&lexicon, &raw)[0].candidate.text,
                "什么意思",
                "{raw}"
            );
        }
    }
}
