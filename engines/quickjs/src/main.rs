// links the engine's napi_* / js_* symbols
extern crate nativescript_quickjs;

fn main() -> std::process::ExitCode {
    nativescript_napi::cli::main("quickjs")
}
