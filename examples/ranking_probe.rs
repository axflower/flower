//! Fixed public examples; never reads the user's dictionary or host text.
use flower::engine::{Key, KeyEvent, Lexicon, PinyinSession};
use flower::user_dictionary::UserDictionary;
use std::sync::Arc;

fn session(raw: &str, data: &UserDictionary) -> PinyinSession {
    let mut session = PinyinSession::from_lexicon(Lexicon::builtin().unwrap(), 9).unwrap();
    session.set_ranking_snapshot(Arc::new(data.ranking_snapshot()));
    for c in raw.chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    session
}

fn main() {
    let data = UserDictionary::default();
    for raw in [
        "huiyi", "shishi", "yiyi", "zhidao", "yijing", "shijian", "wenti", "gongzuo", "xianzai",
        "keyi", "xiexie", "zaijian", "mingtian", "jintian", "pengyou", "xihuan", "dianhua",
        "xiaoxi", "shuru", "shebei", "shijie", "yi", "de", "shi", "nh", "yj", "sj", "shenmeys",
        "zheshism",
    ] {
        let s = session(raw, &data);
        let words: Vec<_> = s
            .candidates_on_current_page()
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        println!("{raw}\t{}", words.join("|"));
    }
    let mut s = session("huiyi", &data);
    let selected = s
        .candidates_on_current_page()
        .iter()
        .position(|c| c.text == "回忆")
        .unwrap();
    s.process_key(KeyEvent::plain(Key::Char(char::from(
        b'1' + selected as u8,
    ))))
    .unwrap();
    println!(
        "commit_words={:?} promotions={:?}",
        s.pending_learning(),
        s.pending_promotions()
    );
    let mut learned = data;
    learned
        .remember_commit(
            s.pending_promotions(),
            s.pending_phrase_learning(),
            s.pending_learning(),
            "",
        )
        .unwrap();
    let next = session("huiyi", &learned);
    println!("after_huiyi={:?}", next.candidates_on_current_page());
}
