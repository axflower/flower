use flower::engine::{Candidate, Key, KeyEvent, PinyinSession};

fn type_text(session: &mut PinyinSession, raw: &str) {
    for c in raw.chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
}

fn find(session: &mut PinyinSession, text: &str) -> (usize, Candidate) {
    for page in 0..session.page_count() {
        if let Some((i, c)) = session
            .candidates_on_current_page()
            .iter()
            .enumerate()
            .find(|(_, c)| c.text == text)
        {
            return (i, c.clone());
        }
        if page + 1 < session.page_count() {
            session.next_page().unwrap();
        }
    }
    panic!("missing {text}: {}", session.preedit());
}

#[test]
fn short_word_edit_types_are_visible_and_commit_without_learning() {
    for (raw, gold) in [
        ("xuexo", "学习"),
        ("xueexi", "学习"),
        ("uxexi", "学习"),
        ("nihoa", "你好"),
        ("zaijain", "再见"),
        ("gongzuoo", "工作"),
        ("xueerunishuixingzhou", "学如逆水行舟"),
        ("dianyiqngyuan", "电影院"),
        ("dianyinngyuan", "电影院"),
        ("jintainzaoshang", "今天早上"),
        ("jintianzaosahng", "今天早上"),
    ] {
        let mut s = PinyinSession::new().unwrap();
        type_text(&mut s, raw);
        let (i, c) = find(&mut s, gold);
        assert!(c.is_spelling(), "{raw}: {c:?}");
        assert!(!c.is_ai());
        assert_eq!(c.display_text(), format!("{gold} ✓ 拼写"));
        let update = s.select_candidate(i).unwrap();
        assert_eq!(update.commit.as_deref(), Some(gold));
        assert!(s.pending_input().is_empty());
        assert!(s.pending_learning().is_empty());
        assert!(s.pending_promotions().is_empty());
        assert!(!s.is_composing());
    }
    // A dictionary without a competing exact "xu'xi" word exercises deletion.
    let mut s = PinyinSession::from_lexicon_tsv("xue'xi\t学习\t100", 9).unwrap();
    type_text(&mut s, "xuxi");
    let (i, c) = find(&mut s, "学习");
    assert!(c.is_spelling());
    assert_eq!(
        s.select_candidate(i).unwrap().commit.as_deref(),
        Some("学习")
    );
    assert!(s.pending_learning().is_empty());
    assert!(s.pending_promotions().is_empty());
}

#[test]
fn exact_words_keep_normal_provenance_and_no_spelling_proposals() {
    for raw in ["xuexi", "nihao", "gongzuo", "xuanze", "l"] {
        let mut s = PinyinSession::new().unwrap();
        type_text(&mut s, raw);
        for page in 0..s.page_count() {
            assert!(s
                .candidates_on_current_page()
                .iter()
                .all(|c| !c.is_spelling()));
            if page + 1 < s.page_count() {
                s.next_page().unwrap();
            }
        }
    }
}

#[test]
fn spelling_is_not_used_to_invent_an_ai_source() {
    let mut s = PinyinSession::from_lexicon_tsv("xue'xi\t学习\t100", 9).unwrap();
    type_text(&mut s, "xuexo");
    assert!(s
        .candidates_on_current_page()
        .iter()
        .any(|c| c.is_spelling()));
    assert_eq!(s.ai_input(), None);

    let mut s = PinyinSession::new().unwrap();
    type_text(&mut s, "xuexo");
    let input = s.ai_input().unwrap();
    assert_ne!(input.source, "学习");
    assert!(!input.reading.contains("[Flower"));
}

#[test]
fn confirmed_prefix_commits_once_and_cancel_remains_reversible() {
    let fixture = "wo\t我\t100\nxue'xi\t学习\t100\nxue\t学\t1";
    let mut s = PinyinSession::from_lexicon_tsv(fixture, 9).unwrap();
    type_text(&mut s, "woxuexo");
    let (i, _) = find(&mut s, "我");
    assert!(s.select_candidate(i).unwrap().commit.is_none());
    assert_eq!(s.pending_input(), "xuexo");
    for _ in 0..5 {
        s.process_key(KeyEvent::plain(Key::Left)).unwrap();
    }
    assert!(s
        .process_key(KeyEvent::plain(Key::Backspace))
        .unwrap()
        .commit
        .is_none());
    assert_eq!(s.pending_input(), "woxuexo");
    assert_eq!(s.preedit(), "woxuexo");
    let (i, _) = find(&mut s, "我");
    assert!(s.select_candidate(i).unwrap().commit.is_none());
    let (i, c) = find(&mut s, "学习");
    assert!(c.is_spelling());
    assert_eq!(
        s.select_candidate(i).unwrap().commit.as_deref(),
        Some("我学习")
    );
    assert!(s.pending_learning().is_empty());
    assert!(s.pending_promotions().is_empty());

    type_text(&mut s, "xuexo");
    assert!(s.cancel().unwrap().cancelled);
    assert!(!s.is_composing());
    assert!(s.pending_learning().is_empty());
}
