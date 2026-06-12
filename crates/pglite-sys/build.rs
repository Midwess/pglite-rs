use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=PGLITE_LIB_DIR");

    let lib_dir = env::var("PGLITE_LIB_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../native/out")
    });

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=pglite");
    println!("cargo:rustc-link-lib=z");
}
