use flower::engine::{Key, KeyEvent, Lexicon, PinyinSession};

const EVERYDAY: &str = flower_dict::EVERYDAY_TSV;

#[test]
fn bounded_daily_prior_keeps_greetings_without_overriding_high_frequency_words() {
    for (raw, frequent, daily) in [("yj", "研究", "邮件"), ("sj", "世纪", "睡觉")] {
        let mut session = session_for(raw);
        let mut words = Vec::new();
        for _ in 0..session.page_count() {
            words.extend(
                session
                    .candidates_on_current_page()
                    .iter()
                    .map(|candidate| candidate.text.clone()),
            );
            session.next_page().unwrap();
        }
        let position = |text| words.iter().position(|word| word == text).unwrap();
        assert!(position(frequent) < position(daily), "{raw}: {words:?}");
    }
    assert_eq!(
        session_for("nh").candidates_on_current_page()[0].text,
        "你好"
    );
    for (raw, expected) in [
        ("wenti", "问题"),
        ("shijie", "世界"),
        ("shijian", "时间"),
        ("gongzuo", "工作"),
        ("xianzai", "现在"),
        ("zhidao", "知道"),
        ("yijing", "已经"),
        ("jiankang", "健康"),
        ("zhongguo", "中国"),
        ("nihao", "你好"),
        ("shi", "是"),
        ("de", "的"),
        ("yi", "一"),
    ] {
        assert_eq!(
            session_for(raw).candidates_on_current_page()[0].text,
            expected,
            "{raw}"
        );
    }
}

fn phrase_rows() -> Vec<(&'static str, &'static str)> {
    EVERYDAY
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 3);
            assert_eq!(fields[0].split('\'').count(), fields[1].chars().count());
            assert_eq!(fields[2], "100");
            (fields[0], fields[1])
        })
        .collect()
}

fn session_for(raw: &str) -> PinyinSession {
    let mut session = PinyinSession::from_lexicon(Lexicon::builtin().unwrap(), 9).unwrap();
    for letter in raw.chars() {
        session
            .process_key(KeyEvent::plain(Key::Char(letter)))
            .unwrap();
    }
    session
}

fn select_phrase(mut session: PinyinSession, word: &str, reading: &str) {
    for _ in 0..session.page_count() {
        if let Some(index) = session
            .candidates_on_current_page()
            .iter()
            .position(|c| c.text == word && c.comment == reading)
        {
            let candidate = &session.candidates_on_current_page()[index];
            assert!(!candidate.is_ai() && !candidate.is_spelling(), "{word}");
            assert_eq!(
                session.select_candidate(index).unwrap().commit.as_deref(),
                Some(word)
            );
            return;
        }
        session.next_page().unwrap();
    }
    panic!("missing phrase {word} ({reading})");
}

#[test]
fn every_project_phrase_has_exact_and_initial_candidates_with_canonical_reading() {
    let lexicon = Lexicon::builtin().unwrap();
    let rows = phrase_rows();
    assert_eq!(rows.len(), 803);
    let mut pairs = std::collections::HashSet::new();
    for (reading, word) in rows {
        assert!(pairs.insert((reading, word)));
        let full = reading.replace('\'', "");
        let exact = lexicon.lookup(&full);
        assert_eq!(
            exact
                .iter()
                .filter(|c| c.text == word && c.comment == reading)
                .count(),
            1,
            "{reading}: {word}"
        );
        let initials: String = reading
            .split('\'')
            .map(|s| s.chars().next().unwrap())
            .collect();
        select_phrase(session_for(&initials), word, reading);
    }
}

#[test]
fn network_and_spoken_additions_commit_full_initial_and_mixed_as_ordinary_words() {
    for (reading, word) in [
        ("qing'xu'jia'zhi", "情绪价值"),
        ("da'zi", "搭子"),
        ("xian'yan'bao", "显眼包"),
        ("beng'bu'zhu", "绷不住"),
        ("jue'jue'zi", "绝绝子"),
        ("she'kong", "社恐"),
        ("she'niu", "社牛"),
        ("zui'ti", "嘴替"),
        ("beng'bu'zhu'le", "蚌埠住了"),
        ("huo'ren'gan", "活人感"),
        ("yi'jian'san'lian", "一键三连"),
        ("dan'mu", "弹幕"),
        ("ping'lun'qu", "评论区"),
        ("you'xi'da'zi", "游戏搭子"),
        ("qin'ce'you'xiao", "亲测有效"),
        ("wo'bu'tai'ming'bai", "我不太明白"),
        ("neng'ju'ge'li'zi'ma", "能举个例子吗"),
        ("bie'gei'zi'ji'ya'li", "别给自己压力"),
        ("bu'yao'jia'bing", "不要加冰"),
        ("neng'pian'yi'dian'ma", "能便宜点吗"),
        ("bie'la'dong'xi", "别落东西"),
    ] {
        let syllables: Vec<_> = reading.split('\'').collect();
        let full = syllables.join("");
        let initials: String = syllables
            .iter()
            .map(|s| s.chars().next().unwrap())
            .collect();
        let mixed = syllables[0].to_owned()
            + &syllables[1..]
                .iter()
                .map(|s| s.chars().next().unwrap())
                .collect::<String>();
        for raw in [&full, &initials, &mixed] {
            select_phrase(session_for(raw), word, reading);
        }
    }
}

