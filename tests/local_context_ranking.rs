use flower::engine::{Key, KeyEvent, LearningEvent, PinyinSession};
use flower::user_dictionary::UserDictionary;
use std::sync::Arc;

fn event(text: &str) -> LearningEvent {
    LearningEvent {
        pinyin: "shi".into(),
        text: text.into(),
    }
}

#[test]
fn successful_words_count_decay_and_context_are_independent_of_raw_spelling() {
    let mut data = UserDictionary::default();
    for _ in 0..16 {
        data.remember_selection(&[event("市")], "城").unwrap();
        data.remember_selection(&[event("是")], "还").unwrap();
    }
    assert_eq!(data.learned[0].count, 16);
    assert_eq!(data.selection_sequence, 32);
    let snapshot = Arc::new(data.ranking_snapshot());
    let rank = |prefix: &str| {
        let mut session =
            PinyinSession::from_lexicon_tsv("shi\t是\t1024\nshi\t市\t1024", 9).unwrap();
        session.set_ranking_snapshot(snapshot.clone());
        session.set_host_prefix(prefix);
        for c in "shi".chars() {
            session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
        }
        session.candidates_on_current_page()[0].text.clone()
    };
    assert_eq!(rank("城"), "市");
    assert_eq!(rank("还"), "是");
    let before = snapshot.word_bonus("shi", "市");
    for _ in 0..256 {
        data.remember_selection(&[event("是")], "").unwrap();
    }
    assert!(data.ranking_snapshot().word_bonus("shi", "市") < before);
    assert_eq!(
        data.generation, 0,
        "recency must not consume management generation"
    );
}

#[test]
fn context_changes_pruned_sentence_paths_before_the_final_candidate_list() {
    let mut data = UserDictionary::default();
    for _ in 0..32 {
        data.remember_selection(
            &[
                LearningEvent {
                    pinyin: "shi".into(),
                    text: "市".into(),
                },
                LearningEvent {
                    pinyin: "chang".into(),
                    text: "场".into(),
                },
            ],
            "城",
        )
        .unwrap();
    }
    let mut session = PinyinSession::from_lexicon_tsv(
        "shi\t是\t1024\nshi\t时\t1024\nshi\t事\t1024\nshi\t使\t1024\nshi\t世\t1024\nshi\t十\t1024\nshi\t石\t1024\nshi\t试\t1024\nshi\t市\t1024\nchang\t长\t1024\nchang\t场\t1024\nshi'chang\t是长\t1",
        9,
    )
    .unwrap();
    session.set_ranking_snapshot(Arc::new(data.ranking_snapshot()));
    session.set_host_prefix("城");
    for c in "shichang".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    assert_eq!(session.candidates_on_current_page()[0].text, "市场");
}

#[test]
fn confirmed_prefix_drives_pending_body_and_literal_cancellation_does_not_learn() {
    let mut data = UserDictionary::default();
    for _ in 0..16 {
        data.remember_selection(&[event("市")], "城").unwrap();
        data.remember_selection(&[event("是")], "还").unwrap();
    }
    let mut session =
        PinyinSession::from_lexicon_tsv("cheng\t城\t1024\nshi\t是\t1024\nshi\t市\t1024", 16)
            .unwrap();
    session.set_ranking_snapshot(Arc::new(data.ranking_snapshot()));
    session.set_host_prefix("还");
    for c in "chengshi".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let partial = session
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "城")
        .unwrap();
    session.select_candidate(partial).unwrap();
    assert_eq!(session.candidates_on_current_page()[0].text, "市");
    let ai = session.ai_input();
    if let Some(ai) = ai {
        assert!(
            !format!("{ai:?}").contains('还'),
            "host context must not flow into AI"
        );
    }
    session.commit_raw();
    assert!(session.pending_learning().is_empty());
    session
        .process_key(KeyEvent::plain(Key::Char('s')))
        .unwrap();
    session.process_key(KeyEvent::plain(Key::Escape)).unwrap();
    assert!(session.pending_learning().is_empty());
}

#[test]
fn no_personal_evidence_falls_back_to_frequency_not_saved_absolute_order() {
    let mut session = PinyinSession::from_lexicon_tsv("shi\t是\t8192\nshi\t市\t128", 9).unwrap();
    session.set_ranking_snapshot(Arc::new(Default::default()));
    for c in "shi".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    assert_eq!(session.candidates_on_current_page()[0].text, "是");
}

#[test]
fn finite_word_cost_prevents_unigram_reward_accumulation_without_forbidding_splits() {
    let mut session = PinyinSession::from_lexicon_tsv(
        "shi\t是\t65536\nchang\t长\t65536\nshi'chang\t市场\t16384",
        9,
    )
    .unwrap();
    session.set_ranking_snapshot(Arc::new(Default::default()));
    for c in "shichang".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    assert_eq!(session.candidates_on_current_page()[0].text, "市场");
    session.select_candidate(0).unwrap();
    assert_eq!(
        session.pending_learning(),
        &[LearningEvent {
            pinyin: "shi'chang".into(),
            text: "市场".into()
        }]
    );
}

