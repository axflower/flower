use flower::{ChineseScript, EngineRouter, Key, KeyEvent, Settings};

const WORDS: &str = "zhong'guo\t中国\t100\nzhong'guo\t中國\t90\nruan'jian\t软件\t100\ntou'fa\t头发\t100\nfa'zhan\t发展\t100";

fn typed(raw: &str) -> EngineRouter {
    let mut router = EngineRouter::from_lexicon_tsv(WORDS, 9).unwrap();
    assert!(router.set_chinese_script(ChineseScript::Traditional));
    for c in raw.chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    router
}

#[test]
fn chinese_script_submission_and_identity() {
    for raw in ["zhongguo", "zg", "zhongg"] {
        let mut router = typed(raw);
        assert!(!router.set_chinese_script(ChineseScript::Simplified));
        let choices = router.session().candidates_on_current_page();
        let index = choices.iter().position(|c| c.text == "中國").unwrap();
        assert_eq!(choices.iter().filter(|c| c.text == "中國").count(), 1);
        let update = router.select_candidate(index).unwrap();
        assert_eq!(update.commit.as_deref(), Some("中國"));
        assert!(router
            .session()
            .pending_learning()
            .iter()
            .all(|event| event.text == "中国"));
        assert!(router.set_chinese_script(ChineseScript::Simplified));
    }
}

#[test]
fn chinese_script_partial_and_raw() {
    let mut router = typed("zhongguoruanjian");
    let index = router
        .session()
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "中國")
        .unwrap();
    let update = router.select_candidate(index).unwrap();
    assert_eq!(update.preedit, "中國ruanjian");
    assert_eq!(
        router.select_candidate(0).unwrap().commit.as_deref(),
        Some("中國軟件")
    );
    let mut router = typed("zhongguo");
    assert_eq!(router.commit_raw().commit.as_deref(), Some("zhongguo"));
    assert!(router.session().pending_learning().is_empty());
}

#[test]
fn chinese_script_settings_compatibility() {
    assert_eq!(
        serde_json::from_str::<Settings>("{}")
            .unwrap()
            .chinese_script,
        ChineseScript::Simplified
    );
    let settings = Settings {
        chinese_script: ChineseScript::Traditional,
        ..Settings::default()
    };
    assert_eq!(
        serde_json::from_str::<Settings>(&serde_json::to_string(&settings).unwrap()).unwrap(),
        settings
    );
}

#[test]
fn chinese_script_ai_proof_and_no_learning() {
    let mut router = EngineRouter::from_lexicon_tsv(
        "zhong'guo'fa'zhan\t中国发站\t100\nzhong'guo'fa'zhan\t中國發展\t90",
        9,
    )
    .unwrap();
    router.set_chinese_script(ChineseScript::Traditional);
    for c in "zhongguofazhan".chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let proof = router.ai_input().unwrap();
    assert_eq!(proof.source, "中国发站");
    let update = router.apply_ai_candidate(&proof, "中国发展").unwrap();
    let index = update.candidates.iter().position(|c| c.is_ai()).unwrap();
    assert_eq!(update.candidates[index].text, "中國發展");
    assert_eq!(
        update
            .candidates
            .iter()
            .filter(|c| c.text == "中國發展")
            .count(),
        1
    );
    assert_eq!(
        router.select_candidate(index).unwrap().commit.as_deref(),
        Some("中國發展")
    );
    assert!(router.session().pending_learning().is_empty());
}

#[test]
fn chinese_script_spelling_proof_before_conversion() {
    let mut router =
        EngineRouter::from_lexicon_tsv("xuan'zai'zai'na'li'shang'ban\t选在在哪里上班\t100", 9)
            .unwrap();
    router.set_chinese_script(ChineseScript::Traditional);
    for c in "xuanzaizainalishangban".chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let proof = router.ai_input().unwrap();
    let reading = "xian'zai'zai'na'li'shang'ban";
    assert!(router
        .apply_ai_spelling_candidate(&proof, "现在在哪里伤班", reading)
        .is_none());
    let update = router
        .apply_ai_spelling_candidate(&proof, "现在在哪里上班", reading)
        .unwrap();
    let index = update.candidates.iter().position(|c| c.is_ai()).unwrap();
    assert_eq!(update.candidates[index].text, "現在在哪裏上班");
    assert_eq!(
        router.select_candidate(index).unwrap().commit.as_deref(),
        Some("現在在哪裏上班")
    );
    assert!(router.session().pending_learning().is_empty());
    assert!(router.pending_promotions().is_empty());
}

