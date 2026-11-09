use flower::engine::{Key, KeyEvent, Lexicon, PinyinSession};
use std::collections::HashSet;

const TABLES: [&str; 13] = [
    "IT",
    "animal",
    "caijing",
    "car",
    "chengyu",
    "diming",
    "food",
    "law",
    "lishimingren",
    "medical",
    "poem",
    "cedict_supplement",
    "erhua",
];

#[test]
fn every_generated_category_pair_is_reachable_in_default_exact_lookup() {
    let manifest: serde_json::Value = serde_json::from_slice(
        flower_dictionary::asset("data/pinyin/categories/manifest.json").unwrap(),
    )
    .unwrap();
    let lexicon = Lexicon::builtin().unwrap();
    let mut unique = HashSet::new();
    for name in TABLES {
        let table = flower_dictionary::table(name).unwrap().tsv;
        for line in table
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let mut fields = line.split('\t');
            let reading = fields.next().unwrap();
            let text = fields.next().unwrap();
            assert_eq!(reading.split('\'').count(), text.chars().count());
            if !unique.insert((reading.to_owned(), text.to_owned())) {
                continue;
            }
            let candidates = lexicon.lookup(&reading.replace('\'', ""));
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.text == text && candidate.comment == reading),
                "default category {name}: {reading} -> {text}"
            );
        }
    }
    assert_eq!(
        unique.len() as u64,
        manifest["global_unique_pairs"].as_u64().unwrap()
    );
    assert!(manifest["additional_to_default"].as_u64().unwrap() >= 80_000);
}

fn choose_and_commit(raw: &str, text: &str) {
    let mut session = PinyinSession::new().unwrap();
    for character in raw.chars() {
        session
            .process_key(KeyEvent::plain(Key::Char(character)))
            .unwrap();
    }
    for _ in 0..session.page_count() {
        if let Some(index) = session
            .candidates_on_current_page()
            .iter()
            .position(|c| c.text == text)
        {
            let update = session.select_candidate(index).unwrap();
            assert_eq!(update.commit.as_deref(), Some(text), "{raw}");
            return;
        }
        session.next_page().unwrap();
    }
    panic!("default full candidate not reachable: {raw} -> {text}");
}

#[test]
fn brands_full_initial_and_mixed_inputs_commit_complete_words() {
    for (raw, text) in [
        ("falali", "法拉利"),
        ("fll", "法拉利"),
        ("falal", "法拉利"),
        ("maidanglao", "麦当劳"),
        ("mdl", "麦当劳"),
        ("maidangl", "麦当劳"),
        ("weixin", "微信"),
        ("wx", "微信"),
        ("weix", "微信"),
    ] {
        choose_and_commit(raw, text);
    }
}

#[test]
fn attested_erhua_inputs_commit_without_losing_final_character() {
    for (raw, text) in [
        ("yidianr", "一点儿"),
        ("wanr", "玩儿"),
        ("nar", "哪儿"),
        ("zher", "这儿"),
        ("huar", "花儿"),
    ] {
        choose_and_commit(raw, text);
    }
}