#[test]
fn fixed_builtin_samples_report_recall_first_and_top_three_separately() {
    // Public fixed lexical probes, not a measured real-user quality claim.
    let probes = [
        ("wenti", "问题"),
        ("shijie", "世界"),
        ("shijian", "时间"),
        ("yingshe", "映射"),
    ];
    for ranked in [false, true] {
        let mut recalled = 0;
        let mut first = 0;
        let mut top_three = 0;
        for (raw, gold) in probes {
            let mut session = PinyinSession::new().unwrap();
            session.set_candidate_width(Some(4096));
            if ranked {
                session.set_ranking_snapshot(Arc::new(Default::default()));
            }
            for c in raw.chars() {
                session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
            }
            let mut ordinary = Vec::new();
            for page in 0..session.page_count() {
                assert_eq!(session.page(), page);
                ordinary.extend(
                    session
                        .candidates_on_current_page()
                        .iter()
                        .filter(|c| !c.is_ai() && !c.is_spelling() && !c.is_completion())
                        .map(|c| c.text.clone()),
                );
                if page + 1 < session.page_count() {
                    session.next_page().unwrap();
                }
            }
            let rank = ordinary.iter().position(|text| text == gold);
            recalled += usize::from(rank.is_some());
            first += usize::from(rank == Some(0));
            top_three += usize::from(rank.is_some_and(|rank| rank < 3));
            eprintln!("ranked={ranked} raw={raw} gold={gold} ordinary_rank={rank:?}");
        }
        eprintln!("fixed lexical probes ranked={ranked}: recall={recalled}/4 first={first}/4 top3={top_three}/4");
        assert_eq!(
            recalled, 4,
            "ranking must retain these fixed lexical choices"
        );
    }
}

