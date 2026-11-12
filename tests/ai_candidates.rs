use flower::engine::{EngineRouter, Key, KeyEvent};

const WORDS: &str = "jin'tian'xin'qing'hen'hao\t今天新情很好\t100\njin'tian\t今天\t100\nxin'qing'hen'hao\t新情很好\t100";

fn typed(page_size: usize) -> EngineRouter {
    let mut r = EngineRouter::from_lexicon_tsv(WORDS, page_size).unwrap();
    for c in "jintianxinqinghenhao".chars() {
        r.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    r
}

#[test]
fn display_equivalent_ai_uses_one_shared_full_slot_with_three_singles() {
    let words = "jin'tian'tou'fa'hen'hao\t今天头发很好\t1000\njin'tian\t今天\t100\njin'tian\t津田\t90\njin'tian\t金田\t80\njin'tian\t近天\t70\njin\t今\t100\njin\t金\t90\njin\t近\t80\njin\t进\t70";
    let mut r = EngineRouter::from_lexicon_tsv(words, 7).unwrap();
    for c in "jintiantoufahenhao".chars() {
        r.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let input = r.ai_input().unwrap();
    let update = r.apply_ai_candidate(&input, "今天頭发很好").unwrap();
    assert_eq!(update.highlighted, Some(0));
    assert_eq!(update.candidates[0].text, "今天头发很好");
    assert!(update.candidates[0].is_ai());
    assert_eq!(
        update
            .candidates
            .iter()
            .filter(|c| c.text == "今天头发很好")
            .count(),
        1
    );
    assert_eq!(
        update
            .candidates
            .iter()
            .map(|c| c.text.chars().count())
            .collect::<Vec<_>>(),
        [6, 2, 2, 2, 1, 1, 1]
    );
    assert_eq!(
        r.process_key(KeyEvent::plain(Key::Space))
            .unwrap()
            .commit
            .as_deref(),
        Some("今天头发很好")
    );
    assert!(r.pending_learning().is_empty());
    assert!(r.pending_promotions().is_empty());
    assert!(r.pending_phrase_learning().is_empty());
}

#[test]
fn late_ai_rebalances_first_page_and_keeps_safe_highlight_semantics() {
    let words = format!("{WORDS}\njin'tian\t津田\t90\njin'tian\t金田\t80\njin'tian\t近天\t70\njin\t今\t100\njin\t金\t90\njin\t近\t80\njin\t进\t70");
    let mut r = EngineRouter::from_lexicon_tsv(&words, 7).unwrap();
    for c in "jintianxinqinghenhao".chars() {
        r.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let before = r.session().candidates_on_current_page().to_vec();
    assert_eq!(
        before
            .iter()
            .map(|c| c.text.chars().count())
            .collect::<Vec<_>>(),
        [6, 2, 2, 2, 1, 1, 1]
    );
    let input = r.ai_input().unwrap();
    // A moved highlight makes the original request inapplicable; it must not
    // silently change what Space/Enter would select.
    r.process_key(KeyEvent::plain(Key::Right)).unwrap();
    assert!(r.apply_ai_candidate(&input, "今天心情很好").is_none());
    assert_eq!(r.session().candidates_on_current_page(), before);
    r.process_key(KeyEvent::plain(Key::Left)).unwrap();
    let update = r.apply_ai_candidate(&input, "今天心情很好").unwrap();
    assert_eq!(update.highlighted, Some(0));
    assert_eq!(update.candidates[0], before[0]);
    assert!(update.candidates[1].is_ai());
    assert_eq!(
        update
            .candidates
            .iter()
            .map(|c| c.text.chars().count())
            .collect::<Vec<_>>(),
        [6, 6, 2, 2, 1, 1, 1]
    );
    let repeated = r.apply_ai_candidate(&input, "今天心情很好").unwrap();
    assert_eq!(repeated.candidates, update.candidates);
    assert_eq!(
        r.select_candidate(1).unwrap().commit.as_deref(),
        Some("今天心情很好")
    );
    assert!(r.pending_learning().is_empty());
    assert!(r.pending_phrase_learning().is_empty());
    assert!(r.pending_promotions().is_empty());
}

#[test]
fn ai_marks_existing_correction_without_duplicate_or_learning() {
    let words = format!("{WORDS}\njin'tian'xin'qing'hen'hao\t今天心情很好\t1");
    let mut r = EngineRouter::from_lexicon_tsv(&words, 9).unwrap();
    for c in "jintianxinqinghenhao".chars() {
        r.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let input = r.ai_input().unwrap();
    assert_eq!(input.source, "今天新情很好");
    assert!(!r
        .session()
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "今天心情很好"));
    let update = r.apply_ai_candidate(&input, "今天心情很好").unwrap();
    assert_eq!(update.candidates[0].text, "今天新情很好");
    assert_eq!(update.candidates[1].display_text(), "今天心情很好 ✓ AI");
    assert_eq!(
        update
            .candidates
            .iter()
            .filter(|c| c.text == "今天心情很好")
            .count(),
        1
    );
    let repeated = r.apply_ai_candidate(&input, "今天心情很好").unwrap();
    assert_eq!(
        repeated
            .candidates
            .iter()
            .filter(|c| c.is_ai() || c.is_spelling())
            .count(),
        1
    );
    assert_eq!(
        r.select_candidate(1).unwrap().commit.as_deref(),
        Some("今天心情很好")
    );
    assert!(r.pending_learning().is_empty());
    assert!(r.pending_promotions().is_empty());
    assert!(r.pending_phrase_learning().is_empty());
}

#[test]
fn ai_candidate_uses_real_selection_and_never_learns() {
    for method in 0..4 {
        let mut r = typed(9);
        let input = r.ai_input().unwrap();
        let original = r.preedit().to_owned();
        let update = r.apply_ai_candidate(&input, "今天心情很好").unwrap();
        assert_eq!(r.preedit(), original);
        assert!(update.candidates[1].is_ai());
        assert_eq!(update.candidates[1].display_text(), "今天心情很好 ✓ AI");
        let update = match method {
            0 => r.process_key(KeyEvent::plain(Key::Char('2'))).unwrap(),
            1 | 2 => {
                r.process_key(KeyEvent::plain(Key::Right)).unwrap();
                r.process_key(KeyEvent::plain(if method == 1 {
                    Key::Space
                } else {
                    Key::Enter
                }))
                .unwrap()
            }
            _ => r.select_candidate(1).unwrap(),
        };
        assert_eq!(update.commit.as_deref(), Some("今天心情很好"));
        assert!(r.pending_learning().is_empty());
        assert!(r.pending_promotions().is_empty());
    }
}

#[test]
fn ai_whole_sentence_replaces_uncommitted_confirmed_prefix_without_old_events() {
    let mut r = typed(9);
    let index = r
        .session()
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "今天")
        .unwrap();
    assert!(r.select_candidate(index).unwrap().commit.is_none());
    assert!(r.preedit().starts_with("今天"));
    let input = r.ai_input().unwrap();
    assert_eq!(input.source, "今天新情很好");
    r.apply_ai_candidate(&input, "今天心情很好").unwrap();
    assert_eq!(
        r.select_candidate(1).unwrap().commit.as_deref(),
        Some("今天心情很好")
    );
    assert!(r.pending_learning().is_empty());
    assert!(r.pending_promotions().is_empty());
}

#[test]
fn ai_rejects_stale_moved_duplicate_and_invalid_results_and_keeps_real_paging() {
    let mut r = typed(1);
    let input = r.ai_input().unwrap();
    assert!(r.apply_ai_candidate(&input, "今天新情很好").is_none());
    assert!(r.apply_ai_candidate(&input, "今天心情很好呀").is_none());
    r.next_page().unwrap();
    assert!(r.apply_ai_candidate(&input, "今天心情很好").is_none());
    r.previous_page().unwrap();
    r.apply_ai_candidate(&input, "今天心情很好").unwrap();
    assert_eq!(
        r.session().candidates_on_current_page()[0].text,
        "今天新情很好"
    );
    r.next_page().unwrap();
    assert!(r.session().candidates_on_current_page()[0].is_ai());
    assert_eq!(
        r.select_candidate(0).unwrap().commit.as_deref(),
        Some("今天心情很好")
    );
    let mut r = typed(9);
    let input = r.ai_input().unwrap();
    r.apply_ai_candidate(&input, "今天心情很好").unwrap();
    r.select_candidate(0).unwrap();
    assert!(!r.pending_promotions()[0]
        .order
        .candidates
        .iter()
        .any(|c| c.pinyin == flower::ai::AI_COMMENT));
    let mut r = typed(9);
    let input = r.ai_input().unwrap();
    r.process_key(KeyEvent::plain(Key::Backspace)).unwrap();
    assert!(r.apply_ai_candidate(&input, "今天心情很好").is_none());
}
