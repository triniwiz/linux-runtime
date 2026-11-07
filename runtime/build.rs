fn main() {
    // napi_* must be visible to dlopen()ed addons
    println!("cargo:rustc-link-arg-tests=-rdynamic");
}
