mod watch;

use nativescript_core::nsbundle::{self, fuses, Fuse};
use nativescript_runtime::Runtime;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: ns-linux [--metadata <metadata.bin>] [--inspect[-brk][=port]] [--watch] <script.js> | -e <code>");
    ExitCode::from(2)
}

/// `--metadata`, then `$NS_METADATA`, then `metadata-<arch>.bin` next to the
/// executable or in the working directory. Release builds (envOverrides fuse off)
/// only use the one next to the executable.
fn find_metadata(explicit: Option<PathBuf>, overrides: bool) -> Option<PathBuf> {
    let file = format!("metadata-{}.bin", std::env::consts::ARCH);
    let next_to_exe = || {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        Some(exe_dir.join(&file)).filter(|p| p.exists())
    };
    if !overrides {
        return next_to_exe();
    }
    explicit
        .or_else(|| std::env::var_os("NS_METADATA").map(PathBuf::from))
        .or_else(next_to_exe)
        .or_else(|| Some(PathBuf::from(&file)).filter(|p| p.exists()))
}

fn main() -> ExitCode {
    let fuses = fuses::get();
    // release builds: every argument belongs to the app
    let overrides = fuses.is_on(Fuse::EnvOverrides);
    let mut args = std::env::args().skip(1);
    let mut metadata = None;
    let mut script = None;
    let mut eval = None;
    let mut rest = Vec::new();
    let mut inspect = None;
    let mut watch = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            _ if !overrides || script.is_some() || eval.is_some() => rest.push(arg),
            "--metadata" => metadata = args.next().map(PathBuf::from),
            a if a.starts_with("--inspect") => {
                let port = a
                    .split_once('=')
                    .and_then(|(_, p)| p.parse().ok())
                    .unwrap_or(9229);
                inspect = Some((port, a.starts_with("--inspect-brk")));
            }
            "-e" | "--eval" => eval = args.next(),
            "-h" | "--help" => return usage(),
            "--watch" => watch = true,
            _ => script = Some(PathBuf::from(arg)),
        }
    }
    // packaged apps: `<exe dir>/app.nsbundle` or `<exe dir>/app`
    let mounted = if script.is_none() && eval.is_none() {
        match nsbundle::packaged_app() {
            Ok(app) => script = app,
            Err(e) => {
                eprintln!("ns-linux: {e}");
                return ExitCode::from(2);
            }
        }
        nsbundle::mounted().is_some()
    } else if let Some(bundle) = script
        .as_ref()
        .filter(|s| s.extension().is_some_and(|e| e == "nsbundle"))
    {
        match nsbundle::mount_file(bundle) {
            Ok(root) => script = Some(root),
            Err(e) => {
                eprintln!("ns-linux: {e}");
                return ExitCode::from(2);
            }
        }
        true
    } else {
        false
    };
    if script.is_none() && eval.is_none() {
        return usage();
    }
    if inspect.is_some() && !fuses.is_on(Fuse::Inspector) {
        eprintln!("ns-linux: the inspector is disabled in this build");
        inspect = None;
    }
    if watch && mounted {
        eprintln!("ns-linux: --watch needs an app folder, not a bundle");
        return ExitCode::from(2);
    }
    if watch {
        let Some(script) = &script else {
            return usage();
        };
        let args: Vec<String> = std::env::args()
            .skip(1)
            .filter(|a| a != "--watch")
            .collect();
        return watch::run(script, args);
    }

    let Some(metadata) = find_metadata(metadata, overrides) else {
        eprintln!("ns-linux: metadata not found, pass --metadata or set NS_METADATA");
        return ExitCode::from(2);
    };
    let metadata = match nativescript_core::bundle::load_metadata(&metadata) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("ns-linux: {e}");
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
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut argv = vec![exe];
    if let Some(script) = &script {
        // bundle paths are virtual (and `app` may link to the unpacked folder)
        let script = if mounted {
            script.clone()
        } else {
            std::fs::canonicalize(script).unwrap_or(script.clone())
        };
        argv.push(script.to_string_lossy().into_owned());
    }
    argv.extend(rest);
    runtime.set_argv(&argv);
    if let Some((port, wait)) = inspect {
        let title = script
            .as_ref()
            .map(|s| s.display().to_string())
            .unwrap_or_else(|| "[eval]".into());
        if let Err(e) = runtime.start_inspector(port, wait, &title) {
            eprintln!("ns-linux: {e}");
            return ExitCode::FAILURE;
        }
    }
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
