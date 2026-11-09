use flower::engine::keymap::{map_windows_virtual_key, VK_BACK, VK_NEXT, VK_PRIOR, VK_RIGHT};
use flower::engine::{
    CompositionUpdate, EngineError, EngineSession, Key, KeyEvent, Lexicon, Modifiers,
    PinyinSession, DEFAULT_PAGE_SIZE,
};

fn session() -> PinyinSession {
    PinyinSession::new().expect("embedded MVP lexicon must load")
}

const ADAPTIVE_TSV: &str = "shi\t是\t900\nshi\t时\t800\nshi\t十\t700\nshi\t事\t600\nshi\t市\t500\nshi\t世界很长的候选文本\t400\nshi\t诗\t300";

#[test]
fn adaptive_promotion_is_one_adjacent_step_and_can_be_restored() {
    use flower::engine::promote_order;
    use std::sync::Arc;
    for selected in [1, 4] {
        let mut session = PinyinSession::from_lexicon_tsv(ADAPTIVE_TSV, 9).unwrap();
        let baseline = type_text(&mut session, "shi").candidates;
        assert!(session.pending_promotions().is_empty());
        session.select_candidate(selected).unwrap();
        let event = session.pending_promotions()[0].clone();
        assert_eq!(event.order.input, "shi");
        assert_eq!(event.order.candidates.len(), baseline.len());
        assert_eq!(event.selected.text, baseline[selected].text);
        let mut orders = Vec::new();
        promote_order(&mut orders, &event);
        let mut expected = baseline.clone();
        expected.swap(selected - 1, selected);
        session.set_ranking_orders(Arc::new(orders.clone()));
        assert_eq!(type_text(&mut session, "shi").candidates, expected);
        assert!(session.pending_promotions().is_empty());
        let mut restored = PinyinSession::from_lexicon_tsv(ADAPTIVE_TSV, 9).unwrap();
        restored.set_ranking_orders(Arc::new(orders));
        assert_eq!(type_text(&mut restored, "shi").candidates, expected);
        restored.cancel().unwrap();
        assert!(restored.pending_promotions().is_empty());
    }
}

#[test]
fn adaptive_pages_cover_all_choices_and_preserve_absolute_highlight() {
    let mut session = PinyinSession::from_lexicon_tsv(ADAPTIVE_TSV, 9).unwrap();
    let baseline = type_text(&mut session, "shi").candidates;
    session.process_key(KeyEvent::plain(Key::Right)).unwrap();
    session.process_key(KeyEvent::plain(Key::Right)).unwrap();
    let update = session.set_candidate_width(Some(130));
    assert!(update.handled);
    assert_eq!(update.candidates[update.highlighted.unwrap()], baseline[2]);
    while session.page() > 0 {
        session.previous_page().unwrap();
    }
    let mut actual = Vec::new();
    loop {
        let page = session.candidates_on_current_page();
        let width: u32 = page
            .iter()
            .enumerate()
            .map(|(i, c)| flower::engine::paging::candidate_cell_width(&c.text, i))
            .sum();
        assert!(page.len() == 1 || width <= 130);
        assert!(page.len() <= 9);
        actual.extend_from_slice(page);
        if session.page() + 1 == session.page_count() {
            break;
        }
        session.next_page().unwrap();
    }
    assert_eq!(actual, baseline);
    let expected = session.candidates_on_current_page()[0].text.clone();
    assert_eq!(
        session
            .process_key(KeyEvent::plain(Key::Enter))
            .unwrap()
            .commit,
        Some(expected)
    );
}

#[test]
fn adaptive_local_selection_and_preview_do_not_apply_promotions() {
    use flower::engine::EngineRouter;
    for key in [Key::Char('2'), Key::Enter] {
        let mut router = EngineRouter::from_lexicon_tsv(ADAPTIVE_TSV, 9).unwrap();
        router.set_candidate_width(Some(130));
        for c in "shi".chars() {
            router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
        }
        router.next_page().unwrap();
        let before = router.session().candidates_on_current_page().to_vec();
        assert!(router.test_key(KeyEvent::plain(key)).unwrap());
        assert_eq!(router.session().candidates_on_current_page(), before);
        assert!(router.pending_promotions().is_empty());
        let index = if key == Key::Enter { 0 } else { 1 };
        let expected = before[index].text.clone();
        assert_eq!(
            router.process_key(KeyEvent::plain(key)).unwrap().commit,
            Some(expected)
        );
        assert_eq!(
            router.pending_promotions()[0].selected.text,
            before[index].text
        );
    }
    let mut session = PinyinSession::from_lexicon_tsv(ADAPTIVE_TSV, 9).unwrap();
    session.set_candidate_width(Some(130));
    type_text(&mut session, "shi");
    session.next_page().unwrap();
    let expected = session.candidates_on_current_page()[1].text.clone();
    assert_eq!(session.select_candidate(1).unwrap().commit, Some(expected));
}

