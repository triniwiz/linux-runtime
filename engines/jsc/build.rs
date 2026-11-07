use std::path::{Path, PathBuf};

/// WebKitGTK's JavaScriptCore, newest first; all export the same C API.
const SONAMES: [&str; 3] = [
    "libjavascriptcoregtk-6.0.so.1",
    "libjavascriptcoregtk-4.1.so.0",
    "libjavascriptcoregtk-4.0.so.18",
];

fn lib_dirs() -> Vec<PathBuf> {
    let multiarch = format!("/usr/lib/{}-linux-gnu", std::env::consts::ARCH);
    [multiarch.as_str(), "/usr/lib64", "/usr/lib", "/usr/local/lib"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// `$NS_JSC_LIB` (a soname or path), else the newest installed soname.
fn find_jsc() -> (Option<PathBuf>, String) {
    if let Ok(lib) = std::env::var("NS_JSC_LIB") {
        let p = Path::new(&lib);
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let dir = p.parent().filter(|d| !d.as_os_str().is_empty());
        return (dir.map(PathBuf::from), name);
    }
    for name in SONAMES {
        for dir in lib_dirs() {
            if dir.join(name).exists() {
                return (Some(dir), name.into());
            }
        }
    }
    panic!("libjavascriptcoregtk not found (install libjavascriptcoregtk-6.0-1 or -4.1-0), or set NS_JSC_LIB");
}

fn main() {
    let vendor = PathBuf::from("vendor");
    // napi_* must stay in the binary for addons: link the shim whole
    cc::Build::new()
        .cpp(true)
        .include(vendor.join("include"))
        .include(vendor.join("napi"))
        .include(vendor.join("shim"))
        .files(["shim/jsc-api.cpp", "shim/jsr.cpp"].map(|f| vendor.join(f)))
        .std("c++17")
        .opt_level(2)
        .warnings(false)
        .link_lib_modifier("+whole-archive")
        .compile("jsc_napi");
    let (dir, soname) = find_jsc();
    if let Some(dir) = dir {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    println!("cargo:rustc-link-lib=dylib:+verbatim={soname}");
    println!("cargo:rustc-env=NS_JSC_SONAME={soname}");
    println!("cargo:rustc-link-arg-bins=-rdynamic");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-env-changed=NS_JSC_LIB");
}
