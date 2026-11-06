//! Command line shared by the Node-API engine binaries.

use crate::{MetaFile, Runtime};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

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

pub fn main(engine: &str) -> ExitCode {
    #[cfg(feature = "compat")]
    crate::compat::keep();
    let bin = format!("ns-linux-{engine}");
    let usage = || {
        eprintln!("usage: {bin} [--metadata <metadata.bin>] <script.js> | -e <code>");
        ExitCode::from(2)
    };
    let mut args = std::env::args().skip(1);
    let (mut metadata, mut script, mut eval, mut rest) = (None, None, None, Vec::new());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            _ if script.is_some() || eval.is_some() => rest.push(arg),
            "--metadata" => metadata = args.next().map(PathBuf::from),
            "-e" | "--eval" => eval = args.next(),
            "-h" | "--help" => return usage(),
            _ => script = Some(PathBuf::from(arg)),
        }
    }
    // packaged apps: `<exe dir>/app`
    if script.is_none() && eval.is_none() {
        script = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("app"))).filter(|a| a.is_dir());
    }
    if script.is_none() && eval.is_none() {
        return usage();
    }
    let Some(metadata) = find_metadata(metadata) else {
        eprintln!("{bin}: metadata not found, pass --metadata or set NS_METADATA");
        return ExitCode::from(2);
    };
    let metadata = match MetaFile::open(&metadata) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            eprintln!("{bin}: {}: {e}", metadata.display());
            return ExitCode::from(2);
        }
    };
    let runtime = match Runtime::new(metadata, engine) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{bin}: failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
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
    unsafe { libc::fflush(std::ptr::null_mut()) };
    if result.is_err() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
