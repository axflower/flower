use flower::engine::{
    map_windows_virtual_key, EngineRouter, Key, KeyEvent, Modifiers, PinyinSession,
};

#[test]
fn shifted_letters_commit_uppercase_without_pinyin_or_learning() {
    for vk in 0x41..=0x5a {
        for raw in ["", "nihao"] {
            let mut router = EngineRouter::from_lexicon_tsv("ni'hao\t你好\t100", 7).unwrap();
            for c in raw.chars() {
                router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
            }
            let event = map_windows_virtual_key(vk, Modifiers::SHIFT).unwrap();
            let before = router.preedit().to_string();
            assert!(router.test_key(event).unwrap());
            assert_eq!(router.preedit(), before);
            let update = router.process_key(event).unwrap();
            assert_eq!(
                update.commit,
                Some(format!("{raw}{}", char::from(vk as u8)))
            );
            assert!(update.handled && !router.is_composing());
            assert!(router.pending_learning().is_empty());
            assert!(router.pending_promotions().is_empty());
            assert!(router.pending_phrase_learning().is_empty());
        }
    }
}

#[test]
fn shifted_letter_keeps_confirmed_raw_and_commands_remain_host_owned() {
    let mut session =
        PinyinSession::from_lexicon_tsv("ni\t你\t100\nhao\t好\t100\nni'hao\t你好\t200", 9).unwrap();
    for c in "ni'hao".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let index = session
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "你")
        .unwrap();
    session.select_candidate(index).unwrap();
    for c in ['c', 'v', 'x', 'a', 'z', 'y'] {
        let before = session.preedit().to_string();
        assert!(
            !session
                .process_key(KeyEvent::new(Key::Char(c), Modifiers::CONTROL.with_shift()))
                .unwrap()
                .handled
        );
        assert_eq!(session.preedit(), before);
    }
    assert_eq!(
        session
            .process_key(KeyEvent::new(Key::Char('v'), Modifiers::SHIFT))
            .unwrap()
            .commit
            .as_deref(),
        Some("ni'haoV")
    );
    assert!(!session.is_composing());
    assert!(session.pending_learning().is_empty());
    assert!(session.pending_promotions().is_empty());
}

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
