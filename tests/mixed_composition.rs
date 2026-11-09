use flower::engine::{Key, KeyEvent, Lexicon, PinyinSession};

fn type_text(session: &mut PinyinSession, raw: &str) {
    for c in raw.chars() {
        assert!(
            session
                .process_key(KeyEvent::plain(Key::Char(c)))
                .unwrap()
                .handled
        );
    }
    assert_eq!(session.pending_input(), raw);
}

fn locate(session: &mut PinyinSession, text: &str, reading: &str) -> (usize, usize) {
    for _ in 0..session.page_count() {
        if let Some(index) = session
            .candidates_on_current_page()
            .iter()
            .position(|c| c.text == text)
        {
            assert_eq!(session.candidates_on_current_page()[index].comment, reading);
            return (session.page() * session.page_size() + index + 1, index);
        }
        session.next_page().unwrap();
    }
    panic!("missing {text}, raw={}", session.pending_input());
}

#[test]
fn builtin_cross_word_pairs_commit_full_and_mixed_with_canonical_learning() {
    let lexicon = Lexicon::builtin().unwrap();
    for (full, mixed, text, reading) in [
        ("jintianxuexi", "jintianxx", "今天学习", "jin'tian'xue'xi"),
        (
            "mingtiangongzuo",
            "mingtiangz",
            "明天工作",
            "ming'tian'gong'zuo",
        ),
        ("womenshuru", "womensr", "我们输入", "wo'men'shu'ru"),
        ("dakaishouji", "dakaisj", "打开手机", "da'kai'shou'ji"),
        ("jintianxiawu", "jintianxw", "今天下午", "jin'tian'xia'wu"),
        (
            "mingtianshangban",
            "mingtiansb",
            "明天上班",
            "ming'tian'shang'ban",
        ),
        ("womenxuexi", "womenxx", "我们学习", "wo'men'xue'xi"),
        ("dakaidiannao", "dakaidn", "打开电脑", "da'kai'dian'nao"),
        (
            "shurufashezhi",
            "shurufasz",
            "输入法设置",
            "shu'ru'fa'she'zhi",
        ),
        ("weixinxiaoxi", "weixinxx", "微信消息", "wei'xin'xiao'xi"),
    ] {
        for raw in [full, mixed] {
            let mut session = PinyinSession::from_lexicon(lexicon.clone(), 9).unwrap();
            type_text(&mut session, raw);
            let (_, index) = locate(&mut session, text, reading);
            assert_eq!(
                session.select_candidate(index).unwrap().commit.as_deref(),
                Some(text),
                "{raw}"
            );
            assert!(session.pending_input().is_empty(), "{raw}");
            assert_eq!(
                session
                    .pending_learning()
                    .iter()
                    .map(|e| e.pinyin.as_str())
                    .collect::<Vec<_>>()
                    .join("'"),
                reading
            );
            assert_eq!(
                session
                    .pending_learning()
                    .iter()
                    .map(|e| e.text.as_str())
                    .collect::<String>(),
                text
            );
        }
    }
}

#[test]
fn core_mixed_ranks_are_measured_not_assumed_first() {
    let lexicon = Lexicon::builtin().unwrap();
    for (raw, text, reading, rank) in [
        ("zheshism", "这是什么", "zhe'shi'shen'me", 5),
        ("zhesm", "这什么", "zhe'shen'me", 1),
        ("shenmeys", "什么意思", "shen'me'yi'si", 2),
        ("womensr", "我们输入", "wo'men'shu'ru", 3),
        ("weixinxx", "微信消息", "wei'xin'xiao'xi", 4),
    ] {
        let mut session = PinyinSession::from_lexicon(lexicon.clone(), 9).unwrap();
        type_text(&mut session, raw);
        let (actual, index) = locate(&mut session, text, reading);
        // Optional spelling suggestions were added after these ordinary-rank
        // measurements. Measure the ordinary lane without treating a suggestion
        // inserted ahead of it as a change to the underlying mixed decoder.
        let target_page = session.page();
        while session.page() > 0 {
            session.previous_page().unwrap();
        }
        let mut suggestions = 0;
        for page in 0..=target_page {
            let end = if page == target_page {
                index
            } else {
                session.candidates_on_current_page().len()
            };
            suggestions += session.candidates_on_current_page()[..end]
                .iter()
                .filter(|c| c.is_spelling())
                .count();
            if page < target_page {
                session.next_page().unwrap();
            }
        }
        assert_eq!(actual - suggestions, rank, "{raw}");
        assert_eq!(
            session.select_candidate(index).unwrap().commit.as_deref(),
            Some(text)
        );
        assert!(session.pending_input().is_empty());
    }
}

