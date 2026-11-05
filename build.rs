//! Records the target triple so `hashi report` can show it.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=HASHI_BUILD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
