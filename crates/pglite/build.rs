use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const ENGINE_TAG: &str = "engine-06c837c6a303-p2";
const RELEASE_BASE: &str = "https://github.com/Midwess/pglite-rs/releases/download";

const EXTENSIONS: &[(&str, &str)] = &[
    ("pgcrypto", "CARGO_FEATURE_PGCRYPTO"),
    ("pgvector", "CARGO_FEATURE_PGVECTOR"),
];

fn main() {
    println!("cargo:rerun-if-env-changed=PGLITE_LIB_DIR");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-export_dynamic");
    } else {
        println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
    }

    let lib_dir = resolve_lib_dir();
    let base_tar = lib_dir.join("pglite-runtime.tar");
    println!("cargo:rerun-if-changed={}", base_tar.display());

    let enabled: Vec<&str> = EXTENSIONS
        .iter()
        .filter(|(_, flag)| env::var(flag).is_ok())
        .map(|(name, _)| *name)
        .collect();

    let dest = PathBuf::from(env::var("OUT_DIR").unwrap()).join("pglite-runtime.tar");
    if enabled.is_empty() {
        std::fs::copy(&base_tar, &dest)
            .unwrap_or_else(|e| panic!("cannot copy {} into OUT_DIR: {e}", base_tar.display()));
        return;
    }

    let target = env::var("TARGET").unwrap();
    let merge_dir = PathBuf::from(env::var("OUT_DIR").unwrap()).join("runtime-merge");
    let _ = std::fs::remove_dir_all(&merge_dir);
    std::fs::create_dir_all(&merge_dir).unwrap();

    run_tar(&[
        "xf",
        base_tar.to_str().unwrap(),
        "-C",
        merge_dir.to_str().unwrap(),
    ]);

    for name in &enabled {
        let ext_tar = resolve_ext_tar(name, &target, &lib_dir);
        println!("cargo:rerun-if-changed={}", ext_tar.display());
        run_tar(&[
            "xzf",
            ext_tar.to_str().unwrap(),
            "-C",
            merge_dir.to_str().unwrap(),
        ]);
    }

    let _ = std::fs::remove_file(&dest);
    run_tar(&[
        "cf",
        dest.to_str().unwrap(),
        "-C",
        merge_dir.to_str().unwrap(),
        ".",
    ]);
}

fn variant_subdir() -> &'static str {
    if env::var("CARGO_FEATURE_ICU").is_ok() {
        "icu"
    } else {
        ""
    }
}

fn resolve_lib_dir() -> PathBuf {
    if let Ok(dir) = env::var("PGLITE_LIB_DIR") {
        return PathBuf::from(dir);
    }
    let local = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../native/out")
        .join(variant_subdir());
    if local.join("pglite-runtime.tar").exists() {
        return local;
    }
    cache_dir()
}

fn cache_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::temp_dir())
        .join(".cache/pglite-rs")
        .join(ENGINE_TAG)
        .join(env::var("TARGET").unwrap())
        .join(variant_subdir())
}

fn resolve_ext_tar(name: &str, target: &str, lib_dir: &Path) -> PathBuf {
    let asset = format!("pglite-ext-{name}-{target}.tar.gz");
    let local = lib_dir.join(&asset);
    if local.exists() {
        return local;
    }
    let cached = cache_dir().join(&asset);
    if cached.exists() {
        return cached;
    }
    std::fs::create_dir_all(cache_dir()).unwrap();
    let url = format!("{RELEASE_BASE}/{ENGINE_TAG}/{asset}");
    fetch(&url, &cached);
    fetch(
        &format!("{url}.sha256"),
        &cache_dir().join(format!("{asset}.sha256")),
    );
    let expected = std::fs::read_to_string(cache_dir().join(format!("{asset}.sha256"))).unwrap();
    let actual = sha256_of(&cached);
    assert_eq!(
        expected.trim(),
        actual.trim(),
        "checksum mismatch for {asset}"
    );
    cached
}

fn run_tar(args: &[&str]) {
    let status = Command::new("tar")
        .args(args)
        .status()
        .expect("failed to run tar");
    assert!(status.success(), "tar {args:?} failed");
}

fn fetch(url: &str, dest: &Path) {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o", dest.to_str().unwrap(), url])
        .status()
        .expect("failed to run curl");
    assert!(
        status.success(),
        "failed to download {url}; build locally with native/build-extensions.sh or set PGLITE_LIB_DIR"
    );
}

fn sha256_of(path: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256", path.to_str().unwrap()])
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output())
        .expect("need shasum or sha256sum");
    assert!(output.status.success(), "checksum tool failed");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}
