use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use flower::engine::LearningEvent;
use flower::user_dictionary::{
    DictionaryError, DictionaryStore, LearningWorker, MAX_FILE_BYTES, MAX_LEARNING_COUNT,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[test]
fn auto_phrase_validation_bounds_and_snapshot_max_weights() {
    use flower::user_dictionary::{AutoPhrase, DictionaryEntry, LearnedEntry, UserDictionary};
    let fixture = Fixture::new();
    let store = fixture.store();
    let mut dictionary = UserDictionary {
        auto_phrases: vec![AutoPhrase {
            pinyin: "ni'hao".into(),
            text: "你好".into(),
        }],
        entries: vec![DictionaryEntry {
            pinyin: "ni'hao".into(),
            text: "你好".into(),
            weight: 20000,
        }],
        ..UserDictionary::default()
    };
    assert_eq!(dictionary.lexicon_tsv().unwrap(), "ni'hao\t你好\t20000\n");
    dictionary.entries[0].weight = 1;
    assert_eq!(dictionary.lexicon_tsv().unwrap(), "ni'hao\t你好\t16384\n");
    assert_eq!(dictionary.base_lexicon_tsv().unwrap(), "ni'hao\t你好\t1\n");
    let bytes = serde_json::to_vec(&dictionary).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert_eq!(store.load().unwrap(), dictionary);
    let long = LearningEvent {
        pinyin: vec!["zhang"; 26].join("'"),
        text: "张".repeat(26),
    };
    let too_many = LearningEvent {
        pinyin: vec!["ni"; 33].join("'"),
        text: "你".repeat(33),
    };
    for invalid in [long, too_many] {
        assert!(store
            .commit_if_generation(0, &[promotion(4)], &[invalid])
            .is_err());
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }
    store
        .commit_if_generation(
            0,
            &[],
            &[LearningEvent {
                pinyin: vec!["ni"; 32].join("'"),
                text: "你".repeat(32),
            }],
        )
        .unwrap();
    assert_eq!(store.load().unwrap().auto_phrases.len(), 2);
    let mut value = serde_json::to_value(&dictionary).unwrap();
    value["auto_phrases"][0]["extra"] = serde_json::json!(true);
    fs::write(store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(store.load().is_err());
    dictionary
        .auto_phrases
        .push(dictionary.auto_phrases[0].clone());
    fs::write(store.path(), serde_json::to_vec(&dictionary).unwrap()).unwrap();
    assert!(store.load().is_err());
    dictionary.auto_phrases.pop();
    dictionary.entries.clear();
    dictionary.learned = (0..100_000u32)
        .map(|n| LearnedEntry {
            pinyin: "ni'hao".into(),
            text: format!(
                "{}{}",
                char::from_u32(0x4e00 + n / 20000).unwrap(),
                char::from_u32(0x4e00 + n % 20000).unwrap()
            ),
            count: 1,
        })
        .collect();
    fs::write(store.path(), serde_json::to_vec(&dictionary).unwrap()).unwrap();
    assert!(matches!(store.load(), Err(DictionaryError::LimitExceeded)));
}

#[test]
fn old_schemas_preserve_all_fields_without_rewriting() {
    let fixture = Fixture::new();
    let store = fixture.store();
    for version in [1, 2] {
        let mut dictionary = flower::user_dictionary::UserDictionary {
            schema_version: version,
            generation: 29,
            adaptive_ranking_enabled: false,
            learning_enabled: true,
            rankings: vec![promotion(1).order],
            learned: vec![flower::user_dictionary::LearnedEntry {
                pinyin: "ni'hao".into(),
                text: "你好".into(),
                count: 9,
            }],
            ..flower::user_dictionary::UserDictionary::default()
        };
        let mut value = serde_json::to_value(&dictionary).unwrap();
        value.as_object_mut().unwrap().remove("auto_phrases");
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(store.path(), &bytes).unwrap();
        dictionary.schema_version = 3;
        assert_eq!(store.load().unwrap(), dictionary);
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
        assert!(dictionary.auto_phrase_tsv().unwrap().is_empty());
    }
}

#[test]
fn compound_commit_is_atomic_and_worker_drains_with_generation_gates() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store
        .commit_if_generation(0, &[promotion(4)], &[event()])
        .unwrap();
    let original = fs::read(store.path()).unwrap();
    for (pinyin, text) in [
        ("ni", "你"),
        ("n'h", "你好"),
        ("Ni'hao", "你好"),
        ("ni hao", "你好"),
        ("ni''hao", "你好"),
        ("ni'hao", "你！"),
        ("ni'hao", "你㐀"),
        ("ni'hao", "你𠀀"),
    ] {
        let invalid = LearningEvent {
            pinyin: pinyin.into(),
            text: text.into(),
        };
        assert!(store
            .commit_if_generation(0, &[promotion(4)], &[event(), invalid])
            .is_err());
        assert_eq!(fs::read(store.path()).unwrap(), original);
    }
    let mut worker = LearningWorker::start(store.clone(), 0).unwrap();
    assert!(matches!(
        worker.submit_commit(&[promotion(1)], &vec![event(); 1024]),
        Err(DictionaryError::LimitExceeded)
    ));
    worker.submit_commit(&[promotion(4)], &[event()]).unwrap();
    worker.shutdown().unwrap();
    let dictionary = store.load().unwrap();
    assert_eq!(dictionary.auto_phrases.len(), 1);
    assert_eq!(
        dictionary.auto_phrase_tsv().unwrap(),
        "ni'hao\t你好\t16384\n"
    );
    assert!(dictionary.base_lexicon_tsv().unwrap().is_empty());
    assert_eq!(
        dictionary.lexicon_tsv().unwrap(),
        dictionary.auto_phrase_tsv().unwrap()
    );
    let output = fixture.path("export.tsv");
    store.export_tsv(&output, None).unwrap();
    assert_eq!(
        fs::read_to_string(output).unwrap(),
        dictionary.lexicon_tsv().unwrap()
    );
    store.set_learning_enabled(false).unwrap();
    store
        .commit_if_generation(1, &[promotion(4)], &[event()])
        .unwrap();
    assert_eq!(store.load().unwrap().auto_phrases, dictionary.auto_phrases);
    store.clear_learning().unwrap();
    store
        .commit_if_generation(0, &[promotion(4)], &[event()])
        .unwrap();
    let cleared = store.load().unwrap();
    assert_eq!(cleared.generation, 2);
    assert!(
        cleared.auto_phrases.is_empty()
            && cleared.rankings.is_empty()
            && cleared.learned.is_empty()
    );
}

#[test]
fn automatic_recent_list_capacity_and_concurrent_merges() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let phrase = |index: u32| LearningEvent {
        pinyin: "ni'hao".into(),
        text: format!("你{}", char::from_u32(0x4e00 + index).unwrap()),
    };
    for batch in 0..4 {
        let events: Vec<_> = (batch * 1024..(batch + 1) * 1024).map(phrase).collect();
        store.commit_if_generation(0, &[], &events).unwrap();
    }
    store
        .commit_if_generation(0, &[], &[phrase(0), phrase(4096)])
        .unwrap();
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.auto_phrases.len(), 4096);
    assert_eq!(snapshot.auto_phrases[0].text, phrase(2).text);
    assert_eq!(snapshot.auto_phrases[4094].text, phrase(0).text);
    std::thread::scope(|scope| {
        for index in 4097..4101 {
            let store = &store;
            let event = phrase(index);
            scope.spawn(move || {
                store
                    .commit_if_generation(0, &[promotion(4)], &[event])
                    .unwrap()
            });
        }
    });
    let snapshot = store.load().unwrap();
    for index in 4097..4101 {
        assert!(snapshot
            .auto_phrases
            .iter()
            .any(|p| p.text == phrase(index).text));
    }
    assert_eq!(snapshot.rankings[0].candidates[0].text, "五");
}

