use std::path::PathBuf;

/// `cc::Build` with stack protection and `_FORTIFY_SOURCE` (clang enables neither by default).
fn cc_build() -> cc::Build {
    let mut build = cc::Build::new();
    build
        .flag_if_supported("-fstack-protector-strong")
        .flag_if_supported("-fstack-clash-protection")
        .flag("-U_FORTIFY_SOURCE")
        .flag("-D_FORTIFY_SOURCE=2");
    build
}

fn main() {
    let qjs = PathBuf::from("vendor/quickjs");
    let napi = PathBuf::from("vendor/napi");
    cc_build()
        .include(&qjs)
        .files(
            [
                "quickjs.c",
                "libregexp.c",
                "libunicode.c",
                "dtoa.c",
                "cutils.c",
            ]
            .map(|f| qjs.join(f)),
        )
        .define("_GNU_SOURCE", None)
        // no leak dumps on stdout at exit
        .define("NDEBUG", None)
        .std("c11")
        .opt_level(2)
        .warnings(false)
        .compile("quickjs");
    // napi_* must stay in the binary for addons: link the shim whole.
    // clang: the shim uses compound literals in static initializers (gcc rejects them)
    cc_build()
        .compiler("clang")
        .include(&qjs)
        .include(napi.join("common"))
        .include(napi.join("quickjs"))
        .file(napi.join("quickjs/quickjs-api.c"))
        .define("__QJS_NG__", None)
        .define("_GNU_SOURCE", None)
        .std("gnu11")
        .opt_level(2)
        .warnings(false)
        .link_lib_modifier("+whole-archive")
        .compile("quickjs_napi");
    cc_build()
        .cpp(true)
        .include(&qjs)
        .include(napi.join("common"))
        .include(napi.join("quickjs"))
        .file("csrc/jsr_c.cpp")
        .define("__QJS_NG__", None)
        .std("c++17")
        .opt_level(2)
        .warnings(false)
        .link_lib_modifier("+whole-archive")
        .compile("quickjs_jsr");
    // exports napi_* to addons and keeps the whole-archive shim
    println!("cargo:rustc-link-arg-bins=-rdynamic");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=csrc");
}
