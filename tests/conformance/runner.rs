// Runs every conformance script with an engine binary; `include!`d by each engine's tests.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).ancestors().find(|p| p.join("tests/conformance/harness.js").exists()).unwrap().to_path_buf()
}

/// The addon used by napi-addon.js, `None` without a C compiler.
fn build_addon(repo: &Path) -> Option<()> {
    let fixtures = repo.join("runtime/tests/fixtures/napi");
    std::fs::create_dir_all(fixtures.join("build")).ok()?;
    let status = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-DNODE_GYP_MODULE_NAME=addon", "-I"])
        .arg(fixtures.join("include"))
        .arg(fixtures.join("addon.c"))
        .arg("-o")
        .arg(fixtures.join("build/addon.node"))
        .arg("-lpthread")
        .status()
        .ok()?;
    status.success().then_some(())
}

/// (exit success, stderr), killed after 30s.
fn run_script(bin: &str, meta: &Path, script: &Path) -> (bool, String) {
    let mut child = Command::new(bin)
        .arg("--metadata")
        .arg(meta)
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("engine binary");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stderr = String::new();
    use std::io::Read;
    let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
    match status {
        Some(s) => (s.success(), stderr),
        None => (false, format!("timed out\n{stderr}")),
    }
}

pub fn run_suite(bin: &str) {
    let repo = repo();
    let meta = repo.join(format!("metadata-generator/tests/TestOutput/metadata-{}.bin", std::env::consts::ARCH));
    if !meta.exists() {
        eprintln!("skipping: {} not found", meta.display());
        return;
    }
    let dir = repo.join("tests/conformance");
    let has_addon = build_addon(&repo).is_some();
    let mut scripts: Vec<(PathBuf, Option<String>)> = Vec::new();
    for (sub, failing) in [("", false), ("fail", true)] {
        for entry in std::fs::read_dir(dir.join(sub)).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".js") || name == "harness.js" || (name == "napi-addon.js" && !has_addon) {
                continue;
            }
            let expect = failing.then(|| {
                let src = std::fs::read_to_string(&path).unwrap();
                src.lines().next().and_then(|l| l.strip_prefix("// expect: ")).unwrap_or("").to_owned()
            });
            scripts.push((path, expect));
        }
    }
    scripts.sort();
    let failures: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = scripts
            .iter()
            .map(|(script, expect)| {
                let (meta, dir) = (&meta, &dir);
                s.spawn(move || {
                    let name = script.strip_prefix(dir).unwrap().display().to_string();
                    let (ok, stderr) = run_script(bin, meta, script);
                    match expect {
                        None if !ok => Some(format!("{name}: failed\n{stderr}")),
                        Some(e) if ok => Some(format!("{name}: expected a failure ({e})")),
                        Some(e) if !stderr.contains(e.as_str()) => Some(format!("{name}: expected '{e}' in\n{stderr}")),
                        _ => None,
                    }
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
    });
    assert!(failures.is_empty(), "{} of {} conformance scripts failed:\n\n{}", failures.len(), scripts.len(), failures.join("\n"));
}