fn promotion(selected: usize) -> flower::engine::PromotionEvent {
    use flower::engine::{PromotionEvent, RankingCandidate, RankingOrder};
    let candidates: Vec<_> = ["一", "二", "三", "四", "五"]
        .into_iter()
        .map(|text| RankingCandidate {
            pinyin: "ni".into(),
            text: text.into(),
            consumed: 2,
        })
        .collect();
    PromotionEvent {
        selected: candidates[selected].clone(),
        order: RankingOrder {
            input: "ni".into(),
            candidates,
        },
    }
}

#[test]
fn schema_one_migrates_only_on_update() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let bytes = br#"{"schema_version":1,"generation":7,"learning_enabled":false,"entries":[],"domains":[],"learned":[]}"#;
    fs::write(store.path(), bytes).unwrap();
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.schema_version, 3);
    assert!(snapshot.adaptive_ranking_enabled);
    assert!(!snapshot.learning_enabled);
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    store.promote_if_generation(7, &[promotion(4)]).unwrap();
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.rankings[0].candidates[3].text, "五");
    assert_eq!(snapshot.generation, 7);
    let json: serde_json::Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(json["schema_version"], 3);
    assert_eq!(snapshot, store.load().unwrap());
}

#[test]
fn promotions_merge_latest_order_and_generation_blocks_resurrection() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let other = DictionaryStore::new(store.path());
    store.promote_if_generation(0, &[promotion(4)]).unwrap();
    other.promote_if_generation(0, &[promotion(4)]).unwrap();
    assert_eq!(
        store.load().unwrap().rankings[0]
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["一", "二", "五", "三", "四"]
    );
    store.promote_if_generation(0, &[promotion(2)]).unwrap();
    assert_eq!(store.load().unwrap().rankings[0].candidates[2].text, "三");
    store.clear_learning().unwrap();
    store.promote_if_generation(0, &[promotion(4)]).unwrap();
    assert!(store.load().unwrap().rankings.is_empty());
    assert!(store.load().unwrap().adaptive_ranking_enabled);
    store.set_learning_enabled(false).unwrap();
    store.promote_if_generation(2, &[promotion(4)]).unwrap();
    let snapshot = store.load().unwrap();
    assert!(!snapshot.adaptive_ranking_enabled);
    assert!(!snapshot.learning_enabled);
    assert!(snapshot.rankings.is_empty());
}

