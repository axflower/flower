use flower::engine::keymap::{map_windows_virtual_key, VK_BACK, VK_NEXT, VK_PRIOR, VK_RIGHT};
use flower::engine::{
    CompositionUpdate, EngineError, EngineSession, Key, KeyEvent, Lexicon, Modifiers,
    PinyinSession, DEFAULT_PAGE_SIZE,
};

fn session() -> PinyinSession {
    PinyinSession::new().expect("embedded MVP lexicon must load")
}

const ADAPTIVE_TSV: &str = "shi\t是\t900\nshi\t时\t800\nshi\t十\t700\nshi\t事\t600\nshi\t市\t500\nshi\t世界很长的候选文本\t400\nshi\t诗\t300";

const QUOTA_TSV: &str = "jin'tian'wo'men'gong'zuo\t今天我们工作\t1000\njin'tian\t今天\t100\njin'tian\t津田\t90\njin'tian\t金田\t80\njin'tian\t近天\t70\njin\t今\t100\njin\t金\t90\njin\t近\t80\njin\t进\t70\nwo'men\t我们\t100\ngong'zuo\t工作\t100";

#[test]
fn first_page_quotas_preserve_page_size_and_remaining_ranked_choices() {
    for (size, word_count, single_count) in [(1, 0, 0), (2, 0, 1), (4, 0, 3), (7, 3, 3), (9, 4, 4)]
    {
        let mut s = PinyinSession::from_lexicon_tsv(QUOTA_TSV, size).unwrap();
        let update = type_text(&mut s, "jintianwomengongzuo");
        assert_eq!(s.page_size(), size);
        assert_eq!(update.candidates[0].text, "今天我们工作");
        assert_eq!(update.candidates.len(), size);
        assert_eq!(
            update
                .candidates
                .iter()
                .skip(1)
                .filter(|c| c.text.chars().count() == 2)
                .count(),
            word_count
        );
        assert_eq!(
            update
                .candidates
                .iter()
                .filter(|c| c.text.chars().count() == 1)
                .count(),
            single_count
        );
        let mut all = update.candidates;
        while s.page() + 1 < s.page_count() {
            all.extend(s.next_page().unwrap().candidates);
        }
        let words: Vec<_> = all
            .iter()
            .filter(|c| c.text.chars().count() == 2)
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(words, ["今天", "津田", "金田", "近天"]);
        let singles: Vec<_> = all
            .iter()
            .filter(|c| c.text.chars().count() == 1)
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(singles, ["今", "金", "近", "进"]);
    }
}

#[test]
fn first_page_quotas_fill_short_tiers_and_resize_without_changing_selection() {
    for (remove, expected) in [
        (
            "\njin'tian\t津田\t90\njin'tian\t金田\t80\njin'tian\t近天\t70",
            [1, 4],
        ),
        ("\njin\t金\t90\njin\t近\t80\njin\t进\t70", [4, 1]),
    ] {
        let words = QUOTA_TSV.replace(remove, "");
        let mut s = PinyinSession::from_lexicon_tsv(&words, 7).unwrap();
        let update = type_text(&mut s, "jintianwomengongzuo");
        assert_eq!(update.candidates.len(), 6);
        assert_eq!(
            update
                .candidates
                .iter()
                .filter(|c| c.text.chars().count() == 2)
                .count(),
            expected[0]
        );
        assert_eq!(
            update
                .candidates
                .iter()
                .filter(|c| c.text.chars().count() == 1)
                .count(),
            expected[1]
        );
    }
    let mut s = PinyinSession::from_lexicon_tsv(QUOTA_TSV, 7).unwrap();
    type_text(&mut s, "jintianwomengongzuo");
    for _ in 0..4 {
        s.process_key(KeyEvent::plain(Key::Right)).unwrap();
    }
    assert_eq!(
        s.candidates_on_current_page()[s.highlighted().unwrap()].text,
        "今"
    );
    s.set_page_size(4).unwrap();
    assert_eq!(
        s.candidates_on_current_page()[s.highlighted().unwrap()].text,
        "今"
    );
    // Enter commits literal raw when the highlight consumes only a prefix;
    // Space selects that real prefix and leaves the remaining raw composing.
    let mut enter_session = s.clone();
    assert_eq!(
        enter_session
            .process_key(KeyEvent::plain(Key::Enter))
            .unwrap()
            .commit
            .as_deref(),
        Some("jintianwomengongzuo")
    );
    let update = s.process_key(KeyEvent::plain(Key::Space)).unwrap();
    assert!(update.commit.is_none());
    assert_eq!(s.pending_input(), "tianwomengongzuo");
    assert_eq!(s.preedit(), "今tianwomengongzuo");
    assert!(s.pending_learning().is_empty());
}

#[test]
fn first_page_quotas_fall_back_to_other_real_prefixes() {
    let mut s = PinyinSession::from_lexicon_tsv(
        "jin'tian'wo'men'gong'zuo\t今天我们工作\t1000\njin'tian'wo'men\t今天我们\t100\njin'tian\t今天\t100\ngong'zuo\t工作\t100",
        7,
    ).unwrap();
    let update = type_text(&mut s, "jintianwomengongzuo");
    assert_eq!(
        update
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["今天我们工作", "今天", "今天我们"]
    );
    assert!(s.select_candidate(2).unwrap().commit.is_none());
    assert_eq!(s.pending_input(), "gongzuo");
    assert_eq!(
        s.select_candidate(0).unwrap().commit.as_deref(),
        Some("今天我们工作")
    );
    assert!(!s.pending_learning().is_empty());
}

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
fn fixed_pages_cover_all_choices_and_preserve_absolute_highlight() {
    let mut session = PinyinSession::from_lexicon_tsv(ADAPTIVE_TSV, 7).unwrap();
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
        assert_eq!(page.len(), 7.min(baseline.len() - actual.len()));
        actual.extend_from_slice(page);
        if session.page() + 1 == session.page_count() {
            break;
        }
        session.next_page().unwrap();
    }
    assert_eq!(actual, baseline);
    let wide = session.set_candidate_width(Some(10000));
    assert_eq!(wide.candidates.len(), 7);
    let expected = wide.candidates[wide.highlighted.unwrap()].text.clone();
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
        let mut router = EngineRouter::from_lexicon_tsv(ADAPTIVE_TSV, 3).unwrap();
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
    let mut session = PinyinSession::from_lexicon_tsv(ADAPTIVE_TSV, 3).unwrap();
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
    let mut seen = std::collections::HashSet::new();
    // The source retains both scripts. The input surface must expose every
    // distinct simplified output exactly once, in stable source order.
    let expected: Vec<_> = lexicon
        .lookup("yi")
        .into_iter()
        .filter_map(|mut candidate| {
            candidate.text = flower::ChineseScript::Simplified.convert(&candidate.text);
            seen.insert((candidate.text.clone(), candidate.comment.clone()))
                .then_some(candidate)
        })
        .collect();
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
    assert_eq!(second.candidates.len(), 4);
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
