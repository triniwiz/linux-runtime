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

/// Serves tests/conformance/http-fixtures for HTTP module imports; returns its origin.
fn http_fixtures(dir: PathBuf) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            let target = line.split_whitespace().nth(1).unwrap_or("/");
            let path = target.split('?').next().unwrap_or("").trim_start_matches('/');
            let path = match path.strip_prefix("slow/") {
                Some(rest) => {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    rest.to_owned()
                }
                None => path.to_owned(),
            };
            let body = (!path.contains("..")).then(|| std::fs::read(dir.join(&path)).ok()).flatten();
            let _ = match body {
                Some(b) => write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len())
                    .and_then(|_| stream.write_all(&b)),
                None => write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            };
        }
    });
    origin
}

/// (exit success, stderr), killed after 30s.
fn run_script(bin: &str, meta: &Path, script: &Path, http: &str) -> (bool, String) {
    let mut child = Command::new(bin)
        .env("NS_TEST_HTTP", http)
        // `gc()` for tests of GC-driven behavior (V8 engines; the others skip those parts)
        .env("NS_V8_FLAGS", "--expose-gc")
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
    let http = http_fixtures(dir.join("http-fixtures"));
    let has_addon = build_addon(&repo).is_some();
    let mut scripts: Vec<(PathBuf, Option<String>)> = Vec::new();
    for (sub, failing) in [("", false), ("fail", true)] {
        for entry in std::fs::read_dir(dir.join(sub)).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !(name.ends_with(".js") || name.ends_with(".mjs")) || name == "harness.js" || (name == "napi-addon.js" && !has_addon) {
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
                let (meta, dir, http) = (&meta, &dir, &http);
                s.spawn(move || {
                    let name = script.strip_prefix(dir).unwrap().display().to_string();
                    let (ok, stderr) = run_script(bin, meta, script, http);
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