#[test]
fn builtin_same_reading_learns_distinct_prefixes_without_changing_neutral_frequency() {
    use flower::engine::Lexicon;
    let lexicon = Lexicon::builtin().unwrap();
    let mut data = UserDictionary::default();
    let compose = |data: &UserDictionary, prefix: &str| {
        let mut session = PinyinSession::from_lexicon(lexicon.clone(), 9).unwrap();
        session.set_ranking_snapshot(Arc::new(data.ranking_snapshot()));
        session.set_host_prefix(prefix);
        for c in "yingshe".chars() {
            session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
        }
        session
    };
    assert_eq!(
        compose(&data, "").candidates_on_current_page()[0].text,
        "影射"
    );
    assert_eq!(
        compose(&data, "函数").candidates_on_current_page()[0].text,
        "影射",
        "before personal context evidence, the builtin frequency winner is unchanged"
    );
    for _ in 0..32 {
        for (prefix, gold) in [("含沙", "影射"), ("函数", "映射")] {
            let mut session = compose(&data, prefix);
            let mut selected = false;
            for _ in 0..session.page_count() {
                if let Some(index) = session
                    .candidates_on_current_page()
                    .iter()
                    .position(|c| c.text == gold && c.comment == "ying'she")
                {
                    assert_eq!(
                        session.select_candidate(index).unwrap().commit.as_deref(),
                        Some(gold)
                    );
                    assert_eq!(
                        session.pending_learning(),
                        &[LearningEvent {
                            pinyin: "ying'she".into(),
                            text: gold.into()
                        }]
                    );
                    // Publish only the successful, canonical ordinary selection.
                    data.remember_selection(session.pending_learning(), prefix)
                        .unwrap();
                    selected = true;
                    break;
                }
                session.next_page().unwrap();
            }
            assert!(
                selected,
                "builtin ordinary candidate {gold} must remain reachable"
            );
        }
    }
    assert_eq!(
        compose(&data, "含沙").candidates_on_current_page()[0].text,
        "影射"
    );
    assert_eq!(
        compose(&data, "函数").candidates_on_current_page()[0].text,
        "映射"
    );
    let mut without_links = data.clone();
    without_links.word_links.clear();
    assert_eq!(
        compose(&without_links, "函数").candidates_on_current_page()[0].text,
        "影射",
        "equal word counts and the public prior alone must not explain the contextual improvement"
    );
    assert_eq!(
        compose(&data, "").candidates_on_current_page()[0].text,
        "影射"
    );
}

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = std::env::var_os("FLOWER_TEST_SCRATCH").expect("isolated test scratch required");
        let path = std::path::PathBuf::from(base).join(format!(
            "local-ranking-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> flower::user_dictionary::DictionaryStore {
        flower::user_dictionary::DictionaryStore::new(self.0.join("dictionary.json"))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn transaction_merges_concurrent_counts_and_disable_revokes_queued_selection() {
    let fixture = Fixture::new();
    let path = fixture.0.join("dictionary.json");
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                let store = flower::user_dictionary::DictionaryStore::new(path);
                for _ in 0..8 {
                    store
                        .commit_selection_if_generation(0, &[], &[], &[event("市")], "城")
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let store = fixture.store();
    let data = store.load().unwrap();
    assert_eq!(data.learned[0].count, 32);
    assert_eq!(data.selection_sequence, 32);
    assert_eq!(data.word_links[0].count, 32);
    let before = std::fs::read(store.path()).unwrap();
    assert!(store
        .commit_selection_if_generation(
            0,
            &[],
            &[],
            &[
                event("市"),
                LearningEvent {
                    pinyin: "bad!".into(),
                    text: "是".into()
                }
            ],
            "城"
        )
        .is_err());
    assert_eq!(std::fs::read(store.path()).unwrap(), before);
    let mut worker = flower::user_dictionary::LearningWorker::start(store.clone(), 0).unwrap();
    store.set_learning_enabled(false).unwrap();
    worker
        .submit_selection_commit(&[], &[], &[event("市")], "城")
        .unwrap();
    worker.shutdown().unwrap();
    assert_eq!(store.load().unwrap().learned[0].count, 32);
    store.clear_learning().unwrap();
    let cleared = store.load().unwrap();
    assert!(
        cleared.learned.is_empty()
            && cleared.recent_words.is_empty()
            && cleared.word_links.is_empty()
    );
    assert_eq!(cleared.selection_sequence, 0);
}

#[test]
fn sequence_overflow_rebases_ages_without_reusing_generation() {
    let mut data = UserDictionary::default();
    data.remember_selection(&[event("市")], "城").unwrap();
    data.selection_sequence = u64::MAX;
    data.recent_words[0].sequence = u64::MAX;
    data.word_links[0].sequence = u64::MAX;
    data.remember_selection(&[event("是")], "").unwrap();
    assert_eq!(data.selection_sequence, 1025);
    assert_eq!(data.recent_words[0].sequence, 1024);
    assert_eq!(data.generation, 0);
    assert!(data.ranking_snapshot().word_bonus("shi", "市") > 0);
}

#[test]
fn recent_and_link_lru_limits_and_saturating_counts_are_enforced() {
    use flower::engine::{RecentWord, WordLink};
    use flower::user_dictionary::{LearnedEntry, MAX_LEARNING_COUNT};
    let mut data = UserDictionary {
        selection_sequence: 8192,
        ..UserDictionary::default()
    };
    for i in 0..8192u32 {
        let text = char::from_u32(0x4e00 + i).unwrap().to_string();
        data.learned.push(LearnedEntry {
            pinyin: "shi".into(),
            text: text.clone(),
            count: MAX_LEARNING_COUNT,
        });
        if i < 4096 {
            data.recent_words.push(RecentWord {
                pinyin: "shi".into(),
                text: text.clone(),
                sequence: u64::from(i) + 1,
            });
        }
        data.word_links.push(WordLink {
            prefix: "城".into(),
            pinyin: "shi".into(),
            text,
            count: MAX_LEARNING_COUNT,
            sequence: u64::from(i) + 1,
        });
    }
    data.remember_selection(&[event("耀")], "城").unwrap();
    assert_eq!(data.recent_words.len(), 4096);
    assert_eq!(data.word_links.len(), 8192);
    assert!(!data.recent_words.iter().any(|word| word.text == "一"));
    assert!(!data.word_links.iter().any(|word| word.text == "一"));
    let last = data.learned[8191].text.clone();
    data.remember_selection(&[event(&last)], "城").unwrap();
    assert_eq!(data.learned[8191].count, MAX_LEARNING_COUNT);
    assert_eq!(data.word_links.last().unwrap().count, MAX_LEARNING_COUNT);
    assert_eq!(data.word_links.last().unwrap().sequence, 8194);
}

#[test]
fn new_records_share_the_existing_global_record_budget() {
    use flower::engine::{RecentWord, WordLink};
    use flower::user_dictionary::{DictionaryError, LearnedEntry};
    let mut data = UserDictionary {
        selection_sequence: 1,
        ..UserDictionary::default()
    };
    data.learned = (0..99_999u32)
        .map(|i| LearnedEntry {
            pinyin: "shi'shi".into(),
            text: format!(
                "{}{}",
                char::from_u32(0x4e00 + i / 20000).unwrap(),
                char::from_u32(0x4e00 + i % 20000).unwrap()
            ),
            count: 1,
        })
        .collect();
    data.recent_words.push(RecentWord {
        pinyin: "shi".into(),
        text: "市".into(),
        sequence: 1,
    });
    data.word_links.push(WordLink {
        prefix: "城".into(),
        pinyin: "shi".into(),
        text: "市".into(),
        count: 1,
        sequence: 1,
    });
    assert!(matches!(
        data.lexicon_tsv(),
        Err(DictionaryError::LimitExceeded)
    ));
}
