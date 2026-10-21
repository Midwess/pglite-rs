use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=lib");
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    if root.join("lib/libpglite.a").exists() || root.join("lib/libpglite.a.zst").exists() {
        println!("cargo:root={}", root.display());
    }
}
