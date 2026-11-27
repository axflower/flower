#[allow(dead_code)]
#[path = "build_support/lexicon_index.rs"]
mod lexicon_index;
#[allow(dead_code)]
#[path = "build_support/spelling_seed.rs"]
mod spelling_seed;
#[path = "build_support/word_list_links.rs"]
mod word_list_links;

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    use sha2::{Digest, Sha256};
    let spoken_table = "data/spoken_ranking/word-diversity.bin";
    println!("cargo:rerun-if-changed={spoken_table}");
    let spoken_table = std::fs::read(spoken_table).expect("read SUBTLEX-CH ranking table");
    assert_eq!(
        format!("{:x}", Sha256::digest(&spoken_table)),
        "a8f63e844690609baad348f6503758e4449944d883c19e68e6d5ca5574676dc7",
        "SUBTLEX-CH ranking table changed"
    );
    let mut entries =
        lexicon_index::parse(flower_dict::DEFAULT_TSV).expect("parse builtin dictionary");
    lexicon_index::classify(
        &mut entries,
        flower_dict::COMMON_USAGE_PAIRS,
        flower_dict::EVERYDAY_TSV,
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // Weak lexical adjacency evidence only; ignore weights and never label this
    // list as corpus frequency. Select by distinct-word support, not Unicode
    // position; duplicate polyphonic rows do not inflate lexical evidence.
    let links = word_list_links::select(flower_dict::DEFAULT_TSV, 65536);
    std::fs::write(output.join("word-list-links.rs"), format!("&{links:?}"))
        .expect("write bounded word-list adjacency prior");
    {
        let mut keys: Vec<_> = entries.keys().collect();
        keys.sort();
        let seed = spelling_seed::encode(keys.iter().flat_map(|key| {
            entries[*key]
                .iter()
                .map(|entry| (entry.reading.as_str(), entry.text.as_str(), entry.weight))
        }));
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&seed, 9);
        std::fs::write(output.join("builtin-spelling.tsv"), &seed)
            .expect("write builtin raw spelling seed");
        std::fs::write(output.join("builtin-spelling.zlib"), &compressed)
            .expect("write builtin spelling seed");
        let digest: [u8; 32] = Sha256::digest(&seed).into();
        std::fs::write(
            output.join("builtin-spelling.rs"),
            format!(
                "const BUILTIN_SPELLING_SIZE: usize = {};\nconst BUILTIN_SPELLING_SHA256: [u8; 32] = {:?};\n",
                seed.len(), digest
            ),
        )
        .expect("write builtin spelling seed identity");
        println!(
            "cargo:warning=builtin spelling seed: {} rows, {} bytes, {} compressed bytes, sha256 {:x}",
            seed.iter().filter(|&&byte| byte == b'\n').count(),
            seed.len(),
            compressed.len(),
            Sha256::digest(&seed)
        );
    }
    let bytes = lexicon_index::encode(&entries);
    std::fs::write(output.join("builtin-dictionary.idx"), &bytes).expect("write builtin index");
    println!(
        "cargo:warning=builtin index: {} keys, {} entries, {} bytes, sha256 {:x}",
        entries.len(),
        entries.values().map(Vec::len).sum::<usize>(),
        bytes.len(),
        Sha256::digest(&bytes)
    );
    println!(
        "cargo:warning=builtin TSV source sha256 {:x}; all exact/initial buckets and trie nodes round-trip verified",
        Sha256::digest(flower_dict::DEFAULT_TSV.as_bytes())
    );
    println!("cargo:rerun-if-changed=build_support/lexicon_index.rs");
    println!("cargo:rerun-if-changed=build_support/spelling_seed.rs");
    println!("cargo:rerun-if-changed=build_support/word_list_links.rs");
    for file in [
        "assets/flower.rc",
        "assets/flower.ico",
        "assets/flower-settings.rc",
        "assets/flower-settings.manifest",
    ] {
        println!("cargo:rerun-if-changed={file}");
    }
    for name in [
        "RC",
        "WindowsSdkVerBinPath",
        "WindowsSdkDir",
        "ProgramFiles(x86)",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        Ok("msvc"),
        "Flower Windows resources currently require an MSVC target"
    );
    let compiler = find_resource_compiler().expect(
        "Windows SDK rc.exe was not found; install a Windows SDK or set RC to its absolute path",
    );
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("flower.res");
    let status = Command::new(&compiler)
        .current_dir(&root)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(root.join("assets/flower.rc"))
        .status()
        .unwrap_or_else(|error| panic!("could not execute {}: {error}", compiler.display()));
    assert!(status.success(), "Windows icon resource compilation failed");
    println!("cargo:rustc-link-arg-cdylib={}", resource.display());
    println!("cargo:rustc-link-arg-bins={}", resource.display());
    // Visual styles and DPI awareness belong only to the standalone settings
    // executable. Never attach this application manifest to the TSF host DLL.
    let manifest_resource =
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("flower-settings.res");
    let status = Command::new(&compiler)
        .current_dir(&root)
        .arg("/nologo")
        .arg("/fo")
        .arg(&manifest_resource)
        .arg(root.join("assets/flower-settings.rc"))
        .status()
        .unwrap_or_else(|error| panic!("could not execute {}: {error}", compiler.display()));
    assert!(
        status.success(),
        "Settings manifest resource compilation failed"
    );
    println!(
        "cargo:rustc-link-arg-bin=flower-settings={}",
        manifest_resource.display()
    );
}

fn find_resource_compiler() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("RC") {
        // An explicit override must not silently fall back to another SDK.
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    for directory in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let path = directory.join("rc.exe");
        if path.is_file() {
            return Some(path);
        }
    }
    let host_arch = if env::var("HOST").ok()?.starts_with("x86_64") {
        "x64"
    } else {
        "x86"
    };
    if let Some(bin) = env::var_os("WindowsSdkVerBinPath") {
        let path = PathBuf::from(bin).join(host_arch).join("rc.exe");
        if path.is_file() {
            return Some(path);
        }
    }
    let sdk = env::var_os("WindowsSdkDir")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("ProgramFiles(x86)")
                .map(|programs| PathBuf::from(programs).join("Windows Kits/10"))
        })?;
    newest_sdk_compiler(&sdk.join("bin"), host_arch)
}

fn newest_sdk_compiler(bin: &Path, host_arch: &str) -> Option<PathBuf> {
    let mut candidates: Vec<_> = std::fs::read_dir(bin)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let version: Vec<u32> = entry
                .file_name()
                .to_str()?
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            let compiler = entry.path().join(host_arch).join("rc.exe");
            compiler.is_file().then_some((version, compiler))
        })
        .collect();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates.pop().map(|(_, path)| path)
}
