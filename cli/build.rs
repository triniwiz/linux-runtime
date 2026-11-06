fn main() {
    // export napi_* from the executable for Node-API addons
    println!("cargo:rustc-link-arg-bins=-rdynamic");
}