#[test]
fn invalid_promotion_batch_preserves_file_and_worker_is_bounded() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.promote_if_generation(0, &[promotion(4)]).unwrap();
    let bytes = fs::read(store.path()).unwrap();
    let mut invalid = promotion(1);
    invalid.order.candidates[0].consumed = 1;
    assert!(store
        .promote_if_generation(0, &[promotion(4), invalid])
        .is_err());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    for input in ["", "'ni", "n''i", "Ni", "n i"] {
        let mut event = promotion(1);
        event.order.input = input.into();
        assert!(store.promote_if_generation(0, &[event]).is_err());
    }
    let mut duplicate = promotion(1);
    duplicate.order.candidates.push(duplicate.selected.clone());
    assert!(store.promote_if_generation(0, &[duplicate]).is_err());
    let mut oversized = promotion(1);
    oversized.order.candidates = vec![oversized.selected.clone(); 4097];
    let mut worker = LearningWorker::start(store.clone(), 0).unwrap();
    assert!(matches!(
        worker.submit_promotions(&[oversized]),
        Err(DictionaryError::LimitExceeded)
    ));
    worker.submit_promotions(&[promotion(4)]).unwrap();
    worker.shutdown().unwrap();
    assert_eq!(store.load().unwrap().rankings[0].candidates[2].text, "五");
}

#[test]
fn ranking_scope_lru_and_candidate_limits() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let events: Vec<_> = (0..257)
        .map(|index| {
            let mut event = promotion(1);
            event.order.input = format!(
                "n{}{}",
                (b'a' + (index / 26) as u8) as char,
                (b'a' + (index % 26) as u8) as char
            );
            for candidate in &mut event.order.candidates {
                candidate.consumed = 3;
            }
            event.selected = event.order.candidates[1].clone();
            event
        })
        .collect();
    store.promote_if_generation(0, &events).unwrap();
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.rankings.len(), 256);
    assert_eq!(snapshot.rankings[0].input, events[1].order.input);
    store
        .promote_if_generation(0, &[events[1].clone()])
        .unwrap();
    assert_eq!(
        store.load().unwrap().rankings.last().unwrap().input,
        events[1].order.input
    );
    let bytes = fs::read(store.path()).unwrap();
    let mut event = promotion(1);
    event.order.candidates = (0..4096)
        .map(|index| {
            let mut candidate = event.selected.clone();
            candidate.text = index.to_string();
            candidate
        })
        .collect();
    event.selected = event.order.candidates[4095].clone();
    // Twenty-five full scopes exceed the aggregate 100,000-identity limit.
    let batch: Vec<_> = (0..25)
        .map(|index| {
            let mut event = event.clone();
            event.order.input = format!("n{}", (b'a' + index) as char);
            event
        })
        .collect();
    assert!(matches!(
        store.promote_if_generation(0, &batch),
        Err(DictionaryError::LimitExceeded)
    ));
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
}

// The caller can direct all test-owned files to its session scratch.
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::var_os("FLOWER_TEST_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("flower-dictionary-tests"))
            .join(format!(
                "dictionary-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn store(&self) -> DictionaryStore {
        DictionaryStore::new(self.path("dictionary.json"))
    }
    fn tsv(&self, text: &str) -> PathBuf {
        let path = self.path("import.tsv");
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn event() -> LearningEvent {
    LearningEvent {
        pinyin: "ni'hao".into(),
        text: "你好".into(),
    }
}

#[test]
fn missing_load_has_no_side_effect_and_learning_defaults_off() {
    let fixture = Fixture::new();
    let store = DictionaryStore::new(fixture.path("missing").join("dictionary.json"));
    let dictionary = store.load().unwrap();
    assert_eq!(dictionary.schema_version, 3);
    assert!(dictionary.adaptive_ranking_enabled);
    assert!(!dictionary.learning_enabled);
    assert_eq!(dictionary.generation, 0);
    assert!(dictionary.lexicon_tsv().unwrap().is_empty());
    assert!(!fixture.path("missing").exists());
    assert_eq!(
        store.path(),
        fixture.path("missing").join("dictionary.json")
    );
}

#[test]
fn personal_and_domain_round_trip_preserves_boundaries() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store
        .import_tsv(&fixture.tsv("ni hao\t你好\t17\n"), None)
        .unwrap();
    store
        .import_tsv(&fixture.tsv("zhong'guo\t中国\t29\n"), Some("医学"))
        .unwrap();
    let dictionary = store.load().unwrap();
    assert_eq!(dictionary.entries[0].pinyin, "ni'hao");
    assert_eq!(dictionary.generation, 2);
    assert!(!dictionary.domains[0].enabled);
    assert_eq!(dictionary.lexicon_tsv().unwrap(), "ni'hao\t你好\t17\n");
    store.set_domain_enabled("医学", true).unwrap();
    assert!(store
        .load()
        .unwrap()
        .lexicon_tsv()
        .unwrap()
        .contains("zhong'guo\t中国\t29\n"));
    let personal = fixture.path("personal.tsv");
    let domain = fixture.path("domain.tsv");
    store.export_tsv(&personal, None).unwrap();
    store.export_tsv(&domain, Some("医学")).unwrap();
    let usable = "ni'hao\t你好\t17\nzhong'guo\t中国\t29\n";
    assert_eq!(fs::read_to_string(&personal).unwrap(), usable);
    assert_eq!(
        fs::read_to_string(&domain).unwrap(),
        "zhong'guo\t中国\t29\n"
    );
    assert!(store.export_tsv(&personal, None).is_err());
    assert_eq!(fs::read_to_string(personal).unwrap(), usable);
    store.remove_entry("ni hao", "你好", None).unwrap();
    assert!(store.load().unwrap().entries.is_empty());
    assert_eq!(store.load().unwrap().domains[0].entries.len(), 1);
    store.set_domain_enabled("医学", false).unwrap();
    assert!(store.load().unwrap().lexicon_tsv().unwrap().is_empty());
}

#[test]
fn bad_tsv_is_transactional_and_never_advances_generation() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.import_tsv(&fixture.tsv("ni\t你\t1\n"), None).unwrap();
    let original = fs::read(store.path()).unwrap();
    for bad in [
        "hao\t好\t2\nni'hao\t你好\t0\n",
        "ni\t你好\t1\n",
        "ni''hao\t你好\t1\n",
        "Ni\t你\t1\n",
        "ni\tA\t1\n",
        "ni\t你\u{fe0f}\t1\n",
        "ni\t你\t4294967296\n",
        "ni\t你\t1\textra\n",
        "ni\t你\t+1\n",
        "ni\t你\n",
    ] {
        assert!(store.import_tsv(&fixture.tsv(bad), None).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), original);
    }
    let path = fixture.path("invalid-utf8.tsv");
    fs::write(&path, [0xff, 0xfe]).unwrap();
    assert!(store.import_tsv(&path, None).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), original);
}

