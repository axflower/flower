use flower::engine::{single_spelling_edit, Lexicon, SpellingIndex};
use std::time::Instant;

#[test]
fn exact_single_edit_contract() {
    assert!(single_spelling_edit("xuexo", "xue'xi"));
    assert!(!single_spelling_edit("xue", "xue'xi"));
    for raw in ["nihaoshijia", "nihaoshijiee", "nihaoshjie", "nihaoshijei"] {
        assert!(single_spelling_edit(raw, "ni'hao'shi'jie"), "{raw}");
    }
    for raw in [
        "nihaoshijie",
        "nihaoshijaa",
        "ni'hao'shi'jie",
        "Nihaoshijie",
        "nihao",
        "你好世界",
    ] {
        assert!(!single_spelling_edit(raw, "ni'hao'shi'jie"), "{raw}");
    }
}

#[test]
fn guided_builtin_user_examples_and_old_positives() {
    let index = SpellingIndex::new(&Lexicon::builtin().unwrap());
    for (raw, source, reading, gold, rank) in [
        (
            "xueerunishuixingzhou",
            "学鄂如逆水行舟",
            "xue'e'ru'ni'shui'xing'zhou",
            "学如逆水行舟",
            7,
        ),
        ("xuexo", "学校哦", "xue'xiao'o", "学习", 1),
        (
            "xuanzaizainalishangban",
            "选在在哪里上班",
            "xuan'zai'zai'na'li'shang'ban",
            "现在在哪里上班",
            1,
        ),
        (
            "xainzaizainalishangban",
            "喜爱内在在哪里上班",
            "xi'ai'nei'zai'zai'na'li'shang'ban",
            "现在在哪里上班",
            1,
        ),
        (
            "xianzaizainalishangbn",
            "现在在哪里上不能",
            "xian'zai'zai'na'li'shang'bu'neng",
            "现在在哪里上班",
            1,
        ),
        (
            "xianzaizainalishangbann",
            "现在在哪里上班嗯",
            "xian'zai'zai'na'li'shang'ban'n",
            "现在在哪里上班",
            1,
        ),
    ] {
        let found = index.candidates_with_source(raw, 8, source, reading);
        assert_eq!(
            found.iter().position(|c| c.text == gold).map(|i| i + 1),
            Some(rank),
            "{raw}: {found:?}"
        );
        assert!(found.len() <= 8);
        assert_eq!(found, index.candidates_with_source(raw, 8, source, reading));
        for candidate in found {
            assert!(flower::ai::spelling_change_span(
                raw,
                source,
                reading,
                &candidate.text,
                &candidate.reading
            )
            .is_some());
        }
    }
    assert!(index
        .candidates("xuexo", 8)
        .iter()
        .any(|c| c.text == "学习"));
}

#[test]
fn invalid_guides_and_unchanged_homophones_are_rejected() {
    let index = SpellingIndex::new(
        &Lexicon::from_tsv("ni'hao\t你好\t1000\nshi'jie\t世界\t1000\n").unwrap(),
    );
    for (source, reading) in [
        ("a好", "ni'hao"),
        ("你", "ni"),
        ("你好", "ni"),
        ("你好", "Ni'hao"),
        ("你好", "ni''hao"),
        ("你好", "n'hao"),
    ] {
        assert!(index
            .candidates_with_source("nihaoshijia", 8, source, reading)
            .is_empty());
    }
    // The unchanged ni syllable must not silently replace 泥 by 你.
    assert!(index
        .candidates_with_source("nihaoshijia", 8, "泥好世家", "ni'hao'shi'jia")
        .is_empty());
    let found = index.candidates_with_source("nihasohijie", 8, "你好是界", "ni'ha'so'hi'jie");
    assert!(found.is_empty()); // Invalid guide: five syllables for four characters.
    let found = index.candidates_with_source("nihasohijie", 8, "你好嗖界", "ni'hao'sou'jie");
    assert!(found.iter().any(|c| c.text == "你好世界"), "{found:?}");
}

