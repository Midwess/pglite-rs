use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=PGLITE_LIB_DIR");

    let lib_dir = env::var("PGLITE_LIB_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../native/out")
    });
    let runtime_tar = lib_dir.join("pglite-runtime.tar");
    println!("cargo:rerun-if-changed={}", runtime_tar.display());

    let dest = PathBuf::from(env::var("OUT_DIR").unwrap()).join("pglite-runtime.tar");
    std::fs::copy(&runtime_tar, &dest).unwrap_or_else(|e| {
        panic!("cannot copy {} into OUT_DIR: {e}", runtime_tar.display())
    });
}
