// links libhermes.so (napi_* / js_*)
extern crate nativescript_hermes;

fn main() -> std::process::ExitCode {
    nativescript_napi_buffer::keep();
    nativescript_napi::cli::main("hermes")
}
