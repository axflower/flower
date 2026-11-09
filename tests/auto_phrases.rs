use flower::engine::{Key, KeyEvent, LearningEvent, Lexicon, PinyinSession};
use flower::user_dictionary::{DictionaryStore, LearningWorker};
use std::{path::PathBuf, sync::Arc};

fn type_text(s: &mut PinyinSession, raw: &str) {
    for ch in raw.chars() {
        s.process_key(KeyEvent::plain(Key::Char(ch))).unwrap();
    }
}

fn choose(s: &mut PinyinSession, text: &str) -> flower::CompositionUpdate {
    for _ in 0..s.page_count() {
        if let Some(i) = s
            .candidates_on_current_page()
            .iter()
            .position(|c| c.text == text && !c.is_ai() && !c.is_spelling())
        {
            return s.select_candidate(i).unwrap();
        }
        s.next_page().unwrap();
    }
    panic!("missing ordinary {text}: {}", s.preedit());
}

fn compose(s: &mut PinyinSession) {
    type_text(s, "xinjiajuan");
    assert!(choose(s, "新").commit.is_none());
    assert!(s.pending_phrase_learning().is_empty());
    assert!(choose(s, "加").commit.is_none());
    assert!(s.pending_phrase_learning().is_empty());
    assert_eq!(choose(s, "卷").commit.as_deref(), Some("新加卷"));
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = std::env::var_os("FLOWER_TEST_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!(
            "auto-phrases-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn builtin_explicit_word_is_available_after_worker_reload_in_full_and_initials() {
    let base = Lexicon::builtin().unwrap();
    assert!(!base
        .lookup("xin'jia'juan")
        .iter()
        .any(|c| c.text == "新加卷"));
    let mut s = PinyinSession::from_lexicon(base.clone(), 9).unwrap();
    compose(&mut s);
    let expected = LearningEvent {
        pinyin: "xin'jia'juan".into(),
        text: "新加卷".into(),
    };
    assert_eq!(s.pending_phrase_learning(), &[expected]);
    let fixture = Fixture::new();
    let store = DictionaryStore::new(fixture.0.join("dictionary.json"));
    let data = store.load().unwrap();
    let mut worker = LearningWorker::start(store.clone(), data.generation).unwrap();
    worker
        .submit_commit(s.pending_promotions(), s.pending_phrase_learning())
        .unwrap();
    worker.shutdown().unwrap();
    let saved = store.load().unwrap();
    assert_eq!(saved.auto_phrases.len(), 1);
    let personal = Arc::new(Lexicon::from_tsv(&saved.auto_phrase_tsv().unwrap()).unwrap());
    for raw in ["xinjiajuan", "xjj", "xinjiaj"] {
        let mut next = PinyinSession::from_lexicon(base.clone(), 9).unwrap();
        next.set_auto_phrase_lexicon(Some(personal.clone()));
        type_text(&mut next, raw);
        let first = &next.candidates_on_current_page()[0];
        assert_eq!(first.text, "新加卷", "{raw}");
        assert_eq!(first.comment, "xin'jia'juan");
        assert!(!first.is_ai() && !first.is_spelling());
        assert_eq!(
            choose(&mut next, "新加卷").commit.as_deref(),
            Some("新加卷")
        );
        assert!(next.pending_phrase_learning().is_empty());
    }
}

#[test]
fn unfinished_cancelled_and_known_words_never_create_a_phrase() {
    let fixture = "xin\t新\t100\njia\t加\t100\njuan\t卷\t100\nxin'jia'juan\t新加卷\t200";
    let mut known = PinyinSession::from_lexicon_tsv(fixture, 9).unwrap();
    compose(&mut known);
    assert!(known.pending_phrase_learning().is_empty());
    let mut s = PinyinSession::from_lexicon_tsv("xin\t新\t100", 9).unwrap();
    type_text(&mut s, "xinqq");
    choose(&mut s, "新");
    let update = s.process_key(KeyEvent::plain(Key::Enter)).unwrap();
    assert_eq!(update.commit.as_deref(), Some("新qq"));
    assert!(s.pending_phrase_learning().is_empty());
    let mut s = PinyinSession::new().unwrap();
    type_text(&mut s, "xinjiajuan");
    choose(&mut s, "新");
    choose(&mut s, "加");
    s.cancel().unwrap();
    assert!(s.pending_phrase_learning().is_empty());
}

#[test]
fn direct_whole_choice_and_raw_commit_do_not_learn_while_punctuation_excludes_itself() {
    let mut s =
        PinyinSession::from_lexicon_tsv("xin\t新\t100\njia\t加\t100\njuan\t卷\t100", 9).unwrap();
    type_text(&mut s, "xinjiajuan");
    choose(&mut s, "新加卷");
    assert!(s.pending_phrase_learning().is_empty());
    type_text(&mut s, "xinjiajuan");
    choose(&mut s, "新");
    assert_eq!(s.commit_raw().commit.as_deref(), Some("xinjiajuan"));
    assert!(s.pending_phrase_learning().is_empty());
    type_text(&mut s, "xinjiajuan");
    choose(&mut s, "新");
    choose(&mut s, "加");
    let update = s.process_key(KeyEvent::plain(Key::Char(','))).unwrap();
    assert_eq!(update.commit.as_deref(), Some("新加卷，"));
    assert_eq!(s.pending_phrase_learning()[0].text, "新加卷");
    assert_eq!(s.pending_phrase_learning()[0].pinyin, "xin'jia'juan");
}

#[test]
fn multi_character_segments_and_initials_use_selected_full_readings() {
    let words = "xing'he\t星河\t100\njuan'zong\t卷宗\t100";
    // Existing partial selection uses full Pinyin for the prefix; the final
    // segment can be abbreviated and must still contribute its full reading.
    for raw in ["xinghejuanzong", "xinghejz"] {
        let mut s = PinyinSession::from_lexicon_tsv(words, 9).unwrap();
        type_text(&mut s, raw);
        choose(&mut s, "星河");
        assert!(s.pending_phrase_learning().is_empty());
        assert_eq!(choose(&mut s, "卷宗").commit.as_deref(), Some("星河卷宗"));
        assert_eq!(
            s.pending_phrase_learning(),
            &[LearningEvent {
                pinyin: "xing'he'juan'zong".into(),
                text: "星河卷宗".into(),
            }]
        );
    }
}

#[test]
fn existing_explicit_order_precedes_new_auto_word_and_index_waits_for_idle() {
    let words = "xin'jia'juan\t新假卷\t100";
    let mut s = PinyinSession::from_lexicon_tsv(words, 9).unwrap();
    type_text(&mut s, "xinjiajuan");
    choose(&mut s, "新假卷");
    let order = s.pending_promotions()[0].order.clone();
    let personal = Arc::new(Lexicon::from_tsv("xin'jia'juan\t新加卷\t16384").unwrap());
    s.set_ranking_orders(Arc::new(vec![order]));
    type_text(&mut s, "xinjiajuan");
    s.set_auto_phrase_lexicon(Some(personal.clone()));
    assert!(!s
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "新加卷"));
    s.cancel().unwrap();
    s.set_auto_phrase_lexicon(Some(personal));
    type_text(&mut s, "xinjiajuan");
    assert_eq!(s.candidates_on_current_page()[0].text, "新假卷");
    assert!(s
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "新加卷"));
}