#[test]
fn partial_and_literal_selection_never_capture_unresolved_suffix() {
    let mut session = PinyinSession::from_lexicon_tsv("ni\t你\t100", 9).unwrap();
    type_text(&mut session, "nix");
    session.select_candidate(0).unwrap();
    assert!(session.pending_promotions().is_empty());
    session.commit().unwrap();
    assert!(session.pending_promotions().is_empty());
    type_text(&mut session, "ni");
    session.cancel().unwrap();
    assert!(session.pending_promotions().is_empty());
}

#[test]
fn sentence_promotion_is_one_complete_identity_and_active_order_is_stable() {
    use flower::engine::{promote_order, RankingOrder};
    use std::sync::Arc;
    let mut session = PinyinSession::from_lexicon_tsv("ni\t你\t100\nhao\t好\t100", 9).unwrap();
    let update = type_text(&mut session, "nihao");
    assert_eq!(update.candidates[0].text, "你好");
    session.select_candidate(0).unwrap();
    let events = session.pending_promotions();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].selected.pinyin, "ni'hao");
    assert_eq!(events[0].selected.text, "你好");
    assert_eq!(events[0].selected.consumed, 5);
    assert_eq!(events[0].order.candidates.len(), 1);

    let mut session = PinyinSession::from_lexicon_tsv(ADAPTIVE_TSV, 9).unwrap();
    let baseline = type_text(&mut session, "shi").candidates;
    session.select_candidate(1).unwrap();
    let mut orders: Vec<RankingOrder> = Vec::new();
    promote_order(&mut orders, &session.pending_promotions()[0]);
    type_text(&mut session, "shi");
    session.set_ranking_orders(Arc::new(orders));
    assert_eq!(session.candidates_on_current_page(), baseline);
    session.cancel().unwrap();
    let reordered = type_text(&mut session, "shi");
    assert_eq!(reordered.candidates[0], baseline[1]);
    let update = session.set_candidate_width(None);
    assert_eq!(update.candidates, reordered.candidates);
}

#[test]
fn expanded_homophones_can_all_be_reached_and_the_last_candidate_selected() {
    let lexicon = Lexicon::builtin().unwrap();
    let expected = lexicon.lookup("yi");
    assert!(expected.len() > DEFAULT_PAGE_SIZE);
    let mut session = PinyinSession::from_lexicon(lexicon, DEFAULT_PAGE_SIZE).unwrap();
    type_text(&mut session, "yi");
    let mut actual = Vec::new();
    loop {
        actual.extend(
            session
                .candidates_on_current_page()
                .iter()
                .map(|candidate| candidate.text.clone()),
        );
        if session.page() + 1 == session.page_count() {
            break;
        }
        session.next_page().unwrap();
    }
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|candidate| candidate.text.clone())
            .collect::<Vec<_>>()
    );
    let last = session.candidates_on_current_page().len() - 1;
    let update = session.select_candidate(last).unwrap();
    assert_eq!(
        update.commit.as_deref(),
        Some(expected.last().unwrap().text.as_str())
    );
}

fn type_text(session: &mut PinyinSession, text: &str) -> CompositionUpdate {
    let mut update = CompositionUpdate::default();
    for character in text.chars() {
        update = session
            .process_key(KeyEvent::plain(Key::Char(character)))
            .expect("typing a letter cannot fail");
    }
    update
}

#[test]
fn typing_nihao_produces_preedit_and_candidates() {
    let mut session = session();
    let update = type_text(&mut session, "nihao");

    assert!(update.handled);
    assert_eq!(update.preedit, "nihao");
    assert_eq!(update.candidates[0].text, "你好");
    assert_eq!(update.highlighted, Some(0));
    assert_eq!(update.page, 0);
    // Full and partial choices are now available beyond the first page.
    assert!(update.page_count >= 1);
    assert_eq!(update.page_count, session.page_count());
    assert_eq!(session.page_size(), DEFAULT_PAGE_SIZE);
}