#[test]
fn supplementary_han_is_one_scalar_and_word_length_is_bounded() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.import_tsv(&fixture.tsv("he\t𠀀\t1\n"), None).unwrap();
    let valid = format!("{}\t{}\t1\n", vec!["ni"; 32].join("'"), "你".repeat(32));
    store.import_tsv(&fixture.tsv(&valid), None).unwrap();
    let original = fs::read(store.path()).unwrap();
    let invalid = format!("{}\t{}\t1\n", vec!["ni"; 33].join("'"), "你".repeat(33));
    assert!(store.import_tsv(&fixture.tsv(&invalid), None).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), original);
}

#[test]
fn corrupt_unknown_and_oversized_data_are_not_replaced() {
    let fixture = Fixture::new();
    let store = fixture.store();
    for bytes in [
        b"{not json".to_vec(),
        br#"{"schema_version":4,"generation":0,"learning_enabled":false,"entries":[],"domains":[],"learned":[]}"#.to_vec(),
        vec![b' '; MAX_FILE_BYTES + 1],
    ] {
        fs::write(store.path(), &bytes).unwrap();
        assert!(store.load().is_err());
        assert!(store.clear_learning().is_err());
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }
    let oversized = fixture.path("large.tsv");
    fs::write(&oversized, vec![b' '; MAX_FILE_BYTES + 1]).unwrap();
    assert!(matches!(
        store.import_tsv(&oversized, None),
        Err(DictionaryError::LimitExceeded)
    ));
}

#[test]
fn old_generations_cannot_restore_cleared_or_disabled_learning() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.set_learning_enabled(true).unwrap();
    let old_generation = store.load().unwrap().generation;
    store
        .learn_if_generation(old_generation, &[event(), event()])
        .unwrap();
    let dictionary = store.load().unwrap();
    assert_eq!(dictionary.learned.len(), 1);
    assert_eq!(dictionary.learned[0].count, 2);
    assert!(dictionary.lexicon_tsv().unwrap().is_empty());
    store.clear_learning().unwrap();
    store
        .learn_if_generation(old_generation, &[event()])
        .unwrap();
    assert!(store.load().unwrap().learned.is_empty());
    let generation = store.load().unwrap().generation;
    store.set_learning_enabled(false).unwrap();
    store.learn_if_generation(generation, &[event()]).unwrap();
    store.set_learning_enabled(true).unwrap();
    store.learn_if_generation(generation, &[event()]).unwrap();
    assert!(store.load().unwrap().learned.is_empty());
}

#[test]
fn learning_is_exact_atomic_and_saturating() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.set_learning_enabled(true).unwrap();
    let generation = store.load().unwrap().generation;
    let batch = vec![event(); 1024];
    for _ in 0..10 {
        store.learn_if_generation(generation, &batch).unwrap();
    }
    let dictionary = store.load().unwrap();
    assert_eq!(dictionary.generation, generation);
    assert_eq!(dictionary.learned.len(), 1);
    assert_eq!(dictionary.learned[0].text, "你好");
    assert_eq!(dictionary.learned[0].count, MAX_LEARNING_COUNT);
    let original = fs::read(store.path()).unwrap();
    let invalid = LearningEvent {
        pinyin: "nihao".into(),
        text: "你好".into(),
    };
    assert!(store
        .learn_if_generation(generation, &[event(), invalid])
        .is_err());
    assert_eq!(fs::read(store.path()).unwrap(), original);
}

#[test]
fn independent_stores_merge_under_the_lock() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.set_learning_enabled(true).unwrap();
    let generation = store.load().unwrap().generation;
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let store = DictionaryStore::new(store.path());
            std::thread::spawn(move || {
                for _ in 0..8 {
                    store.learn_if_generation(generation, &[event()]).unwrap();
                }
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|thread| thread.join()).collect();
    assert!(
        results.iter().all(Result::is_ok),
        "dictionary writer failed"
    );
    assert_eq!(store.load().unwrap().learned[0].count, 32);
}

