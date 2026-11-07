//! Compiles napi-android's V8 shim (v8-api.cpp) and csrc/v8_jsr.cpp against
//! the v8 crate's headers; rusty_v8's static lib provides the V8 symbols.
use std::path::PathBuf;

/// The v8 crate's C++ headers, from its registry source (`DEP_V8_*` isn't exported).
fn v8_include() -> PathBuf {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".cargo"));
    let version = "147.";
    let mut hits: Vec<PathBuf> = std::fs::read_dir(home.join("registry/src"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|index| std::fs::read_dir(index.path()).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&format!("v8-{version}")))
                && p.join("v8/include/v8.h").exists()
        })
        .collect();
    hits.sort();
    hits.pop()
        .map(|p| p.join("v8/include"))
        .expect("v8 crate sources (v8-147.x/v8/include) not found in the cargo registry")
}

fn main() {
    let vendor = PathBuf::from("vendor");
    // napi_* must stay in the binary for addons: link the shim whole
    cc::Build::new()
        .cpp(true)
        // libstdc++ (clang here would pick a GCC without its C++ headers)
        .compiler("g++")
        .include(v8_include())
        .include(vendor.join("shim"))
        .include(vendor.join("napi"))
        .include(vendor.join("compat"))
        .files(["shim/v8-api.cpp", "shim/SimpleAllocator.cpp"].map(|f| vendor.join(f)))
        .file("csrc/v8_jsr.cpp")
        .define("NAPI_VERSION", "8")
        // V8 > 13 code paths (SetAccessorProperty, ...)
        .define("__V8_13__", None)
        .std("c++20")
        .opt_level(2)
        .warnings(false)
        .link_lib_modifier("+whole-archive")
        .compile("v8_napi");
    println!("cargo:rustc-link-arg-bins=-rdynamic");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=csrc");
}