#[test]
fn builtin_lexicon_covers_the_required_demo_words() {
    let lexicon = Lexicon::builtin().expect("embedded lexicon");
    for (pinyin, expected) in [
        ("ni", "你"),
        ("hao", "好"),
        ("nihao", "你好"),
        ("zhong", "中"),
        ("guo", "国"),
        ("zhongguo", "中国"),
        ("shi", "是"),
        ("jie", "解"),
    ] {
        let candidates = lexicon.lookup(pinyin);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.text == expected),
            "missing {expected} for {pinyin}"
        );
    }
}

#[test]
fn number_key_selects_and_commits_the_first_candidate() {
    let mut session = session();
    type_text(&mut session, "nihao");

    let update = session
        .process_key(KeyEvent::plain(Key::Char('1')))
        .expect("candidate selection");

    assert!(update.handled);
    assert_eq!(update.commit.as_deref(), Some("你好"));
    assert_eq!(update.preedit, "");
    assert!(update.candidates.is_empty());
    assert!(!session.is_composing());
}

#[test]
fn space_and_enter_commit_the_highlighted_candidate() {
    let mut session = session();
    type_text(&mut session, "zhongguo");
    let update = session
        .process_key(KeyEvent::plain(Key::Space))
        .expect("space commit");
    assert_eq!(update.commit.as_deref(), Some("中国"));

    type_text(&mut session, "shi");
    let update = session
        .process_key(KeyEvent::plain(Key::Enter))
        .expect("enter commit");
    assert_eq!(update.commit.as_deref(), Some("是"));
}

#[test]
fn space_commits_raw_pinyin_when_no_candidate_exists() {
    // A production dictionary may now resolve xyz as word initials.
    // Exercise the no-match contract with an explicit small dictionary instead.
    let mut session = PinyinSession::from_lexicon_tsv("ni\t你\t100", 9).unwrap();
    type_text(&mut session, "xyz");
    let update = session
        .process_key(KeyEvent::plain(Key::Space))
        .expect("raw commit");
    assert_eq!(update.commit.as_deref(), Some("xyz"));
}

#[test]
fn escape_cancels_without_committing() {
    let mut session = session();
    type_text(&mut session, "nihao");
    let update = session
        .process_key(KeyEvent::plain(Key::Escape))
        .expect("cancel");

    assert!(update.handled);
    assert!(update.cancelled);
    assert_eq!(update.commit, None);
    assert_eq!(update.preedit, "");
    assert!(update.candidates.is_empty());
    assert!(!session.is_composing());
}

#[test]
fn backspace_edits_then_clears_the_composition() {
    let mut session = session();
    type_text(&mut session, "ni");

    let update = session
        .process_key(KeyEvent::plain(Key::Backspace))
        .expect("backspace");
    assert_eq!(update.preedit, "n");
    assert!(update.handled);

    let update = session
        .process_key(KeyEvent::plain(Key::Backspace))
        .expect("clear composition");
    assert_eq!(update.preedit, "");
    assert!(update.candidates.is_empty());
}

#[test]
fn page_keys_move_between_candidate_pages() {
    let mut session = PinyinSession::from_lexicon_tsv(
        flower_dict::table("legacy-mvp").unwrap().tsv,
        DEFAULT_PAGE_SIZE,
    )
    .unwrap();
    let first = type_text(&mut session, "shi");
    assert!(first.page_count > 1);
    assert_eq!(first.page, 0);
    assert_eq!(first.candidates.len(), DEFAULT_PAGE_SIZE);

    let second = session
        .process_key(KeyEvent::plain(Key::PageDown))
        .expect("next page");
    assert_eq!(second.page, 1);
    assert_eq!(second.candidates.len(), 2);
    assert_ne!(first.candidates[0].text, second.candidates[0].text);

    let first_again = session
        .process_key(KeyEvent::plain(Key::PageUp))
        .expect("previous page");
    assert_eq!(first_again.page, 0);
    assert_eq!(first_again.candidates[0].text, first.candidates[0].text);
}

