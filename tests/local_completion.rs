use flower::engine::{EngineRouter, Key, KeyEvent, Lexicon};

const POEM: &str = "君不见黄河之水天上来";
const READING: &str = "jun'bu'jian'huang'he'zhi'shui'tian'shang'lai";
const WORDS: &str = "jun\t君\t100\nbu\t不\t100\njian\t见\t100\nhuang\t黄\t100\nhe\t河\t100\njun'bu'jian\t君不见\t1000\njun'bu'jian'huang'he\t君不见黄河\t1000\njun'bu'jian'huang'he'zhi'shui'tian'shang'lai\t君不见黄河之水天上来\t500\nwo\t我\t1000";
fn typed(raw: &str) -> EngineRouter {
    let mut r = EngineRouter::from_lexicon_tsv(WORDS, 7).unwrap();
    type_into(&mut r, raw);
    r
}
fn type_into(r: &mut EngineRouter, raw: &str) {
    for c in raw.chars() {
        assert!(r
            .process_key(KeyEvent::plain(Key::Char(c)))
            .unwrap()
            .commit
            .is_none());
    }
}
fn select_completion(r: &mut EngineRouter) -> String {
    let candidates = r.session().candidates_on_current_page();
    let index = candidates
        .iter()
        .position(|c| c.is_completion())
        .expect("completion on first page");
    assert!(candidates[index].display_text().contains("联想"));
    assert!(!candidates[index].is_ai() && !candidates[index].is_spelling());
    let result = r.select_candidate(index).unwrap().commit.unwrap();
    assert!(r.pending_learning().is_empty());
    assert!(r.pending_promotions().is_empty());
    assert!(r.pending_phrase_learning().is_empty());
    assert!(r.pending_input().is_empty());
    result
}
#[test]
fn prefix_completion_is_explicit_preserves_first_and_never_learns() {
    for (raw, expected) in [
        ("junbujian", "君不见黄河"),
        ("junbujianhuanghe", POEM),
        ("jun'bu'jian'huang'he'", POEM),
    ] {
        let mut r = typed(raw);
        assert_eq!(r.pending_input(), raw);
        let page = r.session().candidates_on_current_page();
        assert!(!page[0].is_completion());
        assert!(page.len() <= 7);
        assert_eq!(page.iter().filter(|c| c.is_completion()).count(), 1);
        assert_eq!(select_completion(&mut r), expected);
    }
}
#[test]
fn completion_requires_three_whole_syllables_and_is_not_a_full_match_duplicate() {
    for raw in [
        "junbu",
        "junbujia",
        "jbj",
        "junbujianhuanghezhi",
        "junbujianhuanghezhishuitianshanglai",
    ] {
        let r = typed(raw);
        if raw == "junbujianhuanghezhi" {
            assert!(r
                .session()
                .candidates_on_current_page()
                .iter()
                .any(|c| c.is_completion()));
        } else {
            assert!(
                r.session()
                    .candidates_on_current_page()
                    .iter()
                    .all(|c| !c.is_completion()),
                "{raw}"
            );
        }
    }
}
#[test]
fn small_pages_keep_normal_first_and_completion_reachable() {
    for page_size in [1, 2] {
        let mut r = EngineRouter::from_lexicon_tsv(WORDS, page_size).unwrap();
        type_into(&mut r, "junbujianhuanghe");
        assert!(!r.session().candidates_on_current_page()[0].is_completion());
        if page_size == 1 {
            r.next_page().unwrap();
        }
        assert!(r.session().candidates_on_current_page().len() <= page_size);
        assert_eq!(select_completion(&mut r), POEM);
    }
}
#[test]
fn highlighted_completion_uses_the_same_space_and_enter_submission() {
    for key in [Key::Space, Key::Enter] {
        let mut r = typed("junbujianhuanghe");
        let index = r
            .session()
            .candidates_on_current_page()
            .iter()
            .position(|c| c.is_completion())
            .unwrap();
        for _ in 0..index {
            r.process_key(KeyEvent::plain(Key::Right)).unwrap();
        }
        assert_eq!(
            r.process_key(KeyEvent::plain(key))
                .unwrap()
                .commit
                .as_deref(),
            Some(POEM)
        );
        assert!(r.pending_learning().is_empty());
        assert!(r.pending_promotions().is_empty());
        assert!(r.pending_phrase_learning().is_empty());
    }
}
#[test]
fn completion_stays_separate_from_unique_correction_and_ai_source() {
    let mut r = typed("junbujianhuanghe");
    let input = r.ai_input().unwrap();
    assert_eq!(input.source, "君不见黄河");
    let updated = r.apply_ai_candidate(&input, "君不见黄和").unwrap();
    assert!(!updated.candidates[0].is_completion());
    assert!(updated.candidates[1].is_ai());
    assert!(updated.candidates[2].is_completion());
    assert_eq!(
        updated
            .candidates
            .iter()
            .filter(|c| c.is_ai() || c.is_spelling())
            .count(),
        1
    );
    assert_eq!(select_completion(&mut r), POEM);
}
#[test]
fn completion_deletion_cancellation_and_raw_commit_do_not_learn() {
    let mut r = typed("junbujian");
    r.process_key(KeyEvent::plain(Key::Backspace)).unwrap();
    assert!(r
        .session()
        .candidates_on_current_page()
        .iter()
        .all(|c| !c.is_completion()));
    let cancelled = r.process_key(KeyEvent::plain(Key::Escape)).unwrap();
    assert!(cancelled.cancelled && cancelled.commit.is_none());
    assert!(r.pending_learning().is_empty());
    let mut r = typed("junbujianhuanghe");
    assert_eq!(r.commit_raw().commit.as_deref(), Some("junbujianhuanghe"));
    assert!(r.pending_learning().is_empty());
}
#[test]
fn completion_keeps_confirmed_prefix_once_and_converts_entire_output() {
    let mut r = typed("wojunbujianhuanghe");
    let i = r
        .session()
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "我")
        .unwrap();
    assert!(r.select_candidate(i).unwrap().commit.is_none());
    assert_eq!(r.pending_input(), "junbujianhuanghe");
    r.project_chinese_script(flower::ChineseScript::Traditional);
    assert_eq!(select_completion(&mut r), "我君不見黃河之水天上來");
}
#[test]
fn builtin_poem_is_retrieved_from_existing_table_and_completed_before_full_input() {
    let lexicon = Lexicon::builtin().unwrap();
    assert!(lexicon
        .lookup(&READING.replace('\'', ""))
        .iter()
        .any(|c| c.text == POEM));
    let mut r = EngineRouter::from_lexicon(lexicon, 7).unwrap();
    type_into(&mut r, "junbujianhuanghe");
    assert_eq!(r.pending_input(), "junbujianhuanghe");
    assert_eq!(select_completion(&mut r), POEM);
}
