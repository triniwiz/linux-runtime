fn main() -> std::process::ExitCode {
    nativescript_napi_buffer::keep();
    nativescript_napi::cli::main_with_flags("v8", nativescript_v8::init)
}