#[test]
fn cooperative_public_loads_and_updates_preserve_complete_monotonic_snapshots() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.set_learning_enabled(true).unwrap();
    let generation = store.load().unwrap().generation;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || -> Result<(), DictionaryError> {
                barrier.wait();
                if index < 4 {
                    for _ in 0..8 {
                        store.learn_if_generation(generation, &[event()])?;
                    }
                } else {
                    let mut previous_count = 0;
                    for _ in 0..32 {
                        let snapshot = store.load()?;
                        assert_eq!(snapshot.schema_version, 3);
                        assert_eq!(snapshot.generation, generation);
                        assert!(snapshot.learning_enabled);
                        assert!(snapshot.learned.len() <= 1);
                        let count = snapshot.learned.first().map_or(0, |entry| {
                            assert_eq!(entry.pinyin, "ni'hao");
                            assert_eq!(entry.text, "你好");
                            entry.count
                        });
                        assert!((previous_count..=32).contains(&count));
                        previous_count = count;
                        std::thread::yield_now();
                    }
                }
                Ok(())
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|thread| thread.join()).collect();
    assert!(
        results.iter().all(|result| matches!(result, Ok(Ok(())))),
        "cooperative reader or writer failed: {results:?}"
    );
    assert_eq!(store.load().unwrap().learned[0].count, 32);
}

#[test]
fn worker_shutdown_and_drop_drain_and_join() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.set_learning_enabled(true).unwrap();
    let generation = store.load().unwrap().generation;
    let mut worker = LearningWorker::start(store.clone(), generation).unwrap();
    worker.submit(vec![event(); 5]).unwrap();
    worker.shutdown().unwrap();
    worker.shutdown().unwrap();
    assert!(matches!(
        worker.submit(vec![event()]),
        Err(DictionaryError::WorkerStopped)
    ));
    assert_eq!(store.load().unwrap().learned[0].count, 5);
    {
        let worker = LearningWorker::start(store.clone(), generation).unwrap();
        worker.submit(vec![event(); 3]).unwrap();
    }
    assert_eq!(store.load().unwrap().learned[0].count, 8);
}

#[test]
fn worker_retains_fixed_failure_and_batch_limit() {
    let fixture = Fixture::new();
    let store = fixture.store();
    fs::write(store.path(), b"bad json").unwrap();
    let mut worker = LearningWorker::start(store, 0).unwrap();
    assert!(matches!(
        worker.submit(vec![event(); 1025]),
        Err(DictionaryError::LimitExceeded)
    ));
    worker.submit(vec![event()]).unwrap();
    let error = worker.shutdown().unwrap_err();
    assert!(matches!(error, DictionaryError::WorkerFailed));
    assert_eq!(error.to_string(), "user dictionary learning worker failed");
    assert!(worker.shutdown().is_err());
}

#[test]
fn malformed_json_entries_and_record_limit_are_rejected() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let mut dictionary = serde_json::json!({
        "schema_version": 1, "generation": 0, "learning_enabled": false,
        "entries": [], "domains": [], "learned": []
    });
    for entry in [
        serde_json::json!({"pinyin":"ni","text":"你","weight":0}),
        serde_json::json!({"pinyin":"ni hao","text":"你好","weight":1}),
        serde_json::json!({"pinyin":"ni","text":"a","weight":1}),
    ] {
        dictionary["entries"] = serde_json::json!([entry]);
        fs::write(store.path(), serde_json::to_vec(&dictionary).unwrap()).unwrap();
        assert!(store.load().is_err());
    }
    dictionary["entries"] = serde_json::Value::Array(vec![
        serde_json::json!({"pinyin":"ni","text":"你","weight":1});
        100_001
    ]);
    fs::write(store.path(), serde_json::to_vec(&dictionary).unwrap()).unwrap();
    assert!(matches!(store.load(), Err(DictionaryError::LimitExceeded)));
}

#[test]
fn generation_exhaustion_preserves_old_file() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let dictionary = flower::user_dictionary::UserDictionary {
        generation: u64::MAX,
        ..Default::default()
    };
    let bytes = serde_json::to_vec(&dictionary).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert!(matches!(
        store.clear_learning(),
        Err(DictionaryError::GenerationExhausted)
    ));
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
}

#[cfg(target_os = "windows")]
#[test]
fn windows_stale_lock_is_reusable_and_live_lock_times_out() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let store = fixture.store();
    let path = fixture.path("dictionary.json.lock");
    fs::write(&path, b"stale").unwrap();
    store.clear_learning().unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let bytes = fs::read(store.path()).unwrap();
    // Spend part of the same budget in the named-mutex layer, then keep the
    // legacy file lock held. Independent ten-second budgets would take 13s.
    let holder = windows_mutex_process::hold_for(store.path(), std::time::Duration::from_secs(3));
    let started = std::time::Instant::now();
    assert!(matches!(
        store.clear_learning(),
        Err(DictionaryError::LockTimeout)
    ));
    assert!(started.elapsed() >= std::time::Duration::from_secs(9));
    assert!(started.elapsed() < std::time::Duration::from_secs(12));
    holder.join().unwrap();
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    drop(lock);
    store.clear_learning().unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn worker_queue_reports_full_without_waiting_for_disk_lock() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let store = fixture.store();
    store.set_learning_enabled(true).unwrap();
    let generation = store.load().unwrap().generation;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(fixture.path("dictionary.json.lock"))
        .unwrap();
    let mut worker = LearningWorker::start(store.clone(), generation).unwrap();
    let started = std::time::Instant::now();
    let mut accepted = 0;
    let mut full = false;
    for _ in 0..64 {
        match worker.submit(vec![event()]) {
            Ok(()) => accepted += 1,
            Err(DictionaryError::QueueFull) => {
                full = true;
                break;
            }
            Err(error) => panic!("unexpected queue status: {error}"),
        }
    }
    assert!(full);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    drop(lock);
    worker.shutdown().unwrap();
    assert_eq!(store.load().unwrap().learned[0].count, accepted);
}

