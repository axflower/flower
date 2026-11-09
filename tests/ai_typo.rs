//! Explicit diagnostic inputs for pinyin typing mistakes; never accesses user data.
#[path = "../src/ai/inference.rs"]
#[allow(dead_code)]
mod inference;

use flower::engine::{EngineRouter, Key, KeyEvent};

fn typed(raw: &str) -> EngineRouter {
    let mut router = EngineRouter::new().unwrap();
    for c in raw.chars() {
        router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    router
}

#[test]
fn spelling_suggestions_keep_original_first_and_commit_without_learning() {
    const READING: &str = "xian'zai'zai'na'li'shang'ban";
    for (raw, target) in [
        // This expanded builtin guide uses 那; the spelling repair must keep it.
        ("xuanzaizainalishangban", "现在在那里上班"),
        ("xainzaizainalishangban", "现在在哪里上班"),
        ("xianzaizainalishangbn", "现在在哪里上班"),
        ("xianzaizainalishangbann", "现在在那里上班"),
    ] {
        let mut router = typed(raw);
        let input = router.ai_input().unwrap();
        assert_eq!(
            input.reading.split('\'').count(),
            input.source.chars().count()
        );
        let first = router.session().candidates_on_current_page()[0].clone();
        let update = router
            .apply_ai_spelling_candidate(&input, target, READING)
            .unwrap_or_else(|| {
                panic!(
                    "no spelling candidate for {raw}: {} / {}",
                    input.source, input.reading
                )
            });
        assert_eq!(update.candidates[0], first);
        assert_eq!(update.candidates[1].text, target);
        assert!(update.candidates[1].is_ai());
        let commit = router.process_key(KeyEvent::plain(Key::Char('2'))).unwrap();
        assert_eq!(commit.commit.as_deref(), Some(target));
        assert!(router.pending_learning().is_empty());
        assert!(router.pending_promotions().is_empty());
    }
}

#[test]
fn spelling_response_rejects_unrelated_changes_multiple_edits_and_stale_input() {
    use flower::ai::{eligible_request, valid_response, Request, Response};
    let mut router = typed("xuanzaizainalishangban");
    let input = router.ai_input().unwrap();
    let request = Request {
        id: 1,
        text: input.source.clone(),
        raw: input.raw.clone(),
        reading: input.reading.clone(),
    };
    assert!(eligible_request(&request));
    let response = Response {
        id: 1,
        corrected: Some("现在在那里上班".into()),
        reading: "xian'zai'zai'na'li'shang'ban".into(),
    };
    assert!(valid_response(&request, &response));
    let mut wrong = response.clone();
    wrong.id = 2;
    assert!(!valid_response(&request, &wrong));
    wrong = response.clone();
    wrong.corrected = Some("现在在哪里上班".into());
    assert!(
        !valid_response(&request, &wrong),
        "unchanged syllables must keep their original characters"
    );
    let mut twice = request.clone();
    twice.raw.push('n');
    assert!(!valid_response(&twice, &response));
    let mut invalid = request.clone();
    invalid.reading = "xuan".into();
    assert!(!eligible_request(&invalid));
    router.process_key(KeyEvent::plain(Key::Char('a'))).unwrap();
    assert!(router
        .apply_ai_spelling_candidate(&input, "现在在那里上班", &response.reading)
        .is_none());
}

#[test]
fn confirmed_segment_reading_is_retained_but_ai_drops_its_learning_events() {
    use flower::engine::{Lexicon, PinyinSession};
    let lexicon =
        Lexicon::from_tsv("xuan\t选\t100\nzai'zai'na'li'shang'ban\t在在哪里上班\t100").unwrap();
    let mut session = PinyinSession::from_lexicon(lexicon, 9).unwrap();
    for c in "xuanzaizainalishangban".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    let index = session
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "选")
        .unwrap();
    assert!(session.select_candidate(index).unwrap().commit.is_none());
    let input = session.ai_input().unwrap();
    assert_eq!(input.source, "选在在哪里上班");
    assert_eq!(input.reading, "xuan'zai'zai'na'li'shang'ban");
    session
        .apply_ai_spelling_candidate(&input, "现在在哪里上班", "xian'zai'zai'na'li'shang'ban")
        .unwrap();
    assert_eq!(
        session.select_candidate(1).unwrap().commit.as_deref(),
        Some("现在在哪里上班")
    );
    assert!(session.pending_learning().is_empty());
    assert!(session.pending_promotions().is_empty());
}

#[test]
#[ignore = "requires FLOWER_AI_TEST_BUNDLE; never downloads or installs"]
fn pinyin_typo_baseline() {
    let bundle = std::env::var_os("FLOWER_AI_TEST_BUNDLE").expect("explicit bundle");
    let mut model = inference::Corrector::load(std::path::Path::new(&bundle)).unwrap();
    for (kind, raw) in [
        ("correct", "xianzaizainalishangban"),
        ("substitution", "xuanzaizainalishangban"),
        ("transposition", "xainzaizainalishangban"),
        ("missing", "xianzaizainalishangbn"),
        ("extra", "xianzaizainalishangbann"),
        ("substitution", "jintianxinqinghenhoa"),
        ("initials", "xzznlsb"),
        ("mixed", "xianzaizainlshangban"),
    ] {
        let mut router = EngineRouter::new().unwrap();
        for c in raw.chars() {
            router.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
        }
        let first = router
            .session()
            .candidates_on_current_page()
            .iter()
            .take(3)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        println!("case={kind} raw={raw} first={first:?}");
        if let Some(input) = router.ai_input() {
            let request = flower::ai::Request {
                id: 1,
                text: input.source.clone(),
                raw: input.raw.clone(),
                reading: input.reading.clone(),
            };
            let result = model.correct_request(&request).unwrap();
            println!(
                "source={} reading={} result={result:?}",
                input.source, input.reading
            );
            if raw == "xuanzaizainalishangban" {
                assert_eq!(result.corrected.as_deref(), Some("现在在哪里上班"));
            }
        } else {
            println!("ai_input=None");
        }
    }
}
