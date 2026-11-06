fn main() {
    // napi_* must be visible to dlopen()ed addons
    println!("cargo:rustc-link-arg-tests=-rdynamic");
    // the event loop runs on GLib's main context
    pkg_config::Config::new()
        .atleast_version("2.56")
        .probe("glib-2.0")
        .expect("glib-2.0 development files are required (libglib2.0-dev)");
}
