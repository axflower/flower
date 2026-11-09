use flower::engine::{EngineRouter, Key, KeyEvent, PinyinSession};

#[test]
fn raw_conversion_restores_selected_prefix_and_never_learns() {
    let mut session =
        PinyinSession::from_lexicon_tsv("ni\t你\t100\nhao\t好\t100\nni'hao\t你好\t200", 9).unwrap();
    for c in "ni'hao".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    // Select a partial prefix, not the complete candidate.
    let index = session
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "你")
        .unwrap();
    assert!(session.select_candidate(index).unwrap().commit.is_none());
    assert_eq!(session.preedit(), "你hao");
    let update = session.commit_raw();
    assert_eq!(update.commit.as_deref(), Some("ni'hao"));
    assert!(update.handled);
    assert!(!session.is_composing());
    assert!(session.pending_learning().is_empty());
    assert!(session.pending_promotions().is_empty());
    assert!(session.pending_phrase_learning().is_empty());
    assert!(!session.commit_raw().handled);
}

#[test]
fn raw_conversion_clears_stale_promotions_and_does_not_choose_chinese() {
    let mut router = EngineRouter::from_lexicon_tsv("ni'hao\t你好\t100", 9).unwrap();
    for c in "nihao".chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    assert_eq!(router.preedit(), "nihao");
    assert!(router.test_key(KeyEvent::plain(Key::Space)).unwrap());
    assert!(router.pending_promotions().is_empty());
    assert_eq!(router.commit_raw().commit.as_deref(), Some("nihao"));
    assert!(router.pending_promotions().is_empty());
    assert!(router.pending_learning().is_empty());
}