#[cfg(target_os = "windows")]
#[test]
fn windows_network_device_and_alternate_stream_paths_are_rejected() {
    for path in [
        r"\\server\share\dictionary.json",
        r"\\?\UNC\server\share\dictionary.json",
        r"\\.\C:\dictionary.json",
        r"C:\dictionary.json:stream",
    ] {
        assert!(matches!(
            DictionaryStore::new(path).load(),
            Err(DictionaryError::UnsafePath)
        ));
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_reparse_directories_are_rejected_without_touching_target() {
    let fixture = Fixture::new();
    let target = fixture.path("target");
    let link = fixture.path("junction");
    fs::create_dir(&target).unwrap();
    // A local junction does not require symlink privilege or elevation.
    let result = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .output()
        .unwrap();
    assert!(result.status.success(), "could not create test junction");
    let store = DictionaryStore::new(link.join("dictionary.json"));
    assert!(matches!(store.load(), Err(DictionaryError::UnsafePath)));
    assert!(matches!(
        store.clear_learning(),
        Err(DictionaryError::UnsafePath)
    ));
    assert!(!target.join("dictionary.json").exists());
    // Remove only the test junction itself, not its target.
    fs::remove_dir(link).unwrap();
}

#[cfg(unix)]
#[test]
fn symlink_paths_are_rejected() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let target = fixture.path("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, fixture.path("link")).unwrap();
    assert!(matches!(
        DictionaryStore::new(fixture.path("link").join("dictionary.json")).load(),
        Err(DictionaryError::UnsafePath)
    ));
}

#[cfg(target_os = "windows")]
mod windows_mutex_process {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

    struct MutexHandle {
        handle: HANDLE,
        owned: bool,
    }

    impl MutexHandle {
        fn reference_name(path: &std::path::Path) -> Vec<u16> {
            // Independent reference implementation of the documented name:
            // normalized local absolute path, UTF-16LE FNV-1a, fixed seed.
            let normalized: PathBuf = path.components().collect();
            let text = normalized.to_str().unwrap().to_lowercase();
            let mut hash = 14695981039346656037u64;
            for unit in text.encode_utf16() {
                for byte in unit.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(1099511628211);
                }
            }
            format!("Global\\FlowerDictionary-{hash:016x}")
                .encode_utf16()
                .chain(Some(0))
                .collect()
        }

        fn open(path: &std::path::Path) -> Self {
            let name = Self::reference_name(path);
            // SAFETY: Live, null-terminated name and default security settings.
            let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.unwrap();
            Self {
                handle,
                owned: false,
            }
        }

        fn lock(&mut self) {
            // SAFETY: The handle is a live mutex owned by this guard.
            let result = unsafe { WaitForSingleObject(self.handle, 10_000) };
            assert!(result == WAIT_OBJECT_0 || result == WAIT_ABANDONED);
            self.owned = true;
        }
    }

    pub(super) fn hold_for(
        path: &std::path::Path,
        duration: Duration,
    ) -> std::thread::JoinHandle<()> {
        let path = path.to_owned();
        let (ready, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut mutex = MutexHandle::open(&path);
            mutex.lock();
            ready.send(()).unwrap();
            std::thread::sleep(duration);
        });
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        thread
    }

    impl Drop for MutexHandle {
        fn drop(&mut self) {
            // SAFETY: Release only if acquired by this thread, then close once.
            unsafe {
                if self.owned {
                    let _ = ReleaseMutex(self.handle);
                }
                let _ = CloseHandle(self.handle);
            }
        }
    }

    struct Process(Child);

    impl Process {
        fn spawn(fixture: &Fixture, mode: &str, path: &std::path::Path) -> Self {
            Self(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--ignored",
                        "--exact",
                        "windows_mutex_process::helper",
                        "--nocapture",
                    ])
                    .env("FLOWER_DICTIONARY_HELPER_MODE", mode)
                    .env("FLOWER_DICTIONARY_HELPER_PATH", path)
                    .env("FLOWER_DICTIONARY_HELPER_READY", fixture.path("ready"))
                    .env("FLOWER_DICTIONARY_HELPER_DONE", fixture.path("done"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            )
        }

        fn wait_for(&mut self, path: &std::path::Path) {
            let deadline = Instant::now() + Duration::from_secs(15);
            while !path.exists() {
                assert!(self.0.try_wait().unwrap().is_none(), "helper exited early");
                assert!(Instant::now() < deadline, "helper handshake timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // Reap even when the parent assertion fails.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "only invoked as an isolated child process by the mutex tests"]
    fn helper() {
        let mode = std::env::var("FLOWER_DICTIONARY_HELPER_MODE").unwrap();
        let path = PathBuf::from(std::env::var_os("FLOWER_DICTIONARY_HELPER_PATH").unwrap());
        let ready = PathBuf::from(std::env::var_os("FLOWER_DICTIONARY_HELPER_READY").unwrap());
        if mode == "hold" {
            let mut mutex = MutexHandle::open(&path);
            mutex.lock();
            fs::write(ready, b"ready").unwrap();
            // The parent deliberately terminates this process while it owns
            // the mutex. No Drop or ReleaseMutex may run on this path.
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        assert!(mode == "update" || mode == "read");
        fs::write(ready, b"ready").unwrap();
        let store = DictionaryStore::new(path);
        if mode == "read" {
            store.load().unwrap();
        } else {
            store.clear_learning().unwrap();
        }
        fs::write(
            std::env::var_os("FLOWER_DICTIONARY_HELPER_DONE").unwrap(),
            b"done",
        )
        .unwrap();
    }

    #[test]
    fn named_mutex_blocks_an_independent_process_for_the_same_path() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.clear_learning().unwrap();
        let original = fs::read(store.path()).unwrap();
        let mut mutex = MutexHandle::open(store.path());
        mutex.lock();
        // A spelling variation must still enter the same named mutex.
        let spelling = fixture.0.join(".").join("DICTIONARY.JSON");
        let mut child = Process::spawn(&fixture, "update", &spelling);
        child.wait_for(&fixture.path("ready"));
        std::thread::sleep(Duration::from_millis(200));
        assert!(!fixture.path("done").exists());
        assert!(child.0.try_wait().unwrap().is_none());
        assert_eq!(fs::read(store.path()).unwrap(), original);
        drop(mutex);
        child.wait_for(&fixture.path("done"));
        assert!(child.0.wait().unwrap().success());
        assert_eq!(store.load().unwrap().generation, 2);
    }

    #[test]
    fn public_load_waits_for_named_mutex_without_creating_a_disk_lock() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let original = flower::user_dictionary::UserDictionary::default();
        fs::write(store.path(), serde_json::to_vec(&original).unwrap()).unwrap();
        let mut mutex = MutexHandle::open(store.path());
        mutex.lock();
        let mut child = Process::spawn(&fixture, "read", store.path());
        child.wait_for(&fixture.path("ready"));
        std::thread::sleep(Duration::from_millis(150));
        assert!(!fixture.path("done").exists());
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(!fixture.path("dictionary.json.lock").exists());
        drop(mutex);
        child.wait_for(&fixture.path("done"));
        assert!(child.0.wait().unwrap().success());
        assert_eq!(store.load().unwrap(), original);
        assert!(!fixture.path("dictionary.json.lock").exists());
    }

    #[test]
    fn abandoned_named_mutex_is_recovered_after_process_termination() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.clear_learning().unwrap();
        let mut child = Process::spawn(&fixture, "hold", store.path());
        child.wait_for(&fixture.path("ready"));
        // Keep the kernel object alive after the child's last handle closes,
        // so the store must actually handle WAIT_ABANDONED, not a new object.
        let observer = MutexHandle::open(store.path());
        // SAFETY: Live mutex handle; a zero timeout does not block.
        assert_eq!(
            unsafe { WaitForSingleObject(observer.handle, 0) },
            WAIT_TIMEOUT
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        store.clear_learning().unwrap();
        assert_eq!(store.load().unwrap().generation, 2);
        // Successful store completion must release ownership too.
        let mut reacquire = MutexHandle::open(store.path());
        reacquire.lock();
    }

    #[test]
    fn named_mutex_creation_failure_never_falls_back_to_file_lock() {
        use windows::Win32::System::Threading::CreateEventW;
        let fixture = Fixture::new();
        let store = fixture.store();
        store.clear_learning().unwrap();
        let original = fs::read(store.path()).unwrap();
        let name = MutexHandle::reference_name(store.path());
        // An event with this name makes CreateMutexW fail: distinct kernel
        // synchronization object types share the same naming namespace.
        // SAFETY: Valid null-terminated name; default security attributes.
        let event = unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) }.unwrap();
        // This guard only closes the handle because `owned` is false.
        let _event_guard = MutexHandle {
            handle: event,
            owned: false,
        };
        assert!(matches!(
            store.clear_learning(),
            Err(DictionaryError::Io(_))
        ));
        assert_eq!(fs::read(store.path()).unwrap(), original);
    }
}

#[cfg(target_os = "windows")]
mod windows_atomic_publish {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    fn temporary_files(fixture: &Fixture) -> Vec<PathBuf> {
        fs::read_dir(&fixture.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                let name = path.file_name().unwrap().to_string_lossy();
                name.starts_with("dictionary.json.") && name.ends_with(".tmp")
            })
            .collect()
    }

    fn wait_for_closed_temporary(fixture: &Fixture) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let files = temporary_files(fixture);
            assert!(
                files.len() <= 1,
                "publication rebuilt its temporary payload"
            );
            if let Some(path) = files.first() {
                // This opening denies sharing WRITE, so it cannot succeed
                // while the temporary's original write handle is still live.
                if OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .open(path)
                    .is_ok()
                {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "synced temporary was not observed"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn assert_native_publish_denial(result: Result<(), DictionaryError>) {
        match result {
            Err(DictionaryError::Io(error)) => {
                assert!(matches!(error.raw_os_error(), Some(5 | 32 | 33)))
            }
            _ => panic!("publication must return its native denial error"),
        }
    }

    #[test]
    fn short_no_delete_sharing_blocker_releases_and_publication_succeeds() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.clear_learning().unwrap();
        let old = fs::read(store.path()).unwrap();
        let blocker = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(store.path())
            .unwrap();
        std::thread::scope(|scope| {
            let task = scope.spawn(|| store.set_learning_enabled(true));
            wait_for_closed_temporary(&fixture);
            let files = temporary_files(&fixture);
            assert_eq!(files.len(), 1);
            let payload = fs::read(&files[0]).unwrap();
            std::thread::sleep(Duration::from_millis(120));
            assert_eq!(fs::read(store.path()).unwrap(), old);
            assert_eq!(temporary_files(&fixture), files);
            assert_eq!(fs::read(&files[0]).unwrap(), payload);
            drop(blocker);
            task.join().unwrap().unwrap();
        });
        let dictionary = store.load().unwrap();
        assert!(dictionary.learning_enabled);
        assert_eq!(dictionary.generation, 2);
        assert!(temporary_files(&fixture).is_empty());
    }

    #[test]
    fn permanent_no_delete_sharing_blocker_returns_error_and_keeps_old_bytes() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.clear_learning().unwrap();
        let old = fs::read(store.path()).unwrap();
        let _blocker = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(store.path())
            .unwrap();
        std::thread::scope(|scope| {
            let task = scope.spawn(|| store.set_learning_enabled(true));
            wait_for_closed_temporary(&fixture);
            let started = Instant::now();
            assert_native_publish_denial(task.join().unwrap());
            assert!(started.elapsed() >= Duration::from_millis(1800));
            assert!(started.elapsed() < Duration::from_secs(5));
        });
        assert_eq!(fs::read(store.path()).unwrap(), old);
        assert!(temporary_files(&fixture).is_empty());
    }

    struct RestorePermissions {
        path: PathBuf,
        original: fs::Permissions,
    }

    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.path, self.original.clone());
        }
    }

    #[test]
    fn readonly_target_is_not_mistaken_for_success_or_made_writable() {
        let fixture = Fixture::new();
        let store = fixture.store();
        store.clear_learning().unwrap();
        let old = fs::read(store.path()).unwrap();
        let original = fs::metadata(store.path()).unwrap().permissions();
        let _restore = RestorePermissions {
            path: store.path().to_owned(),
            original: original.clone(),
        };
        let mut readonly = original;
        readonly.set_readonly(true);
        fs::set_permissions(store.path(), readonly).unwrap();
        let started = Instant::now();
        assert_native_publish_denial(store.set_learning_enabled(true));
        assert!(started.elapsed() >= Duration::from_millis(1900));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(fs::metadata(store.path()).unwrap().permissions().readonly());
        assert_eq!(fs::read(store.path()).unwrap(), old);
        assert!(temporary_files(&fixture).is_empty());
    }

    #[test]
    fn external_delete_sharing_reader_preserves_atomic_snapshot_outcomes() {
        use std::io::Read;
        let fixture = Fixture::new();
        let store = fixture.store();
        store.clear_learning().unwrap();
        let old = fs::read(store.path()).unwrap();
        let mut reader = OpenOptions::new()
            .read(true)
            .share_mode(5)
            .open(store.path())
            .unwrap();
        let started = Instant::now();
        let result = store.set_learning_enabled(true);
        let mut snapshot = Vec::new();
        reader.read_to_end(&mut snapshot).unwrap();
        assert_eq!(snapshot, old);
        match result {
            Ok(()) => {
                let dictionary = store.load().unwrap();
                assert!(dictionary.learning_enabled);
                assert_eq!(dictionary.generation, 2);
                assert_ne!(fs::read(store.path()).unwrap(), old);
            }
            Err(error) => {
                assert_native_publish_denial(Err(error));
                assert!(started.elapsed() >= Duration::from_millis(1900));
                assert!(started.elapsed() < Duration::from_secs(5));
                assert_eq!(fs::read(store.path()).unwrap(), old);
                assert_eq!(store.load().unwrap().generation, 1);
            }
        }
        assert!(temporary_files(&fixture).is_empty());
    }

    #[test]
    fn native_movefileex_with_read_and_delete_sharing_reader() {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let fixture = Fixture::new();
        let target = fixture.path("native-target.json");
        let temporary = fixture.path("native-payload.tmp");
        fs::write(&target, b"old").unwrap();
        let mut payload = fs::File::create(&temporary).unwrap();
        std::io::Write::write_all(&mut payload, b"new").unwrap();
        payload.sync_all().unwrap();
        drop(payload);
        let mut reader = OpenOptions::new()
            .read(true)
            .share_mode(5)
            .open(&target)
            .unwrap();
        let from: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: Live, null-terminated same-directory paths; no Store code,
        // mutex, directory guards, or retry logic participates in this probe.
        let result = unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        let mut old_snapshot = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut old_snapshot).unwrap();
        assert_eq!(old_snapshot, b"old");
        match result {
            Ok(()) => {
                assert_eq!(fs::read(&target).unwrap(), b"new");
                assert!(!temporary.exists());
            }
            Err(error) => {
                let code = error.code().0 as u32;
                assert!(matches!(code, 0x8007_0005 | 0x8007_0020 | 0x8007_0021));
                assert_eq!(fs::read(&target).unwrap(), b"old");
                assert_eq!(fs::read(&temporary).unwrap(), b"new");
            }
        }
    }
}
