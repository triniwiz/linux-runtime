//! Links libhermes.a from `$NS_HERMES_DIR` or vendor/<arch> (build-hermes.sh)
//! and copies hermesc beside the binary.
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = std::env::var_os("NS_HERMES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("vendor").join(std::env::consts::ARCH));
    let lib = dir.join("libhermes.a");
    if !lib.exists() {
        panic!(
            "{} not found: run engines/hermes/build-hermes.sh or set NS_HERMES_DIR",
            lib.display()
        );
    }
    println!("cargo:rustc-link-search=native={}", dir.display());
    // napi_* must stay in the binary for addons
    println!("cargo:rustc-link-lib=static:+whole-archive=hermes");
    for lib in ["icui18n", "icuuc", "stdc++", "m"] {
        println!("cargo:rustc-link-lib={lib}");
    }
    println!("cargo:rustc-link-arg-bins=-rdynamic");
    // OUT_DIR is under target/<profile>/build/
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile = out
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "build"))
        .and_then(|p| p.parent());
    if let Some(profile) = profile {
        let _ = std::fs::copy(dir.join("hermesc"), profile.join("hermesc"));
    }
    println!("cargo:rerun-if-env-changed=NS_HERMES_DIR");
    println!("cargo:rerun-if-changed={}", lib.display());
}
