#[path = "../src/ai/inference.rs"]
#[allow(dead_code)]
mod inference;

use flower::ai::{spelling_change_span, Request};

#[test]
fn one_edit_structural_fixtures() {
    let target = "现在在哪里上班";
    let reading = "xian'zai'zai'na'li'shang'ban";
    for raw in [
        "xuanzaizainalishangban",  // substitution
        "xainzaizainalishangban",  // transposition
        "xinzaizainalishangban",   // omission
        "xiaanzaizainalishangban", // extra letter
    ] {
        assert!(flower::engine::single_spelling_edit(raw, reading), "{raw}");
    }
    assert!(spelling_change_span(
        "xuanzaizainalishangban",
        "选在在哪里上班",
        "xuan'zai'zai'na'li'shang'ban",
        target,
        reading
    )
    .is_some());
    assert!(spelling_change_span(
        "xuanzaizainalishangban",
        "选在在哪里上班",
        "xuan'zai'zai'na'li'shang'ban",
        "现在在哪里伤班",
        reading
    )
    .is_none());
}

#[test]
#[ignore = "requires FLOWER_AI_TEST_BUNDLE fixed offline bundle; never downloads"]
fn real_model_spelling_and_correct_controls() {
    let bundle = std::env::var_os("FLOWER_AI_TEST_BUNDLE").expect("FLOWER_AI_TEST_BUNDLE");
    let mut corrector = inference::Corrector::load(std::path::Path::new(&bundle)).unwrap();
    for (raw, text, reading, expected) in [
        (
            "xuanzaizainalishangban",
            "选在在哪里上班",
            "xuan'zai'zai'na'li'shang'ban",
            Some("现在在哪里上班"),
        ),
        (
            "qingxuanzexuyaodexuanxiang",
            "请选择需要的选项",
            "qing'xuan'ze'xu'yao'de'xuan'xiang",
            None,
        ),
        (
            "womenxuanzezaizhelijianmian",
            "我们选择在这里见面",
            "wo'men'xuan'ze'zai'zhe'li'jian'mian",
            None,
        ),
        (
            "zhegexuanxiangyijingbaocun",
            "这个选项已经保存",
            "zhe'ge'xuan'xiang'yi'jing'bao'cun",
            None,
        ),
        (
            "tajintianzaizhelishangban",
            "他今天在这里上班",
            "ta'jin'tian'zai'zhe'li'shang'ban",
            None,
        ),
        (
            "tajintianzaizhelishangban",
            "她今天在这里上班",
            "ta'jin'tian'zai'zhe'li'shang'ban",
            None,
        ),
        (
            "womenxuanzainalijianmian",
            "我们选在哪里见面",
            "wo'men'xuan'zai'na'li'jian'mian",
            None,
        ),
        (
            "tamenxuanzaizhelishangban",
            "他们选在这里上班",
            "ta'men'xuan'zai'zhe'li'shang'ban",
            None,
        ),
        (
            "xianzaizainalishangban",
            "现在在哪里上班",
            "xian'zai'zai'na'li'shang'ban",
            None,
        ),
    ] {
        let request = Request {
            id: 1,
            text: text.into(),
            raw: raw.into(),
            reading: reading.into(),
        };
        let start = std::time::Instant::now();
        let result = corrector.correct_request(&request).unwrap();
        eprintln!("raw={raw} result={result:?} elapsed={:?}", start.elapsed());
        assert_eq!(result.corrected.as_deref(), expected, "{raw}");
        if expected.is_some() {
            assert!(!result.reading.is_empty());
        }
    }
    assert_eq!(corrector.correct("我们选在哪里见面").unwrap(), None);
    assert_eq!(corrector.correct("他们选在这里上班").unwrap(), None);
    assert_eq!(corrector.correct("少先队员因该为老人让坐").unwrap(), None);
    let legacy = corrector
        .correct_request(&Request {
            id: 3,
            text: "这个问题很容意解决".into(),
            raw: String::new(),
            reading: String::new(),
        })
        .unwrap();
    assert_eq!(legacy.corrected.as_deref(), Some("这个问题很容易解决"));
    assert!(legacy.reading.is_empty());
    assert!(corrector
        .correct_request(&Request {
            id: 4,
            text: "现在在哪里上班".into(),
            raw: "BAD".into(),
            reading: String::new(),
        })
        .is_err());
    // Fixed regression cases use the actual engine's first complete candidate.
    for (kind, raw) in [
        ("swap", "xainzaizainalishangban"),
        ("omission", "xinzaizainalishangban"),
        ("extra", "xiaanzaizainalishangban"),
    ] {
        use flower::engine::{Key, KeyEvent, PinyinSession};
        let mut session = PinyinSession::new().unwrap();
        for c in raw.chars() {
            session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
        }
        let input = session.ai_input().expect("complete source candidate");
        eprintln!("kind={kind} input={input:?}");
        let request = Request {
            id: 2,
            text: input.source,
            raw: input.raw,
            reading: input.reading,
        };
        let result = corrector.correct_request(&request).unwrap();
        eprintln!("kind={kind} result={result:?}");
        assert_eq!(
            result.corrected.as_deref(),
            Some("现在在哪里上班"),
            "{kind}"
        );
        assert_eq!(
            result.reading.is_empty(),
            kind == "omission",
            "{kind}: CSC priority"
        );
        if let Some(text) = &result.corrected {
            if result.reading.is_empty() {
                assert!(flower::ai::valid_correction(&request.text, text));
            } else {
                assert!(spelling_change_span(
                    &request.raw,
                    &request.text,
                    &request.reading,
                    text,
                    &result.reading
                )
                .is_some());
            }
        }
    }
}
