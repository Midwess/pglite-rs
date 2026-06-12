fn main() {
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-Wl,-export_dynamic");
    #[cfg(not(target_os = "macos"))]
    println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
}
