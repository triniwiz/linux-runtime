use nativescript_runtime::{MetaFile, Runtime};
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: ns-linux [--metadata <metadata.bin>] <script.js> | -e <code>");
    ExitCode::from(2)
}

/// `--metadata`, then `$NS_METADATA`, then `metadata-<arch>.bin` next to the
/// executable or in the working directory.
fn find_metadata(explicit: Option<PathBuf>) -> Option<PathBuf> {
    let file = format!("metadata-{}.bin", std::env::consts::ARCH);
    explicit
        .or_else(|| std::env::var_os("NS_METADATA").map(PathBuf::from))
        .or_else(|| {
            let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
            Some(exe_dir.join(&file)).filter(|p| p.exists())
        })
        .or_else(|| Some(PathBuf::from(&file)).filter(|p| p.exists()))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut metadata = None;
    let mut script = None;
    let mut eval = None;
    let mut rest = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            _ if script.is_some() || eval.is_some() => rest.push(arg),
            "--metadata" => metadata = args.next().map(PathBuf::from),
            "-e" | "--eval" => eval = args.next(),
            "-h" | "--help" => return usage(),
            _ => script = Some(PathBuf::from(arg)),
        }
    }
    if script.is_none() && eval.is_none() {
        return usage();
    }

    let Some(metadata) = find_metadata(metadata) else {
        eprintln!("ns-linux: metadata not found, pass --metadata or set NS_METADATA");
        return ExitCode::from(2);
    };
    let metadata = match MetaFile::open(&metadata) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("ns-linux: {}: {e}", metadata.display());
            return ExitCode::from(2);
        }
    };
    let mut runtime = match Runtime::new(metadata) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ns-linux: failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    // node style: [exe, script, ...args]
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let mut argv = vec![exe];
    if let Some(script) = &script {
        argv.push(std::fs::canonicalize(script).unwrap_or(script.clone()).to_string_lossy().into_owned());
    }
    argv.extend(rest);
    runtime.set_argv(&argv);
    let result = match (eval, script) {
        (Some(code), _) => runtime.run_script(&code, "[eval]"),
        (None, Some(path)) => runtime.run_file(path),
        _ => unreachable!(),
    }
    .and_then(|_| runtime.run_event_loop());
    // flush anything written with C stdio
    unsafe extern "C" {
        fn fflush(stream: *mut std::ffi::c_void) -> i32;
    }
    unsafe { fflush(std::ptr::null_mut()) };
    if result.is_err() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
