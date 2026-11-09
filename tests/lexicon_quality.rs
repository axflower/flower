use flower::engine::{Key, KeyEvent, LearningEvent, Lexicon, PinyinSession};

#[test]
fn actual_embedded_lexicon_keeps_all_fixed_phrase_cases_at_top_one() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../data/pinyin/phrases/quality-cases.json")).unwrap();
    let lexicon = Lexicon::builtin().unwrap();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let reading = case["pinyin"].as_str().unwrap();
        let word = case["text"].as_str().unwrap();
        let candidates = lexicon.lookup(reading);
        assert_eq!(
            candidates.first().map(|c| c.text.as_str()),
            Some(word),
            "{reading}"
        );
    }
}

#[test]
fn local_overlay_and_bounded_learning_do_not_mutate_the_shared_base() {
    let base = Lexicon::from_tsv("shi\t是\t1000\nshi\t事\t900").unwrap();
    let overlay = base.with_overlay("shi\t式\t2000").unwrap();
    assert_eq!(overlay.lookup("shi")[0].text, "式");
    let learned = base.with_learning(&[(
        LearningEvent {
            pinyin: "shi".into(),
            text: "事".into(),
        },
        2,
    )]);
    assert_eq!(learned.lookup("shi")[0].text, "事");
    assert_eq!(base.lookup("shi")[0].text, "是");
    let not_invented = base.with_learning(&[(
        LearningEvent {
            pinyin: "shi".into(),
            text: "虚构".into(),
        },
        u32::MAX,
    )]);
    assert_eq!(not_invented.lookup("shi"), base.lookup("shi"));
}

fn initial_candidates(lexicon: Lexicon, raw: &str) -> Vec<flower::Candidate> {
    let mut session = PinyinSession::from_lexicon(lexicon, 9).unwrap();
    for c in raw.chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let mut candidates = Vec::new();
    for _ in 0..session.page_count() {
        candidates.extend_from_slice(session.candidates_on_current_page());
        session.next_page().unwrap();
    }
    candidates
}

#[test]
fn builtin_daily_prior_remains_bounded_without_dropping_initial_collisions() {
    let builtin = Lexicon::builtin().unwrap();
    let before = initial_candidates(builtin.clone(), "nh");
    let overlay = builtin
        .with_overlay("ni'hao\t私词\t4294967295\nni'hao\t你好\t1")
        .unwrap();
    let candidates = initial_candidates(overlay, "nh");
    let hello = candidates.iter().position(|c| c.text == "你好").unwrap();
    let private = candidates.iter().position(|c| c.text == "私词").unwrap();
    assert!(hello < 9);
    // The documented bounded prior cannot override an explicit maximum weight.
    assert!(private < hello);
    assert_eq!(candidates[hello].comment, "ni'hao");
    assert_eq!(candidates[private].comment, "ni'hao");
    for original in &before {
        assert!(candidates.contains(original), "lost {original:?}");
    }
    assert_eq!(
        initial_candidates(Lexicon::builtin().unwrap(), "nh"),
        before
    );
}

#[test]
fn initials_rank_same_reading_words_and_refresh_each_immutable_snapshot() {
    let base = Lexicon::from_tsv("ni'hao\t你好\t1000\nni'hao\t拟好\t900").unwrap();
    let base_candidates = initial_candidates(base.clone(), "nh");
    assert_eq!(base_candidates[0].text, "你好");
    assert_eq!(base_candidates[1].text, "拟好");
    let overlay = base
        .with_overlay("ni'hao\t拟好\t2000\nnan'hai\t南海\t1500")
        .unwrap();
    let candidates = initial_candidates(overlay, "nh");
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["拟好", "南海", "你好"]
    );
    let learned = base.with_learning(&[(
        LearningEvent {
            pinyin: "ni'hao".into(),
            text: "拟好".into(),
        },
        2,
    )]);
    let candidates = initial_candidates(learned, "nh");
    assert_eq!(candidates[0].text, "拟好");
    assert_eq!(candidates[0].comment, "ni'hao");
    assert_eq!(initial_candidates(base, "nh"), base_candidates);
}

#[test]
fn embedded_initials_include_hello_and_overlay_does_not_pollute_builtin() {
    let builtin = Lexicon::builtin().unwrap();
    let before = initial_candidates(builtin.clone(), "nh");
    assert!(before
        .iter()
        .any(|c| c.text == "你好" && c.comment == "ni'hao"));
    let overlay = builtin.with_overlay("ni'hao\t私词\t4294967295").unwrap();
    let candidates = initial_candidates(overlay, "nh");
    let hello = candidates.iter().position(|c| c.text == "你好").unwrap();
    let private = candidates.iter().position(|c| c.text == "私词").unwrap();
    assert!(private < hello);
    assert_eq!(candidates[private].comment, "ni'hao");
    assert!(!before.iter().any(|c| c.text == "私词"));
    assert_eq!(initial_candidates(builtin, "nh"), before);
    assert_eq!(
        initial_candidates(Lexicon::builtin().unwrap(), "nh"),
        before
    );
}
