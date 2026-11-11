//! Command line shared by the Node-API engine binaries.

use crate::Runtime;
use nativescript_core::nsbundle::{self, fuses, Fuse};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

pub fn main(engine: &str) -> ExitCode {
    main_with_flags(engine, |_| {})
}

/// [`main`] for engines taking V8 flags (`linux.v8Flags`), applied before the runtime starts.
pub fn main_with_flags(engine: &str, set_flags: fn(&str)) -> ExitCode {
    #[cfg(feature = "compat")]
    crate::compat::keep();
    let bin = format!("ns-linux-{engine}");
    let usage = || {
        eprintln!("usage: {bin} [--metadata <metadata.bin[:more.bin...]>] <script.js> | -e <code>");
        ExitCode::from(2)
    };
    // release builds (envOverrides fuse off): every argument belongs to the app
    let overrides = fuses::get().is_on(Fuse::EnvOverrides);
    let mut args = std::env::args().skip(1);
    let (mut metadata, mut script, mut eval, mut rest) = (None, None, None, Vec::new());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            _ if !overrides || script.is_some() || eval.is_some() => rest.push(arg),
            "--metadata" => metadata = args.next().map(OsString::from),
            "-e" | "--eval" => eval = args.next(),
            "-h" | "--help" => return usage(),
            _ => script = Some(PathBuf::from(arg)),
        }
    }
    // packaged apps: `<exe dir>/app.nsbundle` or `<exe dir>/app`
    let packaged = if script.is_none() && eval.is_none() {
        nsbundle::packaged_app().map(|app| script = app)
    } else if let Some(b) = script
        .clone()
        .filter(|s| s.extension().is_some_and(|e| e == "nsbundle"))
    {
        nsbundle::mount_file(&b).map(|root| script = Some(root))
    } else {
        Ok(())
    };
    if let Err(e) = packaged {
        eprintln!("{bin}: {e}");
        return ExitCode::from(2);
    }
    if script.is_none() && eval.is_none() {
        return usage();
    }
    set_flags(&nativescript_core::config::v8_flags(script.as_deref()));
    let Some(metadata) = nativescript_core::bundle::find_metadata(metadata, overrides) else {
        eprintln!("{bin}: metadata not found, pass --metadata or set NS_METADATA");
        return ExitCode::from(2);
    };
    let metadata = match nativescript_core::bundle::load_metadata(&metadata) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            eprintln!("{bin}: {e}");
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
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut argv = vec![exe];
    if let Some(script) = &script {
        // bundle paths are virtual (and `app` may link to the unpacked folder)
        let script = if nsbundle::in_bundle(script) {
            script.clone()
        } else {
            std::fs::canonicalize(script).unwrap_or(script.clone())
        };
        argv.push(script.to_string_lossy().into_owned());
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
