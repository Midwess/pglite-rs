use std::env;
use std::path::PathBuf;
use std::process::Command;

const ENGINE_TAG: &str = "engine-06c837c6a303-p2";
const RELEASE_BASE: &str = "https://github.com/Midwess/pglite-rs/releases/download";

fn main() {
    println!("cargo:rerun-if-env-changed=PGLITE_LIB_DIR");

    let lib_dir = resolve_lib_dir();
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static:+whole-archive=pglite");
    println!("cargo:rustc-link-lib=z");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-export_dynamic");
        if env::var("CARGO_FEATURE_ICU").is_ok() {
            println!("cargo:rustc-link-lib=c++");
        }
    } else {
        println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
        if env::var("CARGO_FEATURE_ICU").is_ok() {
            println!("cargo:rustc-link-lib=stdc++");
        }
    }
}

fn variant_subdir() -> &'static str {
    if env::var("CARGO_FEATURE_ICU").is_ok() {
        "icu"
    } else {
        ""
    }
}

fn asset_stem() -> &'static str {
    if env::var("CARGO_FEATURE_ICU").is_ok() {
        "pglite-icu"
    } else {
        "pglite"
    }
}

fn resolve_lib_dir() -> PathBuf {
    if let Ok(dir) = env::var("PGLITE_LIB_DIR") {
        return PathBuf::from(dir);
    }

    let local = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../native/out")
        .join(variant_subdir());
    if local.join("libpglite.a").exists() {
        return local;
    }

    download_prebuilt()
}

fn download_prebuilt() -> PathBuf {
    let target = env::var("TARGET").unwrap();
    let cache = env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::temp_dir())
        .join(".cache/pglite-rs")
        .join(ENGINE_TAG)
        .join(&target)
        .join(variant_subdir());

    if cache.join("libpglite.a").exists() {
        return cache;
    }
    std::fs::create_dir_all(&cache).expect("cannot create cache dir");

    let asset = format!("{}-{target}.tar.gz", asset_stem());
    let url = format!("{RELEASE_BASE}/{ENGINE_TAG}/{asset}");
    let tarball = cache.join(&asset);

    fetch(&url, &tarball);
    fetch(
        &format!("{url}.sha256"),
        &cache.join(format!("{asset}.sha256")),
    );

    let expected = std::fs::read_to_string(cache.join(format!("{asset}.sha256")))
        .expect("cannot read checksum file");
    let actual = sha256_of(&tarball);
    assert_eq!(
        expected.trim(),
        actual.trim(),
        "checksum mismatch for {asset}; delete {} and retry",
        cache.display()
    );

    let status = Command::new("tar")
        .args([
            "xzf",
            tarball.to_str().unwrap(),
            "-C",
            cache.to_str().unwrap(),
        ])
        .status()
        .expect("failed to run tar");
    assert!(status.success(), "failed to extract {asset}");

    cache
}

fn fetch(url: &str, dest: &std::path::Path) {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o", dest.to_str().unwrap(), url])
        .status()
        .expect("failed to run curl; install curl or set PGLITE_LIB_DIR");
    assert!(
        status.success(),
        "failed to download {url}; build the engine locally with native/build-libpglite.sh or set PGLITE_LIB_DIR"
    );
}

fn sha256_of(path: &std::path::Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256", path.to_str().unwrap()])
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output())
        .expect("need shasum or sha256sum to verify download");
    assert!(output.status.success(), "checksum tool failed");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}
