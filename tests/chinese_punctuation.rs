use flower::engine::{map_windows_virtual_key, EngineRouter, Key, KeyEvent, Modifiers};

const WORDS: &str = "ni\t你\t100\nhao\t好\t100\nni'hao\t你好\t1000";

fn router() -> EngineRouter {
    EngineRouter::from_lexicon_tsv(WORDS, 9).unwrap()
}

fn type_text(r: &mut EngineRouter, text: &str) {
    for c in text.chars() {
        r.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
}

fn press(r: &mut EngineRouter, c: char) -> flower::CompositionUpdate {
    r.process_key(KeyEvent::plain(Key::Char(c))).unwrap()
}

#[test]
fn punctuation_commits_once_without_learning() {
    let mut r = router();
    for (ascii, chinese) in [
        (',', "，"),
        ('.', "。"),
        ('?', "？"),
        ('!', "！"),
        (';', "；"),
        (':', "："),
        ('(', "（"),
        (')', "）"),
        ('[', "【"),
        (']', "】"),
        ('<', "《"),
        ('>', "》"),
        ('/', "、"),
        ('\\', "、"),
        ('~', "～"),
        ('^', "……"),
        ('_', "——"),
    ] {
        let update = press(&mut r, ascii);
        assert!(update.handled);
        assert_eq!(update.commit.as_deref(), Some(chinese));
        assert!(!update.is_composing());
        assert!(r.pending_learning().is_empty());
        assert!(r.pending_promotions().is_empty());
    }
}

#[test]
fn complete_candidate_learns_but_punctuation_does_not() {
    let mut r = router();
    type_text(&mut r, "nihao");
    assert_eq!(press(&mut r, ',').commit.as_deref(), Some("你好，"));
    assert!(!r.pending_learning().is_empty());
    assert!(r.pending_learning().iter().all(|e| !e.text.contains('，')));
    assert_eq!(press(&mut r, '.').commit.as_deref(), Some("。"));
    assert!(r.pending_learning().is_empty());
    assert!(r.pending_promotions().is_empty());
}

#[test]
fn partial_candidate_keeps_all_raw_and_confirmed_prefix() {
    let mut r = router();
    type_text(&mut r, "nihaozzz");
    assert_eq!(press(&mut r, '!').commit.as_deref(), Some("nihaozzz！"));
    type_text(&mut r, "nihaozzz");
    let index = r
        .session()
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "你好")
        .unwrap();
    assert!(r.select_candidate(index).unwrap().commit.is_none());
    assert_eq!(press(&mut r, '?').commit.as_deref(), Some("你好zzz？"));
    assert!(!r.session().is_composing());
}

#[test]
fn quotes_are_independent_survive_clear_and_preview_is_pure() {
    let mut r = router();
    for _ in 0..3 {
        assert!(r.test_key(KeyEvent::plain(Key::Char('"'))).unwrap());
    }
    assert_eq!(press(&mut r, '"').commit.as_deref(), Some("“"));
    assert_eq!(press(&mut r, '\'').commit.as_deref(), Some("‘"));
    type_text(&mut r, "ni");
    assert!(press(&mut r, '\'').commit.is_none());
    assert_eq!(r.preedit(), "ni'");
    r.process_key(KeyEvent::plain(Key::Escape)).unwrap();
    assert_eq!(press(&mut r, '"').commit.as_deref(), Some("”"));
    assert_eq!(press(&mut r, '\'').commit.as_deref(), Some("’"));
}

#[test]
fn shortcuts_stay_host_owned_and_ascii_symbols_commit() {
    let mut r = router();
    type_text(&mut r, "ni");
    for modifiers in [Modifiers::CONTROL, Modifiers::ALT, Modifiers::META] {
        for c in [',', '"', '\'', '^', '_'] {
            assert!(
                !r.process_key(KeyEvent::new(Key::Char(c), modifiers))
                    .unwrap()
                    .handled
            );
            assert_eq!(r.preedit(), "ni");
        }
    }
    for c in ['@', '#', '$', '%', '&', '*', '{', '}', '|', '`'] {
        let mut symbols = router();
        type_text(&mut symbols, "ni");
        let update = press(&mut symbols, c);
        assert!(update.handled);
        assert_eq!(update.commit, Some(format!("你{c}")));
        assert!(!symbols.session().is_composing());
        assert!(symbols
            .pending_learning()
            .iter()
            .all(|event| event.text == "你"));
    }
    assert_eq!(press(&mut r, '"').commit.as_deref(), Some("你“"));
}

