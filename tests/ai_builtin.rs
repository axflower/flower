//! Opt-in diagnostics using only the built-in lexicon and fixed offline model.
#[path = "../src/ai/inference.rs"]
#[allow(dead_code)]
mod inference;

use flower::engine::{EngineRouter, Key, KeyEvent};

#[test]
#[ignore = "strict positive; requires FLOWER_AI_TEST_BUNDLE; never downloads assets"]
fn builtin_question_corrects_and_commits_without_learning() {
    let bundle = std::env::var_os("FLOWER_AI_TEST_BUNDLE").expect("explicit fixed bundle");
    let mut model = inference::Corrector::load(std::path::Path::new(&bundle)).unwrap();
    const RAW: &str = "qingwenxianzainalishangban";
    const SOURCE: &str = "请问现在那里上班";
    // Human gold: the question asks where someone works, requiring 哪, not 那.
    const EXPECTED: &str = "请问现在哪里上班";
    let mut router = typed(RAW);
    let input = router.ai_input().expect("eligible real built-in candidate");
    assert_eq!(input.raw, RAW);
    assert_eq!(input.source, SOURCE);
    let original = router.session().candidates_on_current_page()[0].clone();
    let corrected = model.correct(&input.source).unwrap();
    assert_eq!(corrected.as_deref(), Some(EXPECTED));
    let update = router
        .apply_ai_candidate(&input, corrected.as_deref().unwrap())
        .expect("real correction must be shown");
    assert!(update.commit.is_none());
    assert_eq!(router.preedit(), RAW);
    assert_eq!(update.candidates[0], original);
    assert_eq!(update.candidates[1].text, EXPECTED);
    assert!(update.candidates[1].is_ai());
    assert_eq!(update.candidates[1].display_text(), format!("{EXPECTED} √"));
    assert!(router.pending_learning().is_empty());
    assert!(router.pending_promotions().is_empty());

    // Count every page, not just the visible second slot.
    let mut copies = update
        .candidates
        .iter()
        .filter(|c| c.text == EXPECTED)
        .count();
    for _ in 1..update.page_count {
        let page = router.next_page().unwrap();
        copies += page
            .candidates
            .iter()
            .filter(|c| c.text == EXPECTED)
            .count();
    }
    assert_eq!(copies, 1);
    for _ in 1..update.page_count {
        router.previous_page().unwrap();
    }
    let committed = router.select_candidate(1).unwrap();
    assert_eq!(committed.commit.as_deref(), Some(EXPECTED));
    assert!(!committed.commit.as_deref().unwrap().contains('√'));
    assert!(!router.is_composing());
    assert!(router.pending_learning().is_empty());
    assert!(router.pending_promotions().is_empty());
}

// Fixed before the first run: sixteen everyday phrases, each in full, initial,
// and mixed spelling. These are diagnostic inputs, not correctness assertions.
const CORPUS: &[(&str, &str, &str)] = &[
    ("shentijiankang", "stjk", "shentijk"),
    ("ganxienidebangzhu", "gxndbz", "ganxiendbz"),
    (
        "zhegewentihenrongyijiejue",
        "zg wethryjj",
        "zhegewentihenryjj",
    ),
    ("woxiwangdajiashentijiankang", "wxwdjstjk", "woxiwangdjstjk"),
    (
        "wofeichangganxienidebangzhu",
        "wfcgxndbz",
        "wofeichangganxiendbz",
    ),
    ("jintianxinqinghenhao", "jtxqhh", "jintianxqhenhao"),
    ("womenyidingyaonulixuexi", "wmydynlxx", "womenyidingyaonlxx"),
    (
        "shaoxianduiyuanyinggaiweilao renrangzuo",
        "sxdy ygwlrrz",
        "shaoxianduiyuanyinggaiweilr rz",
    ),
    ("qingnibaowenjianfageiwo", "qnbwjfgw", "qingnibaowenjianfgw"),
    ("womenmingtianzaijian", "w mmtzj", "womenmingtianzj"),
    (
        "zhejianshiqingfeichangzhongyao",
        "zjsqfcz y",
        "zhejianshiqingfczhongyao",
    ),
    (
        "wojintianquyiyuankanbing",
        "wj tqyykb",
        "wojintianquyykanbing",
    ),
    ("zhunishengrikuaile", "znsrkl", "zhunishengrikl"),
    (
        "qingwenxianzainalishangban",
        "qwxznlsb",
        "qingwenxianzainlshangban",
    ),
    ("woyijingzhidaole", "wyjzdl", "woyijingzdl"),
    (
        "zhegefangfafeichangyouyong",
        "zgfffcyy",
        "zhegefangfafcyouyong",
    ),
];

fn typed(raw: &str) -> EngineRouter {
    let mut router = EngineRouter::new().unwrap();
    for c in raw.chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    router
}

#[test]
#[ignore = "diagnostic only; requires FLOWER_AI_TEST_BUNDLE; never downloads assets"]
fn builtin_fixed_corpus_diagnostic() {
    let bundle = std::env::var_os("FLOWER_AI_TEST_BUNDLE").expect("explicit fixed bundle");
    let mut model = inference::Corrector::load(std::path::Path::new(&bundle)).unwrap();
    assert_eq!(CORPUS.len() * 3, 48);
    for &(full, initials, mixed) in CORPUS {
        for spelling in [full, initials, mixed] {
            let raw = spelling.replace(' ', "");
            let mut router = typed(&raw);
            match router.ai_input() {
                Some(input) => {
                    let result = model.correct(&input.source);
                    println!("{raw}\t{}\t{result:?}", input.source);
                    if let Ok(Some(corrected)) = result {
                        let candidates = router.session().candidates_on_current_page();
                        println!(
                            "corrected_on_first_page={:?}",
                            candidates.iter().position(|c| c.text == corrected)
                        );
                        println!(
                            "valid_correction={}",
                            flower::ai::valid_correction(&input.source, &corrected)
                        );
                        println!(
                            "inserted={}",
                            router.apply_ai_candidate(&input, &corrected).is_some()
                        );
                    }
                }
                None => println!("{raw}\t<ineligible>\t<not run>"),
            }
        }
    }
}
