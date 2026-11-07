//! `--watch`: run the script in a child process, restart it when files change.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::time::{Duration, Instant};

const SKIP: [&str; 3] = ["node_modules", ".git", "target"];
/// only these trigger a restart (not logs etc.)
const SOURCES: [&str; 10] = [
    ".js", ".mjs", ".cjs", ".json", ".node", ".ts", ".jsx", ".tsx", ".css", ".html",
];

struct Watcher {
    fd: i32,
}

impl Watcher {
    fn new(root: &Path) -> Option<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        let w = Watcher { fd };
        w.add_tree(root);
        Some(w)
    }

    fn add_tree(&self, dir: &Path) {
        let Ok(path) = CString::new(dir.as_os_str().as_encoded_bytes()) else {
            return;
        };
        let mask = libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_TO;
        unsafe { libc::inotify_add_watch(self.fd, path.as_ptr(), mask) };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let skip = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| SKIP.contains(&n) || n.starts_with('.'));
            if p.is_dir() && !skip {
                self.add_tree(&p);
            }
        }
    }

    /// True when something changed within `timeout`.
    fn changed(&self, timeout: Duration) -> bool {
        let mut pfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut pfd, 1, timeout.as_millis() as i32) } <= 0 {
            return false;
        }
        let mut buf = [0u8; 4096];
        let mut relevant = false;
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            }
            // struct inotify_event { int wd; u32 mask, cookie, len; char name[len]; }
            let mut at = 0;
            while at + 16 <= n as usize {
                let len = u32::from_ne_bytes(buf[at + 12..at + 16].try_into().unwrap()) as usize;
                let name = &buf[at + 16..(at + 16 + len).min(n as usize)];
                let name = std::str::from_utf8(name)
                    .unwrap_or("")
                    .trim_end_matches('\0');
                relevant |= SOURCES.iter().any(|ext| name.ends_with(ext));
                at += 16 + len;
            }
        }
        relevant
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

fn spawn(args: &[String]) -> Option<Child> {
    let exe = std::env::current_exe().ok()?;
    Command::new(exe).args(args).spawn().ok()
}

fn stop(child: &mut Child) {
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub fn run(script: &Path, child_args: Vec<String>) -> ExitCode {
    let root = std::fs::canonicalize(script)
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let Some(watcher) = Watcher::new(&root) else {
        eprintln!("ns-linux: inotify unavailable");
        return ExitCode::FAILURE;
    };
    eprintln!("watching {} for changes", root.display());
    loop {
        let Some(mut child) = spawn(&child_args) else {
            return ExitCode::FAILURE;
        };
        loop {
            if watcher.changed(Duration::from_millis(200)) {
                // let editors finish writing
                while watcher.changed(Duration::from_millis(100)) {}
                eprintln!("change detected, restarting");
                stop(&mut child);
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                eprintln!("exited ({status}), waiting for changes");
                while !watcher.changed(Duration::from_secs(3600)) {}
                while watcher.changed(Duration::from_millis(100)) {}
                break;
            }
        }
    }
}
