// links the engine's napi_* / js_* symbols
extern crate nativescript_jsc;

fn main() -> std::process::ExitCode {
    nativescript_napi_buffer::keep();
    nativescript_napi::cli::main("jsc")
}
