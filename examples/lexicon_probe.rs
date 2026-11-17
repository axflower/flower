//! Fixed developer workload only; does not inspect hosts or user input.
use std::hint::black_box;
use std::time::Instant;

use flower::engine::{EngineRouter, Key, KeyEvent, Lexicon};

fn main() {
    let start = Instant::now();
    let lexicon = Lexicon::builtin().expect("embedded dictionary");
    println!("cold_load_us={}", start.elapsed().as_micros());
    println!("pinyin_keys={}", lexicon.len());
    let mut first = EngineRouter::from_lexicon(lexicon.clone(), 7).unwrap();
    let start = Instant::now();
    let update = first.process_key(KeyEvent::plain(Key::Char('n'))).unwrap();
    assert!(update.handled && !first.session().candidates_on_current_page().is_empty());
    println!("first_pinyin_key_us={}", start.elapsed().as_micros());
    let start = Instant::now();
    let personal = lexicon.with_overlay("ni\t拟\t4294967295").unwrap();
    println!("small_overlay_load_us={}", start.elapsed().as_micros());
    assert_eq!(personal.lookup("ni")[0].text, "拟");
    assert_ne!(lexicon.lookup("ni")[0].text, "拟");
    let fixed_keys = ["ni", "hao", "shi", "yi", "lv", "nv", "chong", "he", "xyz"];
    let mut samples = Vec::new();
    let mut max_candidates = 0;
    for _ in 0..1000 {
        for key in fixed_keys {
            let start = Instant::now();
            let candidates = black_box(lexicon.lookup(black_box(key)));
            samples.push(start.elapsed().as_nanos());
            max_candidates = max_candidates.max(candidates.len());
            black_box(candidates);
        }
    }
    samples.sort_unstable();
    println!("warm_lookup_p50_ns={}", samples[samples.len() / 2]);
    println!("warm_lookup_p95_ns={}", samples[samples.len() * 95 / 100]);
    println!("max_candidates_in_fixed_workload={max_candidates}");
    let router = EngineRouter::new().unwrap();
    let start = Instant::now();
    for _ in 0..10000 {
        black_box(router.clone());
    }
    println!(
        "empty_router_clone_mean_ns={}",
        start.elapsed().as_nanos() / 10000
    );
    let workloads = [
        "nihao".to_owned(),
        "woaizhongguo".to_owned(),
        "beijingruanjiankaifa".to_owned(),
        "shi".repeat(42),
    ];
    let mut key_samples = Vec::new();
    for _ in 0..5 {
        for input in &workloads {
            let mut router = EngineRouter::new().unwrap();
            for c in input.chars() {
                let start = Instant::now();
                black_box(router.process_key(KeyEvent::plain(Key::Char(c))).unwrap());
                key_samples.push(start.elapsed().as_nanos());
            }
        }
    }
    key_samples.sort_unstable();
    println!(
        "engine_key_p95_ns={}",
        key_samples[key_samples.len() * 95 / 100]
    );
    println!("engine_key_max_ns={}", key_samples.last().unwrap());
}
