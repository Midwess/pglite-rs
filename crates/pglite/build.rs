use std::env;
use std::path::PathBuf;

const ENGINE_TAG: &str = "engine-06c837c6a303";

fn main() {
    println!("cargo:rerun-if-env-changed=PGLITE_LIB_DIR");

    let runtime_tar = resolve_lib_dir().join("pglite-runtime.tar");
    println!("cargo:rerun-if-changed={}", runtime_tar.display());

    let dest = PathBuf::from(env::var("OUT_DIR").unwrap()).join("pglite-runtime.tar");
    std::fs::copy(&runtime_tar, &dest)
        .unwrap_or_else(|e| panic!("cannot copy {} into OUT_DIR: {e}", runtime_tar.display()));
}

fn resolve_lib_dir() -> PathBuf {
    if let Ok(dir) = env::var("PGLITE_LIB_DIR") {
        return PathBuf::from(dir);
    }
    let local = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../native/out");
    if local.join("pglite-runtime.tar").exists() {
        return local;
    }
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::temp_dir())
        .join(".cache/pglite-rs")
        .join(ENGINE_TAG)
        .join(env::var("TARGET").unwrap())
}
