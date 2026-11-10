use flower::engine::{CompositionUpdate, Key, KeyEvent, PinyinSession};

const WORDS: &str = "ni\t你\t1000\nni\t尼\t900\nni\t泥\t800\nhao\t好\t1000\nwo\t我\t1000\nni'hao\t你好\t2000\nxi\t西\t100\nan\t安\t100\nxi'an\t西安\t2000";

fn session() -> PinyinSession {
    PinyinSession::from_lexicon_tsv(WORDS, 2).unwrap()
}

fn press(s: &mut PinyinSession, key: Key) -> CompositionUpdate {
    s.process_key(KeyEvent::plain(key)).unwrap()
}

fn type_text(s: &mut PinyinSession, text: &str) -> CompositionUpdate {
    let mut update = CompositionUpdate::default();
    for c in text.chars() {
        update = press(s, Key::Char(c));
    }
    update
}

fn choose(s: &mut PinyinSession, text: &str) -> CompositionUpdate {
    for _ in 0..s.page_count() {
        if let Some(index) = s
            .candidates_on_current_page()
            .iter()
            .position(|c| c.text == text)
        {
            return s.select_candidate(index).unwrap();
        }
        s.next_page().unwrap();
    }
    panic!("missing {text}");
}

#[test]
fn caret_roundtrip_precedes_candidate_navigation_and_preserves_choices() {
    let mut s = session();
    let original = type_text(&mut s, "ni");
    let left = press(&mut s, Key::Left);
    assert_eq!(left.preedit_caret_utf16, Some(1));
    assert_eq!(left.candidates, original.candidates);
    assert_eq!(press(&mut s, Key::Right).highlighted, Some(0));
    assert_eq!(press(&mut s, Key::Right).highlighted, Some(1));
    let previous = press(&mut s, Key::Left);
    assert_eq!(previous.highlighted, Some(0));
    assert_eq!(previous.preedit_caret_utf16, Some(2));
    press(&mut s, Key::Left);
    press(&mut s, Key::Left);
    assert_eq!(press(&mut s, Key::Left).preedit_caret_utf16, Some(0));
    assert!(s.pending_learning().is_empty());
    assert!(s.pending_promotions().is_empty());
}

#[test]
fn internal_insert_backspace_delete_and_saturated_boundaries() {
    let mut s = session();
    type_text(&mut s, "zz");
    press(&mut s, Key::Left); // No candidates still permits editing.
    assert_eq!(press(&mut s, Key::Char('A')).preedit, "zaz");
    assert_eq!(press(&mut s, Key::Backspace).preedit, "zz");
    assert_eq!(press(&mut s, Key::Delete).preedit, "z");
    assert_eq!(press(&mut s, Key::Delete).preedit, "z");
    press(&mut s, Key::Left);
    assert_eq!(press(&mut s, Key::Backspace).preedit, "z");
    let empty = press(&mut s, Key::Delete);
    assert!(empty.handled);
    assert_eq!(empty.preedit_caret_utf16, Some(0));
    assert!(!press(&mut s, Key::Delete).handled);
}

#[test]
fn separators_reject_both_neighbors_and_deletions_do_not_normalize() {
    let mut s = session();
    type_text(&mut s, "x'a'n");
    for _ in 0..3 {
        press(&mut s, Key::Left);
    }
    assert_eq!(press(&mut s, Key::Char('\'')).preedit, "x'a'n");
    assert_eq!(press(&mut s, Key::Delete).preedit, "x''n");
    assert_eq!(press(&mut s, Key::Char('\'')).preedit, "x''n");
    press(&mut s, Key::Left);
    assert_eq!(press(&mut s, Key::Char('\'')).preedit, "x''n");
    press(&mut s, Key::Left);
    assert_eq!(press(&mut s, Key::Char('\'')).preedit, "x''n");
}

