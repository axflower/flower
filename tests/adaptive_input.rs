use flower::engine::{Key, KeyEvent, Lexicon, PinyinSession};
use flower::user_dictionary::DictionaryStore;
use std::path::PathBuf;
use std::sync::Arc;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn builtin_session(raw: &str, orders: Vec<flower::engine::RankingOrder>) -> PinyinSession {
    let mut session = PinyinSession::from_lexicon(Lexicon::builtin().unwrap(), 9).unwrap();
    session.set_candidate_width(Some(4096));
    session.set_ranking_orders(Arc::new(orders));
    for letter in raw.chars() {
        session
            .process_key(KeyEvent::plain(Key::Char(letter)))
            .unwrap();
    }
    session
}

#[test]
fn builtin_ambiguities_promote_only_one_step_and_keep_raw_domains_separate() {
    use flower::engine::promote_order;
    for raw in ["wenti", "shijie", "shijian"] {
        let mut session = builtin_session(raw, Vec::new());
        let before = session.candidates_on_current_page().to_vec();
        assert!(before.len() >= 3, "{raw} must exercise a later candidate");
        let selected = before[2].text.clone();
        session.select_candidate(2).unwrap();
        let event = session.pending_promotions().last().unwrap().clone();
        // Persistence keeps the original dictionary identity; the input
        // surface is projected to the selected script without rewriting it.
        assert_eq!(
            flower::ChineseScript::Simplified.convert(&event.selected.text),
            selected
        );
        assert_eq!(event.selected.consumed, raw.len());
        let mut orders = Vec::new();
        assert!(promote_order(&mut orders, &event));
        let mut next = builtin_session(raw, orders.clone());
        assert_eq!(next.candidates_on_current_page()[0], before[0]);
        assert_eq!(next.candidates_on_current_page()[1].text, selected);
        assert_eq!(next.candidates_on_current_page()[2], before[1]);
        next.select_candidate(1).unwrap();
        assert!(promote_order(
            &mut orders,
            next.pending_promotions().last().unwrap()
        ));
        assert_eq!(
            builtin_session(raw, orders.clone()).candidates_on_current_page()[0].text,
            selected
        );
        // A saved full-spelling order must not leak into initials or mixed input.
        let domains: &[&str] = match raw {
            "wenti" => &["wt", "wen't", "wen'ti"],
            "shijie" => &["sj", "shi'j", "shi'jie"],
            _ => &["sj", "shi'jian", "shi'j"],
        };
        for domain in domains {
            assert_eq!(
                builtin_session(domain, orders.clone()).candidates_on_current_page(),
                builtin_session(domain, Vec::new()).candidates_on_current_page(),
                "{raw} must not reorder {domain}"
            );
        }
    }
}

#[test]
fn selected_mapping_moves_one_position_and_survives_a_new_store_and_session() {
    let root = std::env::var_os("FLOWER_TEST_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(root.join(format!("adaptive-input-{}-{nonce}", std::process::id())));
    std::fs::create_dir(&fixture.0).unwrap();
    let store = DictionaryStore::new(fixture.0.join("dictionary.json"));
    let initial = store.load().unwrap();
    assert!(initial.adaptive_ranking_enabled);
    // Explicit second-position fixture, independent of upstream frequency changes.
    let mut session =
        PinyinSession::from_lexicon_tsv("ying'she\t影射\t200\nying'she\t映射\t100", 9).unwrap();
    session.set_candidate_width(Some(544));
    for letter in "yingshe".chars() {
        session
            .process_key(KeyEvent::plain(Key::Char(letter)))
            .unwrap();
    }
    assert_eq!(session.candidates_on_current_page()[1].text, "映射");
    let committed = session.select_candidate(1).unwrap();
    assert_eq!(committed.commit.as_deref(), Some("映射"));
    let events = session.pending_promotions().to_vec();
    assert_eq!(events.len(), 1);
    store
        .promote_if_generation(initial.generation, &events)
        .unwrap();
    drop(store);

    let reopened = DictionaryStore::new(fixture.0.join("dictionary.json"));
    let persisted = reopened.load().unwrap();
    assert_eq!(persisted.schema_version, 3);
    // The saved stable identity also applies when the real embedded dictionary
    // includes additional candidates not present in the original fixture.
    assert!(Lexicon::builtin()
        .unwrap()
        .lookup("yingshe")
        .iter()
        .any(|c| c.text == "映射"));
    let mut restarted = PinyinSession::from_lexicon(Lexicon::builtin().unwrap(), 9).unwrap();
    restarted.set_ranking_orders(Arc::new(persisted.rankings));
    restarted.set_candidate_width(Some(544));
    for letter in "yingshe".chars() {
        restarted
            .process_key(KeyEvent::plain(Key::Char(letter)))
            .unwrap();
    }
    assert_eq!(restarted.candidates_on_current_page()[0].text, "映射");
    assert_eq!(
        restarted.select_candidate(0).unwrap().commit.as_deref(),
        Some("映射")
    );
}