#[test]
fn fixture_generation_limits_and_stability() {
    let lexicon = Lexicon::from_tsv("ni'hao\t你好\t1000\nshi'jie\t世界\t1000\n").unwrap();
    let index = SpellingIndex::new(&lexicon);
    for raw in ["nihaoshijia", "nihaoshijiee", "nihaoshjie", "nihaoshijei"] {
        let found = index.candidates(raw, 100);
        assert!(
            found.iter().any(|c| c.text == "你好世界"),
            "{raw}: {found:?}"
        );
        assert!(found.len() <= 8);
        assert_eq!(found, index.candidates(raw, 100));
        assert!(found
            .iter()
            .all(|c| c.edits == 1 && single_spelling_edit(raw, &c.reading)));
        assert!(index.candidates(raw, 0).is_empty());
        assert!(index.candidates(raw, 1).len() <= 1);
    }
    for raw in [
        "nihaoshijie",
        "ni'hao'shi'jie",
        "NIHAOSHIJIE",
        "nihaoshijaa",
    ] {
        assert!(index.candidates(raw, 8).is_empty(), "{raw}");
    }
    assert!(index.candidates(&"a".repeat(129), 8).is_empty());
}

#[test]
fn adjacent_swap_across_dictionary_edges() {
    // The swapped o/s straddles 你好 | 世界; neither entry alone is one edit away.
    let lexicon = Lexicon::from_tsv("ni'hao\t你好\t1000\nshi'jie\t世界\t1000\n").unwrap();
    let index = SpellingIndex::new(&lexicon);
    let raw = "nihasohijie";
    let found = index.candidates(raw, 8);
    assert!(found.iter().any(|c| c.text == "你好世界"), "{found:?}");
    assert!(found.iter().all(|c| single_spelling_edit(raw, &c.reading)));
}

#[test]
fn beam_is_eight_deterministic_unique_sentences() {
    let mut tsv = String::new();
    for text in ["你好", "泥好", "拟好", "逆好"] {
        tsv.push_str(&format!("ni'hao\t{text}\t1000\n"));
    }
    for text in ["世界", "时界", "事界", "市界"] {
        tsv.push_str(&format!("shi'jie\t{text}\t1000\n"));
    }
    let lexicon = Lexicon::from_tsv(&tsv).unwrap();
    let index = SpellingIndex::new(&lexicon);
    let found = index.candidates("nihaoshijia", usize::MAX);
    assert_eq!(found.len(), 8);
    let unique: std::collections::BTreeSet<_> = found.iter().map(|c| &c.text).collect();
    assert_eq!(unique.len(), 8);
    assert!(found.windows(2).all(|w| w[0].text < w[1].text));
    // Lexicon traversal is hash-seeded; construction and tie breaks must not be.
    let rebuilt = SpellingIndex::new(&Lexicon::from_tsv(&tsv).unwrap());
    assert_eq!(found, rebuilt.candidates("nihaoshijia", 8));
}

#[test]
fn vowelless_syllables_are_filtered_only_from_repair() {
    for syllable in ["n", "m", "ng", "hm"] {
        let reading = format!("ni'hao'shi'jie'{syllable}");
        let lexicon = Lexicon::from_tsv(&format!("{reading}\t你好世界嗯\t1000\n")).unwrap();
        assert_eq!(lexicon.lookup(&reading)[0].text, "你好世界嗯");
        let raw = format!("nihaoshijia{syllable}");
        assert!(SpellingIndex::new(&lexicon).candidates(&raw, 8).is_empty());
        assert!(!single_spelling_edit(&raw, &reading));
        assert_eq!(lexicon.lookup(&reading)[0].text, "你好世界嗯");
    }
}

#[test]
fn builtin_gold_recall_and_timing() {
    let lexicon = Lexicon::builtin().unwrap();
    let start = Instant::now();
    let index = SpellingIndex::new(&lexicon);
    eprintln!("cold index: {:?}", start.elapsed());
    let mut missing = Vec::new();
    for (raw, gold) in [
        ("xuanzaizainalishangban", "现在在哪里上班"),
        ("xainzaizainalishangban", "现在在哪里上班"),
        ("xianzaizainalishangbn", "现在在哪里上班"),
        ("xianzaizainalishangbann", "现在在哪里上班"),
        ("jintianxinqinghenhoa", "今天心情很好"),
    ] {
        let start = Instant::now();
        let found = index.candidates(raw, 8);
        let position = found.iter().position(|c| c.text == gold);
        eprintln!(
            "{raw}: gold={position:?}, elapsed={:?}, candidates={found:?}",
            start.elapsed()
        );
        if position.is_none() {
            missing.push(raw);
        }
    }
    assert!(missing.is_empty(), "missing gold: {missing:?}");
}