#[test]
fn arrow_keys_move_the_highlight_and_space_commits_it() {
    let mut session = session();
    let typed = type_text(&mut session, "shi");
    let expected = typed.candidates[1].text.clone();

    let moved = session
        .process_key(KeyEvent::plain(Key::Right))
        .expect("move highlight");
    assert_eq!(moved.highlighted, Some(1));

    let committed = session
        .process_key(KeyEvent::plain(Key::Space))
        .expect("commit highlighted candidate");
    assert_eq!(committed.commit.as_deref(), Some(expected.as_str()));
}

#[test]
fn control_and_alt_shortcuts_are_passed_through() {
    let mut session = session();
    type_text(&mut session, "ni");

    for modifiers in [
        Modifiers::CONTROL,
        Modifiers::ALT,
        Modifiers::META,
        Modifiers::CONTROL.with_shift(),
    ] {
        let update = session
            .process_key(KeyEvent::new(Key::Char('c'), modifiers))
            .expect("pass-through");
        assert!(!update.handled);
        assert_eq!(update.preedit, "ni");
        assert_eq!(session.preedit(), "ni");
    }
}

#[test]
fn keys_without_an_active_composition_pass_through() {
    let mut session = session();
    for key in [
        Key::Backspace,
        Key::Enter,
        Key::Space,
        Key::Escape,
        Key::PageUp,
        Key::PageDown,
        Key::Left,
        Key::Right,
        Key::Char('1'),
    ] {
        let update = session
            .process_key(KeyEvent::plain(key))
            .expect("pass-through");
        assert!(!update.handled, "{key:?} should pass through");
    }
}

#[test]
fn windows_virtual_keys_map_to_the_core_api() {
    let letter = map_windows_virtual_key(b'A' as u32, Modifiers::SHIFT).expect("letter key");
    assert_eq!(letter.key, Key::Char('a'));
    assert_eq!(letter.modifiers, Modifiers::SHIFT);

    assert_eq!(
        map_windows_virtual_key(VK_BACK, Modifiers::NONE)
            .expect("backspace")
            .key,
        Key::Backspace
    );
    assert_eq!(
        map_windows_virtual_key(VK_PRIOR, Modifiers::NONE)
            .expect("page up")
            .key,
        Key::PageUp
    );
    assert_eq!(
        map_windows_virtual_key(VK_NEXT, Modifiers::NONE)
            .expect("page down")
            .key,
        Key::PageDown
    );
    assert_eq!(
        map_windows_virtual_key(VK_RIGHT, Modifiers::NONE)
            .expect("right")
            .key,
        Key::Right
    );
    assert!(map_windows_virtual_key(0x70, Modifiers::NONE).is_none());
}

#[test]
fn selection_state_errors_are_results_not_panics() {
    let mut session = session();
    assert_eq!(
        session.select_candidate(0),
        Err(EngineError::NoActiveComposition)
    );
    assert_eq!(session.commit(), Err(EngineError::NoActiveComposition));
    assert_eq!(session.cancel(), Err(EngineError::NoActiveComposition));

    type_text(&mut session, "nihao");
    assert!(matches!(
        session.select_candidate(99),
        Err(EngineError::CandidateOutOfRange { .. })
    ));
    assert_eq!(session.preedit(), "nihao");
}

#[test]
fn empty_and_malformed_lexicons_are_rejected() {
    assert!(matches!(
        Lexicon::from_tsv("# comments only\n\n"),
        Err(EngineError::EmptyLexicon)
    ));
    assert!(matches!(
        Lexicon::from_tsv("ni\t你\n"),
        Err(EngineError::InvalidLexiconLine { line: 1, .. })
    ));
    assert!(matches!(
        Lexicon::from_tsv("Ni\t你\t1\n"),
        Err(EngineError::InvalidLexiconLine { line: 1, .. })
    ));
    assert!(matches!(
        Lexicon::from_tsv_bytes(&[0xFF, 0xFE]),
        Err(EngineError::InvalidUtf8 { .. })
    ));
}

#[test]
fn invalid_page_size_is_rejected() {
    assert!(matches!(
        PinyinSession::with_page_size(0),
        Err(EngineError::InvalidPageSize { page_size: 0 })
    ));
}

#[test]
fn engine_session_alias_uses_the_same_api() {
    let mut session = EngineSession::new().expect("engine session");
    type_text(&mut session, "ni");
    assert_eq!(session.preedit(), "ni");
}