#[test]
fn shifted_digits_map_before_candidate_selection() {
    for (digit, symbol) in "0123456789".chars().zip(")!@#$%^&*(".chars()) {
        let event = map_windows_virtual_key(digit as u32, Modifiers::SHIFT).unwrap();
        assert_eq!(event.key, Key::Char(symbol));
        let mut actual = router();
        let mut direct = router();
        type_text(&mut actual, "nihao");
        type_text(&mut direct, "nihao");
        let actual_update = actual.process_key(event).unwrap();
        let direct_update = direct
            .process_key(KeyEvent::new(Key::Char(digit), Modifiers::SHIFT))
            .unwrap();
        assert_eq!(actual_update, direct_update);
        let output = match symbol {
            ')' => "）".to_owned(),
            '!' => "！".to_owned(),
            '^' => "……".to_owned(),
            '(' => "（".to_owned(),
            symbol => symbol.to_string(),
        };
        assert!(!actual.session().is_composing());
        assert_eq!(actual_update.commit, Some(format!("你好{output}")));
        assert_eq!(
            direct
                .pending_learning()
                .iter()
                .map(|event| event.text.as_str())
                .collect::<Vec<_>>(),
            vec!["你好"]
        );
    }
}

#[test]
fn oem_shift_branches_and_existing_paging_keys() {
    for (vk, plain, shifted) in [
        (0xBA, ';', ':'),
        (0xBC, ',', '<'),
        (0xBE, '.', '>'),
        (0xBF, '/', '?'),
        (0xC0, '`', '~'),
        (0xDB, '[', '{'),
        (0xDC, '\\', '|'),
        (0xDD, ']', '}'),
        (0xDE, '\'', '"'),
    ] {
        for (modifiers, expected) in [(Modifiers::NONE, plain), (Modifiers::SHIFT, shifted)] {
            assert_eq!(
                map_windows_virtual_key(vk, modifiers),
                Some(KeyEvent::new(Key::Char(expected), modifiers))
            );
        }
    }
    assert_eq!(
        map_windows_virtual_key(0xBD, Modifiers::SHIFT).unwrap().key,
        Key::Char('_')
    );
    for (vk, key) in [
        (0xBD, Key::PageUp),
        (0xBB, Key::PageDown),
        (0x6B, Key::PageDown),
        (0x6D, Key::PageUp),
    ] {
        assert_eq!(
            map_windows_virtual_key(vk, Modifiers::NONE).unwrap().key,
            key
        );
    }
    assert_eq!(
        map_windows_virtual_key(0xBB, Modifiers::SHIFT).unwrap().key,
        Key::Char('+')
    );
}

#[test]
fn punctuation_uses_ai_only_after_explicit_highlight_and_never_learns_ai() {
    for select_ai in [false, true] {
        let mut r =
            EngineRouter::from_lexicon_tsv("jin'tian'xin'qing'hen'hao\t今天新情很好\t100", 9)
                .unwrap();
        type_text(&mut r, "jintianxinqinghenhao");
        let input = r.ai_input().unwrap();
        r.apply_ai_candidate(&input, "今天心情很好").unwrap();
        if select_ai {
            r.process_key(KeyEvent::plain(Key::Right)).unwrap();
        }
        assert_eq!(
            press(&mut r, '.').commit.as_deref(),
            Some(if select_ai {
                "今天心情很好。"
            } else {
                "今天新情很好。"
            })
        );
        if select_ai {
            assert!(r.pending_learning().is_empty());
            assert!(r.pending_promotions().is_empty());
        } else {
            assert!(!r.pending_learning().is_empty());
        }
    }
}
