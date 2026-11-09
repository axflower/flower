use flower::engine::{
    CompositionUpdate, EngineError, EngineRouter, Key, KeyEvent, Modifiers, DEFAULT_PAGE_SIZE,
};

fn router() -> EngineRouter {
    EngineRouter::new().expect("embedded MVP lexicon must load")
}

fn type_text(router: &mut EngineRouter, text: &str) -> CompositionUpdate {
    let mut update = CompositionUpdate::default();
    for character in text.chars() {
        update = router
            .process_key(KeyEvent::plain(Key::Char(character)))
            .expect("typing a letter cannot fail");
    }
    update
}

#[test]
fn test_key_reports_handled_keys_without_an_active_composition() {
    let router = router();

    assert_eq!(router.test_key(KeyEvent::plain(Key::Char('n'))), Ok(true));
    assert_eq!(router.test_key(KeyEvent::plain(Key::Char('!'))), Ok(true));
    assert_eq!(router.test_key(KeyEvent::plain(Key::Char('@'))), Ok(false));

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
        assert_eq!(
            router.test_key(KeyEvent::plain(key)),
            Ok(false),
            "{key:?} should pass through without a composition"
        );
    }
}

#[test]
fn test_key_passes_command_shortcuts_through() {
    let mut router = router();
    type_text(&mut router, "ni");

    for modifiers in [
        Modifiers::CONTROL,
        Modifiers::ALT,
        Modifiers::META,
        Modifiers::CONTROL.with_shift(),
    ] {
        assert_eq!(
            router.test_key(KeyEvent::new(Key::Char('c'), modifiers)),
            Ok(false)
        );
    }

    assert_eq!(router.test_key(KeyEvent::plain(Key::Char('h'))), Ok(true));
    assert_eq!(router.preedit(), "ni");
}

#[test]
fn test_key_does_not_change_state_or_later_commits() {
    let mut router = router();
    let typed = type_text(&mut router, "shi");
    let expected_from_first_page = typed.candidates[0].text.clone();

    assert_eq!(router.test_key(KeyEvent::plain(Key::PageDown)), Ok(true));
    assert_eq!(router.test_key(KeyEvent::plain(Key::Char('2'))), Ok(true));
    assert_eq!(router.test_key(KeyEvent::plain(Key::Escape)), Ok(true));

    assert_eq!(router.preedit(), typed.preedit);
    assert_eq!(
        router.session().candidates_on_current_page(),
        typed.candidates.as_slice()
    );
    assert_eq!(router.session().page(), typed.page);
    assert_eq!(router.session().highlighted(), typed.highlighted);

    let committed = router
        .process_key(KeyEvent::plain(Key::Space))
        .expect("space commit");
    assert_eq!(
        committed.commit.as_deref(),
        Some(expected_from_first_page.as_str())
    );

    let typed = type_text(&mut router, "shi");
    let expected_from_preview = typed.candidates[1].text.clone();
    assert_eq!(router.test_key(KeyEvent::plain(Key::Char('2'))), Ok(true));
    assert!(router.is_composing());

    let committed = router
        .process_key(KeyEvent::plain(Key::Char('2')))
        .expect("candidate selection");
    assert_eq!(
        committed.commit.as_deref(),
        Some(expected_from_preview.as_str())
    );
}

#[test]
fn process_key_advances_selection_commit_cancel_and_paging() {
    // Keep a two-page fixture to test the short last page independently of
    // future expansion of the production dictionary.
    let mut router =
        EngineRouter::from_lexicon_tsv(flower_dict::table("legacy-mvp").unwrap().tsv, 9).unwrap();
    let typed = type_text(&mut router, "shi");
    assert!(router.is_composing());
    assert_eq!(typed.page_count, 2);

    let second_page = router
        .process_key(KeyEvent::plain(Key::PageDown))
        .expect("next page");
    assert_eq!(second_page.page, 1);
    assert_eq!(second_page.candidates.len(), 2);

    let first_page = router
        .process_key(KeyEvent::plain(Key::PageUp))
        .expect("previous page");
    assert_eq!(first_page.page, 0);
    assert_eq!(first_page.candidates[0].text, typed.candidates[0].text);

    let expected_highlighted = typed.candidates[1].text.clone();
    let moved = router
        .process_key(KeyEvent::plain(Key::Right))
        .expect("move highlight");
    assert_eq!(moved.highlighted, Some(1));

    let committed = router
        .process_key(KeyEvent::plain(Key::Space))
        .expect("space commit");
    assert_eq!(
        committed.commit.as_deref(),
        Some(expected_highlighted.as_str())
    );
    assert!(!router.is_composing());

    type_text(&mut router, "ni");
    let committed = router
        .process_key(KeyEvent::plain(Key::Enter))
        .expect("enter commit");
    assert_eq!(committed.commit.as_deref(), Some("你"));

    type_text(&mut router, "ni");
    let selected = router
        .process_key(KeyEvent::plain(Key::Char('1')))
        .expect("digit selection");
    assert_eq!(selected.commit.as_deref(), Some("你"));

    type_text(&mut router, "ni");
    let cancelled = router
        .process_key(KeyEvent::plain(Key::Escape))
        .expect("cancel");
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.commit, None);
    assert!(!router.is_composing());
}

#[test]
fn constructor_errors_are_returned_without_panicking() {
    assert!(matches!(
        EngineRouter::with_page_size(0),
        Err(EngineError::InvalidPageSize { page_size: 0 })
    ));
    assert!(matches!(
        EngineRouter::from_lexicon_tsv("# comments only\n\n", DEFAULT_PAGE_SIZE),
        Err(EngineError::EmptyLexicon)
    ));
    assert!(matches!(
        EngineRouter::from_lexicon_tsv("ni\t你\n", DEFAULT_PAGE_SIZE),
        Err(EngineError::InvalidLexiconLine { line: 1, .. })
    ));
}