#[test]
fn chinese_script_simplified_and_literal_boundaries() {
    let mut router = EngineRouter::from_lexicon_tsv(
        "zhong'guo\t中國\t100\nzhong'guo\t中国\t90\nxiao\t😀\t100",
        9,
    )
    .unwrap();
    for c in "zhongguo".chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    assert_eq!(
        router
            .session()
            .candidates_on_current_page()
            .iter()
            .filter(|c| c.text == "中国")
            .count(),
        1
    );
    assert_eq!(
        router.select_candidate(0).unwrap().commit.as_deref(),
        Some("中国")
    );
    assert_eq!(router.session().pending_learning()[0].text, "中國");
    for c in "xiao".chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    assert_eq!(
        router.select_candidate(0).unwrap().commit.as_deref(),
        Some("😀")
    );
    assert_eq!(
        router
            .process_key(KeyEvent::plain(Key::Char(',')))
            .unwrap()
            .commit
            .as_deref(),
        Some("，")
    );
}

fn confirmed_prefix(words: &str, raw: &str, displayed: &str) -> EngineRouter {
    let mut router = EngineRouter::from_lexicon_tsv(words, 9).unwrap();
    router.set_chinese_script(ChineseScript::Traditional);
    for c in raw.chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let index = router
        .session()
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == displayed)
        .unwrap();
    assert!(router.select_candidate(index).unwrap().commit.is_none());
    router
}

#[test]
fn chinese_script_confirmed_context_projects_suffix_and_deduplicates() {
    let mut router = confirmed_prefix("tou\t头\t100\nfa\t发\t100\nfa\t髮\t90", "toufa", "頭");
    assert_eq!(router.preedit(), "頭fa");
    let choices = router.session().candidates_on_current_page();
    assert_eq!(choices.iter().filter(|c| c.text == "髮").count(), 1);
    assert!(!choices.iter().any(|c| c.text == "發"));
    let index = choices.iter().position(|c| c.text == "髮").unwrap();
    assert_eq!(
        router.select_candidate(index).unwrap().commit.as_deref(),
        Some("頭髮")
    );
    let events = router.session().pending_learning();
    assert_eq!(
        events.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        ["头", "发"]
    );
}

#[test]
fn chinese_script_confirmed_context_can_rewrite_prefix() {
    // Pinned OpenCC first values: 干 -> 幹, but 干杯 -> 乾杯.
    let mut router = confirmed_prefix("gan\t干\t100\nbei\t杯\t100", "ganbei", "幹");
    assert_eq!(router.preedit(), "幹bei");
    let choices = router.session().candidates_on_current_page();
    let index = choices.iter().position(|c| c.text == "乾杯").unwrap();
    assert_eq!(
        router.select_candidate(index).unwrap().commit.as_deref(),
        Some("乾杯")
    );
    assert_eq!(router.session().pending_learning()[0].text, "干");
}

#[test]
fn chinese_script_ai_with_confirmed_prefix_is_not_prefixed_again() {
    let mut router = confirmed_prefix(
        "zhong'guo\t中国\t100\nfa'zhan\t发站\t100\nfa'zhan\t發展\t90",
        "zhongguofazhan",
        "中國",
    );
    let proof = router.ai_input().unwrap();
    assert_eq!(proof.source, "中国发站");
    let update = router.apply_ai_candidate(&proof, "中国发展").unwrap();
    let index = update.candidates.iter().position(|c| c.is_ai()).unwrap();
    assert_eq!(update.candidates[index].text, "中國發展");
    assert!(!update.candidates.iter().any(|c| c.text == "發展"));
    assert_eq!(
        router.select_candidate(index).unwrap().commit.as_deref(),
        Some("中國發展")
    );
    assert!(router.session().pending_learning().is_empty());
}