#[test]
fn candidate_navigation_and_aliases_restore_end_even_when_saturated() {
    for key in [
        Key::Up,
        Key::Down,
        Key::PageUp,
        Key::PageDown,
        Key::Char('-'),
        Key::Char('='),
    ] {
        let mut s = session();
        type_text(&mut s, "ni");
        press(&mut s, Key::Left);
        assert_eq!(press(&mut s, key).preedit_caret_utf16, Some(2));
    }
    // '+' is a literal symbol now, not a paging alias. It commits once even
    // when the raw caret is internal, and leaves no active composition.
    let mut symbol = session();
    type_text(&mut symbol, "ni");
    press(&mut symbol, Key::Left);
    let update = press(&mut symbol, Key::Char('+'));
    assert_eq!(update.commit.as_deref(), Some("你+"));
    assert!(!update.is_composing());
    assert_eq!(update.preedit_caret_utf16, Some(0));
    let mut s = session();
    type_text(&mut s, "ni");
    press(&mut s, Key::Left);
    assert_eq!(s.next_page().unwrap().preedit_caret_utf16, Some(2));
    press(&mut s, Key::Left);
    assert_eq!(s.previous_page().unwrap().preedit_caret_utf16, Some(2));
}

#[test]
fn confirmed_restore_keeps_suffix_and_only_unaffected_learning() {
    let mut s = session();
    type_text(&mut s, "wonihao");
    choose(&mut s, "我");
    let partial = choose(&mut s, "你");
    assert_eq!(partial.preedit, "我你hao");
    assert_eq!(partial.preedit_caret_utf16, Some(5));
    for _ in 0..3 {
        press(&mut s, Key::Left);
    }
    let restored = press(&mut s, Key::Backspace);
    assert_eq!(restored.preedit, "我nihao");
    assert_eq!(restored.preedit_caret_utf16, Some(3));
    assert_eq!(s.pending_input(), "nihao");
    assert_eq!(s.commit().unwrap().commit.as_deref(), Some("我你好"));
    assert_eq!(s.pending_learning().len(), 2);
    assert_eq!(s.pending_learning()[0].pinyin, "wo");
    assert_eq!(s.pending_learning()[1].pinyin, "ni'hao");
}

#[test]
fn utf16_prefix_counts_surrogate_pairs_and_clear_resets_caret() {
    let mut s = PinyinSession::from_lexicon_tsv("ni\t𠀀\t1000\nhao\t好\t1000", 9).unwrap();
    type_text(&mut s, "nihao");
    let partial = choose(&mut s, "𠀀");
    assert_eq!(partial.preedit_caret_utf16, Some(5));
    assert_eq!(press(&mut s, Key::Left).preedit_caret_utf16, Some(4));
    assert_eq!(s.cancel().unwrap().preedit_caret_utf16, Some(0));
    assert_eq!(type_text(&mut s, "ni").preedit_caret_utf16, Some(2));
    assert_eq!(s.commit().unwrap().preedit_caret_utf16, Some(0));
}

#[test]
fn ai_pauses_inside_raw_and_width_and_clone_preserve_caret() {
    let mut s = PinyinSession::from_lexicon_tsv("ni'hao'shi'jie\t你好世界\t2000", 9).unwrap();
    let end = type_text(&mut s, "nihaoshijie");
    assert_eq!(end.preedit_caret_utf16, Some(11));
    assert!(s.ai_input().is_some());
    assert_eq!(press(&mut s, Key::Left).preedit_caret_utf16, Some(10));
    assert!(s.ai_input().is_none());
    assert_eq!(
        s.set_candidate_width(Some(300)).preedit_caret_utf16,
        Some(10)
    );
    let mut prediction = s.clone();
    assert_eq!(
        press(&mut prediction, Key::Right).preedit_caret_utf16,
        Some(11)
    );
    assert!(prediction.ai_input().is_some());
    assert_eq!(press(&mut s, Key::Other).preedit_caret_utf16, Some(10));
    assert!(s.ai_input().is_none());
    press(&mut s, Key::Right);
    assert!(s.ai_input().is_some());
}

#[test]
fn on_test_predicts_internal_edits_without_mutating_live_caret_or_text() {
    use flower::engine::EngineRouter;
    let mut router = EngineRouter::from_lexicon_tsv(WORDS, 9).unwrap();
    for c in "nihao".chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    router.process_key(KeyEvent::plain(Key::Left)).unwrap();
    let before = router.process_key(KeyEvent::plain(Key::Other)).unwrap();
    for key in [
        Key::Left,
        Key::Right,
        Key::Backspace,
        Key::Delete,
        Key::Char('a'),
    ] {
        assert_eq!(router.test_key(KeyEvent::plain(key)), Ok(true));
        assert_eq!(
            router.process_key(KeyEvent::plain(Key::Other)).unwrap(),
            before
        );
    }
    assert!(router.pending_learning().is_empty());
    assert!(router.pending_promotions().is_empty());
}
