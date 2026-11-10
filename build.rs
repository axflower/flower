use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Store the exact source dictionary once in a compact representation in
    // each consumer. The runtime restores and verifies it before using the
    // unchanged parser; this never removes words or changes ranking inputs.
    use sha2::{Digest, Sha256};
    let source = flower_dict::DEFAULT_TSV.as_bytes();
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(source, 9);
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    std::fs::write(output.join("builtin-dictionary.zlib"), &compressed)
        .expect("write compressed builtin dictionary");
    std::fs::write(
        output.join("builtin-dictionary.rs"),
        format!(
            "const BUILTIN_SIZE: usize = {};\nconst BUILTIN_SHA256: [u8; 32] = {:?};\n",
            source.len(),
            <[u8; 32]>::from(Sha256::digest(source))
        ),
    )
    .expect("write builtin dictionary identity");
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
