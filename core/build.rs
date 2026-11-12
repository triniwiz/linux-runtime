fn main() {
    pkg_config::Config::new()
        .atleast_version("2.56")
        .probe("glib-2.0")
        .expect("glib-2.0 development files are required (libglib2.0-dev)");
    pkg_config::Config::new()
        .atleast_version("2.56")
        .probe("gobject-2.0")
        .expect("gobject-2.0 development files are required (libglib2.0-dev)");
}
