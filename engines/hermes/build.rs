//! Links libhermes.so from `$NS_HERMES_DIR` or vendor/<arch> (build-hermes.sh)
//! and copies it, with hermesc, beside the binary.
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = std::env::var_os("NS_HERMES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("vendor").join(std::env::consts::ARCH));
    if !dir.join("libhermes.so").exists() {
        panic!(
            "{}/libhermes.so not found: run engines/hermes/build-hermes.sh or set NS_HERMES_DIR",
            dir.display()
        );
    }
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=hermes");
    // packaged next to the executable
    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    println!("cargo:rustc-link-arg-bins=-rdynamic");
    // OUT_DIR is under target/<profile>/build/
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile = out
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "build"))
        .and_then(|p| p.parent());
    if let Some(profile) = profile {
        for file in ["libhermes.so", "hermesc"] {
            let _ = std::fs::copy(dir.join(file), profile.join(file));
        }
    }
    println!("cargo:rerun-if-env-changed=NS_HERMES_DIR");
    println!("cargo:rerun-if-changed={}", dir.join("libhermes.so").display());
}