#[test]
fn representative_project_phrases_commit_full_and_initial_with_real_pagination() {
    for (reading, word) in [
        ("wo'ai'ni", "我爱你"),
        ("bu'zhi'dao", "不知道"),
        ("wan'shang'hao", "晚上好"),
        ("qing'zai'shuo'yi'bian", "请再说一遍"),
        ("ke'yi'shua'ka'ma", "可以刷卡吗"),
    ] {
        select_phrase(session_for(&reading.replace('\'', "")), word, reading);
        let initials: String = reading
            .split('\'')
            .map(|s| s.chars().next().unwrap())
            .collect();
        select_phrase(session_for(&initials), word, reading);
    }
    let session = session_for("wan");
    assert!(session
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "晚"));
    assert!(session
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "万"));
    assert!(!session
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "我爱你"));
    select_phrase(session, "我爱你", "wo'ai'ni");
}

#[test]
fn custom_fixture_and_overlay_do_not_pollute_builtin_project_phrases() {
    let fixture = Lexicon::from_tsv("wo'ai'ni\t私词\t1000\n").unwrap();
    assert!(!fixture.lookup("woaini").iter().any(|c| c.text == "我爱你"));
    let original = Lexicon::builtin().unwrap();
    let snapshot = original.with_overlay("wo'ai'ni\t私词\t1000\n").unwrap();
    assert!(snapshot.lookup("woaini").iter().any(|c| c.text == "私词"));
    assert!(!original.lookup("woaini").iter().any(|c| c.text == "私词"));
    assert_eq!(snapshot.lookup("woaini")[0].text, "我爱你");
    assert!(Lexicon::builtin()
        .unwrap()
        .lookup("woaini")
        .iter()
        .any(|c| c.text == "我爱你"));
}

#[test]
fn builtin_mixed_sentence_is_on_the_first_page_without_a_sentence_entry() {
    let lexicon = Lexicon::builtin().unwrap();
    assert!(!lexicon
        .lookup("shenmeyisi")
        .iter()
        .any(|c| c.text == "什么意思"));
    let mut session = PinyinSession::from_lexicon(lexicon, 9).unwrap();
    for c in "shenmeys".chars() {
        session.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
    }
    assert!(session
        .candidates_on_current_page()
        .iter()
        .any(|c| c.text == "什么意思" && c.comment == "shen'me'yi'si"));
}

#[test]
fn frequent_primary_readings_precede_rare_and_secondary_readings() {
    let lexicon = Lexicon::builtin().unwrap();
    for (pinyin, expected) in [
        ("de", "的"),
        ("yi", "一"),
        ("shi", "是"),
        ("wo", "我"),
        ("ni", "你"),
    ] {
        assert_eq!(lexicon.lookup(pinyin)[0].text, expected, "{pinyin}");
    }
    let yi = lexicon.lookup("yi");
    assert!(yi.iter().any(|candidate| candidate.text == "也"));
    assert_ne!(yi[0].text, "也", "text frequency is not reading frequency");
    let de = lexicon.lookup("de");
    let common = de.iter().position(|c| c.text == "的").unwrap();
    for rare in ["嘚", "锝"] {
        assert!(de.iter().position(|c| c.text == rare).unwrap() > common);
    }
    for (pinyin, expected) in [
        ("shijie", "世界"),
        ("beijing", "北京"),
        ("xuexi", "学习"),
        ("yinyue", "音乐"),
        ("nihao", "你好"),
        ("women", "我们"),
        ("shenme", "什么"),
        ("keyi", "可以"),
    ] {
        assert_eq!(lexicon.lookup(pinyin)[0].text, expected, "{pinyin}");
    }
}

#[test]
fn common_words_are_reachable_with_initials_in_real_session() {
    for (raw, expected, canonical) in [
        ("nh", "你好", "ni'hao"),
        ("wm", "我们", "wo'men"),
        ("sm", "什么", "shen'me"),
        ("ky", "可以", "ke'yi"),
        ("zg", "中国", "zhong'guo"),
        ("xx", "学习", "xue'xi"),
    ] {
        let mut session = PinyinSession::from_lexicon(Lexicon::builtin().unwrap(), 9).unwrap();
        for letter in raw.chars() {
            session
                .process_key(KeyEvent::plain(Key::Char(letter)))
                .unwrap();
        }
        assert!(
            session
                .candidates_on_current_page()
                .iter()
                .any(|candidate| candidate.text == expected && candidate.comment == canonical),
            "{raw}: {expected} must be on the first nine-candidate page"
        );
        let mut selected = None;
        for _ in 0..session.page_count() {
            if let Some(index) = session
                .candidates_on_current_page()
                .iter()
                .position(|candidate| candidate.text == expected)
            {
                assert_eq!(
                    session.candidates_on_current_page()[index].comment,
                    canonical
                );
                selected = Some(session.select_candidate(index).unwrap());
                break;
            }
            session.next_page().unwrap();
        }
        let result = selected.unwrap_or_else(|| panic!("{raw} missing {expected}"));
        assert_eq!(result.commit.as_deref(), Some(expected));
    }
}