#[test]
fn mixed_partial_undo_restores_raw_not_canonical_reading_or_text_length() {
    let mut session =
        PinyinSession::from_lexicon_tsv("wo'men\t我们\t2000\nshu'ru\t输入\t2000", 9).unwrap();
    type_text(&mut session, "womensr");
    let (_, index) = locate(&mut session, "我们", "wo'men");
    assert!(session.select_candidate(index).unwrap().commit.is_none());
    assert_eq!(session.pending_input(), "sr");
    assert!(session.pending_learning().is_empty());
    for _ in 0..3 {
        session
            .process_key(KeyEvent::plain(Key::Backspace))
            .unwrap();
    }
    assert_eq!(session.pending_input(), "women");
    assert_eq!(session.preedit(), "women");
    assert!(session.pending_learning().is_empty());
    for c in "sr".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    assert_eq!(session.pending_input(), "womensr");
    let (_, index) = locate(&mut session, "我们输入", "wo'men'shu'ru");
    assert_eq!(
        session.select_candidate(index).unwrap().commit.as_deref(),
        Some("我们输入")
    );
    assert_eq!(session.pending_promotions()[0].order.input, "womensr");
    let selected = &session.pending_promotions()[0].selected;
    assert_eq!(selected.consumed, 7);
    assert_eq!(selected.pinyin, "wo'men'shu'ru");
    assert_eq!(selected.text, "我们输入");
}

#[test]
fn small_word_graphs_work_without_builtin_priors_or_sentence_entries() {
    for (left, a, right, b, mixed) in [
        ("jin'tian", "今天", "xue'xi", "学习", "jintianxx"),
        ("ming'tian", "明天", "gong'zuo", "工作", "mingtiangz"),
        ("wo'men", "我们", "shu'ru", "输入", "womensr"),
        ("da'kai", "打开", "shou'ji", "手机", "dakaisj"),
        ("jin'tian", "今天", "xia'wu", "下午", "jintianxw"),
        ("ming'tian", "明天", "shang'ban", "上班", "mingtiansb"),
        ("wo'men", "我们", "xue'xi", "学习", "womenxx"),
        ("da'kai", "打开", "dian'nao", "电脑", "dakaidn"),
        ("shu'ru'fa", "输入法", "she'zhi", "设置", "shurufasz"),
        ("wei'xin", "微信", "xiao'xi", "消息", "weixinxx"),
        ("shen'me", "什么", "yi'si", "意思", "shenmeys"),
        ("ni'hao", "你好", "peng'you", "朋友", "nihpy"),
    ] {
        let tsv = format!("{left}\t{a}\t2000\n{right}\t{b}\t2000");
        let reading = format!("{left}'{right}");
        let text = format!("{a}{b}");
        let full = reading.replace('\'', "");
        for raw in [full.as_str(), mixed] {
            let mut session = PinyinSession::from_lexicon_tsv(&tsv, 9).unwrap();
            type_text(&mut session, raw);
            let (_, index) = locate(&mut session, &text, &reading);
            assert_eq!(
                session.select_candidate(index).unwrap().commit.as_deref(),
                Some(text.as_str())
            );
            assert!(session.pending_input().is_empty());
            assert_eq!(session.pending_learning().len(), 2);
        }
    }
}
