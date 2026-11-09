#[path = "../src/ai/inference.rs"]
#[allow(dead_code)]
mod inference;
use flower::ai::Request;
use flower::engine::{Key, KeyEvent, Lexicon, PinyinSession, SpellingIndex};

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    category: String,
    raw: String,
    gold: Option<String>,
    reading: String,
    expectation: String,
}
fn cases() -> Vec<Case> {
    include_str!("fixtures/correction_cases.jsonl")
        .lines()
        .chain(include_str!("fixtures/correction_followups.jsonl").lines())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
#[test]
fn correction_fixture_integrity() {
    let mut ids = std::collections::HashSet::new();
    let mut pairs = std::collections::HashSet::new();
    let rows = cases();
    assert!(rows.len() >= 120);
    for c in rows {
        assert!(ids.insert(c.id.clone()));
        assert!(pairs.insert((c.raw.clone(), c.gold.clone())));
        assert!(["correct", "unchanged", "abstain"].contains(&c.expectation.as_str()));
        if c.expectation == "correct" {
            assert!(c.gold.is_some());
            assert!(
                flower::engine::single_spelling_edit(&c.raw, &c.reading),
                "{}",
                c.id
            );
        }
    }
}
#[test]
#[ignore = "requires fixed offline FLOWER_AI_TEST_BUNDLE and owned FLOWER_CORRECTION_REPORT"]
fn broad_correction_quality_report() {
    use std::io::Write;
    let bundle = std::env::var_os("FLOWER_AI_TEST_BUNDLE").unwrap();
    let report = std::path::PathBuf::from(std::env::var_os("FLOWER_CORRECTION_REPORT").unwrap());
    let mut progress = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report.with_extension("progress.jsonl"))
        .unwrap();
    let mut corrector = inference::Corrector::load(std::path::Path::new(&bundle)).unwrap();
    let lexicon = Lexicon::builtin().unwrap();
    let idx = SpellingIndex::new(&lexicon);
    let mut rows = Vec::new();
    let mut user_pass = std::collections::HashMap::new();
    for c in cases() {
        let start = std::time::Instant::now();
        let mut session = PinyinSession::from_lexicon(lexicon.clone(), 9).unwrap();
        let mut input_error = None;
        for ch in c.raw.chars() {
            if let Err(error) = session.process_key(KeyEvent::plain(Key::Char(ch))) {
                input_error = Some(error.to_string());
                break;
            }
        }
        if let Some(error) = input_error {
            let row = serde_json::json!({"case":c,"input_error":error,
                "passed":c.expectation=="abstain","normal_recall":false,
                "spelling_recall":false,"spelling_proposals":[],"repair_recall":false,
                "ai_hit":false,"response":null,"source":null,"first":null,
                "error":null,"committed":false,"lexical_committed":false,
                "elapsed_ms":start.elapsed().as_millis()});
            writeln!(progress, "{}", row).unwrap();
            rows.push(row);
            continue;
        }
        let first = session.candidates_on_current_page().first().cloned();
        let mut found_lexical = false;
        let mut found_normal = false;
        let mut lexical_committed = false;
        let mut spelling_proposals = Vec::new();
        for page in 0..session.page_count() {
            for (index, item) in session.candidates_on_current_page().iter().enumerate() {
                if item.is_spelling() {
                    spelling_proposals.push(item.text.clone());
                }
                if c.gold.as_deref() == Some(item.text.as_str()) {
                    let mut selection = PinyinSession::from_lexicon(lexicon.clone(), 9).unwrap();
                    for ch in c.raw.chars() {
                        selection
                            .process_key(KeyEvent::plain(Key::Char(ch)))
                            .unwrap();
                    }
                    for _ in 0..page {
                        selection.next_page().unwrap();
                    }
                    let update = selection.select_candidate(index).unwrap();
                    let full_commit = update.commit.as_deref() == c.gold.as_deref()
                        && selection.pending_input().is_empty();
                    if item.is_spelling() && full_commit {
                        found_lexical = true;
                        lexical_committed = true;
                        assert!(selection.pending_learning().is_empty());
                        assert!(selection.pending_promotions().is_empty());
                    } else if !item.is_spelling() && full_commit {
                        found_normal = true;
                    }
                }
            }
            if page + 1 < session.page_count() {
                session.next_page().unwrap();
            }
        }
        // Return to the actual default source; never substitute the gold as source.
        let mut session = PinyinSession::from_lexicon(lexicon.clone(), 9).unwrap();
        for ch in c.raw.chars() {
            session.process_key(KeyEvent::plain(Key::Char(ch))).unwrap();
        }
        let input = session.ai_input();
        let mut response = None;
        let mut error = None;
        let mut ai_hit = false;
        let mut committed = false;
        if let Some(ref i) = input {
            let request = Request {
                id: 1,
                text: i.source.clone(),
                raw: i.raw.clone(),
                reading: i.reading.clone(),
            };
            match corrector.correct_request(&request) {
                Ok(r) => {
                    ai_hit = c
                        .gold
                        .as_deref()
                        .is_some_and(|g| r.corrected.as_deref() == Some(g));
                    if let Some(text) = r.corrected.as_deref() {
                        let update = if r.reading.is_empty() {
                            session.apply_ai_candidate(i, text)
                        } else {
                            session.apply_ai_spelling_candidate(i, text, &r.reading)
                        };
                        if update.is_some() {
                            for page in 0..session.page_count() {
                                if let Some(index) = session
                                    .candidates_on_current_page()
                                    .iter()
                                    .position(|x| x.is_ai())
                                {
                                    let u = session.select_candidate(index).unwrap();
                                    assert_eq!(u.commit.as_deref(), Some(text));
                                    assert!(session.pending_learning().is_empty());
                                    assert!(session.pending_promotions().is_empty());
                                    committed = true;
                                    break;
                                }
                                if page + 1 < session.page_count() {
                                    session.next_page().unwrap();
                                }
                            }
                        }
                    }
                    response = Some(r)
                }
                Err(e) => error = Some(e),
            }
        }
        let source_is_gold = input
            .as_ref()
            .is_some_and(|i| c.gold.as_deref() == Some(i.source.as_str()));
        let corrected = response.as_ref().and_then(|r| r.corrected.as_ref());
        let repair_recall = input.as_ref().is_some_and(|i| {
            c.gold.as_ref().is_some_and(|g| {
                idx.candidates_with_source(&c.raw, 8, &i.source, &i.reading)
                    .iter()
                    .any(|x| &x.text == g)
            })
        });
        let pass = match c.expectation.as_str() {
            "correct" => found_normal || found_lexical || (ai_hit && committed),
            "unchanged" => source_is_gold && corrected.is_none(),
            _ => corrected.is_none(),
        };
        if [
            "xuexo",
            "xueerunishuixingzhou",
            "jintainzaoshang",
            "jintianzaosahng",
        ]
        .contains(&c.raw.as_str())
        {
            user_pass.insert(c.raw.clone(), pass);
        }
        rows.push(serde_json::json!({"case":c,"first":first.map(|x|x.text),"source":input.map(|x|serde_json::json!({"text":x.source,"reading":x.reading})),"normal_recall":found_normal,"spelling_recall":found_lexical,"spelling_proposals":spelling_proposals,"repair_recall":repair_recall,"ai_hit":ai_hit,"response":response,"error":error,"committed":committed,"lexical_committed":lexical_committed,"passed":pass,"elapsed_ms":start.elapsed().as_millis()}));
        writeln!(progress, "{}", rows.last().unwrap()).unwrap();
        eprintln!(
            "case={} pass={pass} lexical={found_lexical} normal={found_normal} ai={ai_hit}",
            rows.last().unwrap()["case"]["id"]
        );
    }
    let output = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"cases":rows,"scope":"fixed builtin engine -> actual source -> fixed model -> explicit AI selection; not real user host"});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report)
        .unwrap();
    file.write_all(serde_json::to_string_pretty(&output).unwrap().as_bytes())
        .unwrap();
    assert_eq!(user_pass.get("xuexo"), Some(&true));
    assert_eq!(user_pass.get("xueerunishuixingzhou"), Some(&true));
    assert_eq!(user_pass.get("jintainzaoshang"), Some(&true));
    assert_eq!(user_pass.get("jintianzaosahng"), Some(&true));
}

#[test]
#[ignore = "requires fixed offline FLOWER_AI_TEST_BUNDLE"]
fn user_examples_probe() {
    let bundle = std::env::var_os("FLOWER_AI_TEST_BUNDLE").unwrap();
    let mut c = inference::Corrector::load(std::path::Path::new(&bundle)).unwrap();
    let idx = SpellingIndex::new(&Lexicon::builtin().unwrap());
    for raw in [
        "xuexo",
        "xueerunishuixingzhou",
        "nihoa",
        "zaijain",
        "gongzuoo",
        "jintain",
    ] {
        let mut s = PinyinSession::new().unwrap();
        for ch in raw.chars() {
            s.process_key(KeyEvent::plain(Key::Char(ch))).unwrap();
        }
        eprintln!(
            "RAW={raw} FIRST={:?} CANDIDATES={:?}",
            s.candidates_on_current_page().first(),
            idx.candidates(raw, 8)
        );
        if let Some(i) = s.ai_input() {
            eprintln!("INPUT={i:?}");
            let r = c
                .correct_request(&Request {
                    id: 1,
                    text: i.source,
                    raw: i.raw,
                    reading: i.reading,
                })
                .unwrap();
            eprintln!("RESULT={r:?}");
        }
    }
}
