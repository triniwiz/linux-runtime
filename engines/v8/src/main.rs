fn main() -> std::process::ExitCode {
    nativescript_napi_buffer::keep();
    nativescript_v8::init();
    nativescript_napi::cli::main("v8")
}
