use flower::engine::{Key, KeyEvent, Lexicon, PinyinSession};

fn type_text(session: &mut PinyinSession, raw: &str) {
    for c in raw.chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
}

fn select(session: &mut PinyinSession, text: &str) -> flower::CompositionUpdate {
    for _ in 0..session.page_count() {
        if let Some(index) = session
            .candidates_on_current_page()
            .iter()
            .position(|candidate| candidate.text == text)
        {
            return session.select_candidate(index).unwrap();
        }
        session.next_page().unwrap();
    }
    panic!("missing {text} for {}", session.pending_input());
}

#[test]
fn standalone_initials_commit_one_raw_letter_with_canonical_learning() {
    let lexicon = Lexicon::from_tsv(
        "le\t了\t1000\nlai\t来\t900\nni\t你\t800\nhao\t好\t700\n\
         zhong\t中\t600\nlu\t路\t500\nlv\t绿\t400",
    )
    .unwrap();
    for (raw, text, reading) in [
        ("l", "了", "le"),
        ("l", "来", "lai"),
        ("n", "你", "ni"),
        ("h", "好", "hao"),
        ("z", "中", "zhong"),
        ("l", "绿", "lv"),
    ] {
        let mut session = PinyinSession::from_lexicon(lexicon.clone(), 9).unwrap();
        type_text(&mut session, raw);
        assert_eq!(session.pending_input(), raw);
        assert!(session.ai_input().is_none());
        let update = select(&mut session, text);
        assert_eq!(update.commit.as_deref(), Some(text));
        assert!(session.pending_input().is_empty());
        assert_eq!(session.pending_learning().len(), 1);
        assert_eq!(session.pending_learning()[0].pinyin, reading);
        assert_eq!(session.pending_learning()[0].text, text);
    }
}

#[test]
fn consonants_rank_common_characters_by_frequency_without_losing_exact_readings() {
    let mut builtin = PinyinSession::new().unwrap();
    type_text(&mut builtin, "n");
    assert!(builtin
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "你"));
    assert_ne!(builtin.candidates_on_current_page()[0].text, "嗯");
    assert_eq!(select(&mut builtin, "嗯").commit.as_deref(), Some("嗯"));
    let mut session = PinyinSession::from_lexicon_tsv(
        "n\t嗯\t10\nni\t你\t100\nna\t那\t80\nm\t呣\t1\nma\t吗\t200",
        9,
    )
    .unwrap();
    type_text(&mut session, "n");
    let candidates = session.candidates_on_current_page();
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["你", "那", "嗯"]
    );
    assert_eq!(candidates[0].comment, "ni");
    assert_eq!(select(&mut session, "你").commit.as_deref(), Some("你"));
    assert_eq!(session.pending_learning()[0].pinyin, "ni");
    type_text(&mut session, "m");
    assert_eq!(session.candidates_on_current_page()[0].text, "吗");
    assert!(session
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "呣" && c.comment == "m"));
}

#[test]
fn exact_syllables_stay_first_and_polyphonic_characters_are_not_duplicated() {
    let mut session = PinyinSession::from_lexicon_tsv(
        "a\t啊\t1\nan\t安\t500\na\t阿\t2\nan\t阿\t700\nai\t爱\t900",
        9,
    )
    .unwrap();
    type_text(&mut session, "a");
    let candidates = session.candidates_on_current_page();
    let text: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.text.as_str())
        .collect();
    assert_eq!(text, ["阿", "啊", "爱", "安"]);
    assert_eq!(candidates[0].comment, "a");
    assert_eq!(select(&mut session, "安").commit.as_deref(), Some("安"));
    assert_eq!(session.pending_learning()[0].pinyin, "an");
}

#[test]
fn continuation_backspace_cancel_and_long_input_keep_the_original_boundaries() {
    let mut session =
        PinyinSession::from_lexicon_tsv("le\t了\t1000\nlai\t来\t900\nni\t你\t800", 9).unwrap();
    type_text(&mut session, "l");
    type_text(&mut session, "ai");
    assert_eq!(session.candidates_on_current_page()[0].text, "来");
    session
        .process_key(KeyEvent::plain(Key::Backspace))
        .unwrap();
    session
        .process_key(KeyEvent::plain(Key::Backspace))
        .unwrap();
    assert_eq!(session.pending_input(), "l");
    assert_eq!(session.candidates_on_current_page()[0].text, "了");
    session.process_key(KeyEvent::plain(Key::Escape)).unwrap();
    assert!(session.pending_input().is_empty());
    assert!(session.pending_learning().is_empty());
    type_text(&mut session, "ln");
    assert!(
        session.candidates_on_current_page().is_empty(),
        "single initials must not become multi-letter sentence graph edges"
    );
}

#[test]
fn builtin_l_contains_common_characters_on_the_first_page() {
    let mut session = PinyinSession::new().unwrap();
    type_text(&mut session, "l");
    let candidates = session.candidates_on_current_page();
    eprintln!("builtin_l={candidates:?}");
    assert!(candidates.iter().any(|candidate| candidate.text == "了"));
    assert!(candidates.iter().any(|candidate| candidate.text == "来"));
    assert_eq!(select(&mut session, "来").commit.as_deref(), Some("来"));
    assert_eq!(session.pending_learning()[0].pinyin, "lai");
}

#[test]
fn standalone_index_tracks_overlay_and_learning_without_mutating_shared_snapshots() {
    let base = Lexicon::from_tsv("le\t了\t1000\nlai\t来\t900").unwrap();
    let overlay = base.with_overlay("lin\t林\t2000").unwrap();
    let learned = base.with_learning(&[(
        flower::engine::LearningEvent {
            pinyin: "lai".into(),
            text: "来".into(),
        },
        1,
    )]);
    for (lexicon, first) in [
        (overlay, "林"),
        (learned, "来"),
        (base.clone(), "了"),
        (base, "了"),
    ] {
        let mut session = PinyinSession::from_lexicon(lexicon, 9).unwrap();
        type_text(&mut session, "l");
        assert_eq!(session.candidates_on_current_page()[0].text, first);
    }
}
