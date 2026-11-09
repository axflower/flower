use flower::engine::{Key, KeyEvent, PinyinSession};

fn type_text(s: &mut PinyinSession, raw: &str) {
    for c in raw.chars() {
        s.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
}

const WORDS: &str = "jin'tian\t今天\t1000\nzao'shang\t早上\t900\nwo'men\t我们\t1000\ngong'zuo\t工作\t900\nxue'xi\t学习\t800";

#[test]
fn a_correct_word_survives_all_four_edit_types_in_the_other_word() {
    for (raw, gold) in [
        ("jintainzaoshang", "今天早上"),
        ("jintianzaosahng", "今天早上"),
        ("jintianzaoshagn", "今天早上"),
        ("jintianzaohang", "今天早上"),
        ("jintianzqoshang", "今天早上"),
        ("jintianzaoshangq", "今天早上"),
        ("jintianzaooshang", "今天早上"),
        ("gongzuoxuexo", "工作学习"),
        ("womengongzuoo", "我们工作"),
    ] {
        let mut s = PinyinSession::from_lexicon_tsv(WORDS, 9).unwrap();
        type_text(&mut s, raw);
        let mut committed = false;
        for page in 0..s.page_count() {
            if let Some(index) = s
                .candidates_on_current_page()
                .iter()
                .position(|c| c.text == gold && c.is_spelling())
            {
                assert_eq!(
                    s.select_candidate(index).unwrap().commit.as_deref(),
                    Some(gold),
                    "{raw}"
                );
                assert!(s.pending_input().is_empty());
                assert!(s.pending_learning().is_empty());
                assert!(s.pending_promotions().is_empty());
                assert!(s.pending_phrase_learning().is_empty());
                committed = true;
                break;
            }
            if page + 1 < s.page_count() {
                s.next_page().unwrap();
            }
        }
        assert!(committed, "{raw} -> {gold}");
    }
}

#[test]
fn correct_words_and_multiple_edits_do_not_invent_a_composed_repair() {
    let mut short = PinyinSession::from_lexicon_tsv(
        "ni\t你\t100\nhao\t好\t100\nni'he\t你和\t100\no\t哦\t100",
        9,
    )
    .unwrap();
    type_text(&mut short, "nihao");
    for page in 0..short.page_count() {
        assert!(short
            .candidates_on_current_page()
            .iter()
            .all(|c| c.text != "你和哦"));
        if page + 1 < short.page_count() {
            short.next_page().unwrap();
        }
    }
    for raw in [
        "jintianzaoshang",
        "womenxuexi",
        "jintainzaosahng",
        "jintian'zaoshang",
    ] {
        let mut s = PinyinSession::from_lexicon_tsv(WORDS, 9).unwrap();
        type_text(&mut s, raw);
        for page in 0..s.page_count() {
            assert!(
                s.candidates_on_current_page()
                    .iter()
                    .all(|c| !c.is_spelling()),
                "{raw}"
            );
            if page + 1 < s.page_count() {
                s.next_page().unwrap();
            }
        }
    }
}
