use std::path::PathBuf;

fn main() {
    let qjs = PathBuf::from("vendor/quickjs");
    let napi = PathBuf::from("vendor/napi");
    cc::Build::new()
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
    cc::Build::new()
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
    cc::Build::new()
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
