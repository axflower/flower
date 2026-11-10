use flower::engine::{map_windows_virtual_key, EngineRouter, Key, KeyEvent, Modifiers};
use flower::ChineseScript;

const WORDS: &str = "zhong'guo\t中国\t100\nruan'jian\t软件\t100\nni\t你\t100";
fn router(raw: &str) -> EngineRouter {
    let mut router = EngineRouter::from_lexicon_tsv(WORDS, 9).unwrap();
    for c in raw.chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    router
}
fn toggle() -> KeyEvent {
    KeyEvent::new(Key::Char('f'), Modifiers::CONTROL.with_shift())
}

#[test]
fn symbols_consume_once_and_only_word_identity_learns() {
    for c in "@#$%&*+{}|`".chars() {
        let mut empty = router("");
        let update = empty.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
        assert_eq!(update.commit, Some(c.to_string()));
        assert!(empty.pending_learning().is_empty());
        let mut composed = router("ni");
        let update = composed.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
        assert_eq!(update.commit, Some(format!("你{c}")));
        assert!(!composed.is_composing());
        assert!(!composed.pending_learning().is_empty());
        assert!(composed.pending_learning().iter().all(|e| e.text == "你"));
    }
}

#[test]
fn shifted_digits_and_plus_are_symbols_not_candidate_or_page_keys() {
    for (vk, output) in (0x30..=0x39)
        .zip(["）", "！", "@", "#", "$", "%", "……", "&", "*", "（"])
        .chain(std::iter::once((0xBB, "+")))
    {
        let event = map_windows_virtual_key(vk, Modifiers::SHIFT).unwrap();
        let mut composed = router("ni");
        assert_eq!(
            composed.process_key(event).unwrap().commit,
            Some(format!("你{output}"))
        );
        assert_eq!(
            router("").process_key(event).unwrap().commit.as_deref(),
            Some(output)
        );
    }
    for vk in [0xBB, 0x6B] {
        assert_eq!(
            map_windows_virtual_key(vk, Modifiers::NONE).unwrap().key,
            Key::PageDown
        );
    }
    for vk in [0xBD, 0x6D] {
        assert_eq!(
            map_windows_virtual_key(vk, Modifiers::NONE).unwrap().key,
            Key::PageUp
        );
    }
}

#[test]
fn toggle_preview_is_pure_and_preserves_caret_and_raw() {
    let mut r = router("zhongguo");
    let before = r.process_key(KeyEvent::plain(Key::Left)).unwrap();
    for _ in 0..3 {
        assert!(r.test_key(toggle()).unwrap());
    }
    assert_eq!(r.session().candidates_on_current_page()[0].text, "中国");
    let update = r.process_key(toggle()).unwrap();
    assert_eq!(update.preedit, before.preedit);
    assert_eq!(update.preedit_caret_utf16, before.preedit_caret_utf16);
    assert!(update.commit.is_none());
    assert_eq!(r.session().candidates_on_current_page()[0].text, "中國");
    assert!(r.pending_learning().is_empty());
    assert!(r.pending_promotions().is_empty());
    assert_eq!(r.commit_raw().commit.as_deref(), Some("zhongguo"));
}

#[test]
fn toggle_keeps_confirmed_identity_and_legal_learning() {
    let mut r = router("zhongguoruanjian");
    let index = r
        .session()
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "中国")
        .unwrap();
    assert!(r.select_candidate(index).unwrap().commit.is_none());
    let raw = r.pending_input().to_string();
    let update = r.process_key(toggle()).unwrap();
    assert_eq!(r.pending_input(), raw);
    assert_eq!(update.preedit, "中國ruanjian");
    assert!(update.commit.is_none());
    assert!(r.pending_learning().is_empty());
    assert!(r.pending_promotions().is_empty());
    assert_eq!(
        r.select_candidate(0).unwrap().commit.as_deref(),
        Some("中國軟件")
    );
    assert!(r.pending_learning().iter().any(|e| e.text == "中国"));
    assert!(r.pending_learning().iter().all(|e| !e.text.contains('國')));
    assert!(r.set_chinese_script(ChineseScript::Simplified));
}

#[test]
fn only_exact_toggle_is_engine_owned() {
    for modifiers in [
        Modifiers::CONTROL,
        Modifiers::ALT.with_shift(),
        Modifiers::CONTROL.with_shift().with_alt(),
        Modifiers::CONTROL.with_shift().with_meta(),
    ] {
        let mut r = router("ni");
        assert!(
            !r.process_key(KeyEvent::new(Key::Char('f'), modifiers))
                .unwrap()
                .handled
        );
        assert_eq!(r.preedit(), "ni");
    }
}
