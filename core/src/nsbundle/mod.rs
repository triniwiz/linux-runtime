//! `app.nsbundle`: the app folder as one read-only archive, mounted at `<exe dir>/app`.
//!
//! Format NSB2 (little endian): 64 byte header (magic, flags, key mode, entry count, TOC
//! length, data offset, key check tag), then 88 byte records sorted by name, then the names,
//! then 16-byte aligned data. Entries are stored (zero-copy), sealed (AES-256-GCM, random
//! nonce, name as AAD) or unpacked to `app.nsbundle.unpacked/` for code that needs real files.

pub mod container;
pub mod crypto;
pub mod fuses;
pub mod pack;

pub use container::{Bytes, Container, KeyMode, Kind};
pub use fuses::Fuse;

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

pub struct Mount {
    pub root: PathBuf,
    pub bundle: PathBuf,
    pub container: Container,
    /// onlyLoadAppFromBundle: nothing under `root` is read from disk unless unpacked
    strict: bool,
}

static MOUNT: OnceLock<Mount> = OnceLock::new();

pub fn mounted() -> Option<&'static Mount> {
    MOUNT.get()
}

/// Opens `bundle` with `fuses`' key, checking it against the integrity fuse.
pub fn open(bundle: &Path, fuses: &fuses::Fuses) -> Result<Container, String> {
    let integrity = fuses.is_on(Fuse::BundleIntegrity);
    let container = Container::open(bundle, fuses.key, integrity)?;
    if integrity {
        match fuses.integrity {
            Some(hash) if hash == container.toc_hash() => {}
            Some(_) => return Err(format!("{}: integrity check failed", bundle.display())),
            None => return Err("the bundle integrity fuse is on, but no hash is embedded".into()),
        }
    }
    Ok(container)
}

/// Opens `bundle` with this binary's fuses and serves it at `root`.
pub fn mount(bundle: &Path, root: &Path) -> Result<&'static Mount, String> {
    let fuses = fuses::get();
    let mount = Mount {
        root: normalize(root),
        bundle: bundle.to_path_buf(),
        container: open(bundle, fuses)?,
        strict: fuses.is_on(Fuse::OnlyLoadAppFromBundle),
    };
    MOUNT
        .set(mount)
        .map_err(|_| "an app bundle is already mounted".to_string())?;
    Ok(MOUNT.get().unwrap())
}

/// Packaged app entry: mounts `<exe dir>/app.nsbundle` at `<exe dir>/app`, else the plaintext
/// `app` folder (refused by onlyLoadAppFromBundle).
pub fn packaged_app() -> Result<Option<PathBuf>, String> {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    else {
        return Ok(None);
    };
    let root = dir.join("app");
    let bundle = dir.join("app.nsbundle");
    if bundle.is_file() {
        return mount(&bundle, &root).map(|m| Some(m.root.clone()));
    }
    if fuses::get().is_on(Fuse::OnlyLoadAppFromBundle) {
        return Err(format!("{} is missing", bundle.display()));
    }
    Ok(root.is_dir().then_some(root))
}

/// Mounts a bundle given on the command line, at `app` next to it.
pub fn mount_file(bundle: &Path) -> Result<PathBuf, String> {
    let bundle = std::fs::canonicalize(bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
    let root = bundle.with_file_name("app");
    mount(&bundle, &root).map(|m| m.root.clone())
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

pub enum Node {
    File(usize),
    Dir,
}

impl Mount {
    /// Bundle relative name of `path`, if it is under the root.
    pub fn relative(&self, path: &Path) -> Option<String> {
        let path = normalize(path);
        let mut rel = String::new();
        for c in path.strip_prefix(&self.root).ok()?.components() {
            if !rel.is_empty() {
                rel.push('/');
            }
            rel.push_str(c.as_os_str().to_str()?);
        }
        Some(rel)
    }

    pub fn node(&self, rel: &str) -> Option<Node> {
        if let Some(i) = self.container.find(rel) {
            return Some(Node::File(i));
        }
        self.container.is_dir(rel).then_some(Node::Dir)
    }

    /// Disk path for an app path that isn't in the bundle (`None`: refused).
    fn fallback<'p>(&self, path: &'p Path) -> Option<&'p Path> {
        (!self.strict).then_some(path)
    }
}

/// Where a path lives.
enum Loc<'p> {
    Disk(&'p Path),
    Bundle(&'static Mount, usize),
    BundleDir(&'static Mount, String),
    Missing,
}

fn locate(path: &Path) -> Loc<'_> {
    let Some(m) = mounted() else {
        return Loc::Disk(path);
    };
    let Some(rel) = m.relative(path) else {
        return Loc::Disk(path);
    };
    match m.node(&rel) {
        Some(Node::File(i)) => Loc::Bundle(m, i),
        Some(Node::Dir) => Loc::BundleDir(m, rel),
        None => m.fallback(path).map_or(Loc::Missing, Loc::Disk),
    }
}

pub fn is_file(path: &Path) -> bool {
    match locate(path) {
        Loc::Disk(p) => p.is_file(),
        Loc::Bundle(..) => true,
        _ => false,
    }
}

pub fn is_dir(path: &Path) -> bool {
    match locate(path) {
        Loc::Disk(p) => p.is_dir(),
        Loc::BundleDir(..) => true,
        _ => false,
    }
}

pub fn exists(path: &Path) -> bool {
    match locate(path) {
        Loc::Disk(p) => p.exists(),
        Loc::Missing => false,
        _ => true,
    }
}

fn not_found(path: &Path) -> String {
    format!("{}: No such file or directory", path.display())
}

/// File contents; zero-copy for stored bundle entries.
pub fn read(path: &Path) -> Result<Bytes<'static>, String> {
    match locate(path) {
        Loc::Disk(p) => std::fs::read(p)
            .map(|b| Bytes::Owned(b.into()))
            .map_err(|e| format!("{}: {e}", path.display())),
        Loc::Bundle(m, i) => m.container.read(i),
        Loc::BundleDir(..) => Err(format!("{}: Is a directory", path.display())),
        Loc::Missing => Err(not_found(path)),
    }
}

pub fn read_to_string(path: &Path) -> Result<String, String> {
    let bytes = read(path)?;
    match bytes {
        Bytes::Owned(v) => String::from_utf8(std::sync::Arc::unwrap_or_clone(v)),
        Bytes::Mapped(b) => String::from_utf8(b.to_vec()),
    }
    .map_err(|_| format!("{}: not UTF-8", path.display()))
}

pub struct Stat {
    pub dir: bool,
    pub size: u64,
    /// in the bundle (not a plain file)
    pub bundled: bool,
    pub unpacked: bool,
}

pub fn stat(path: &Path) -> Option<Stat> {
    match locate(path) {
        Loc::Disk(p) => std::fs::metadata(p).ok().map(|m| Stat {
            dir: m.is_dir(),
            size: m.len(),
            bundled: false,
            unpacked: false,
        }),
        Loc::Bundle(m, i) => {
            let e = m.container.entry(i);
            Some(Stat {
                dir: false,
                size: e.size,
                bundled: true,
                unpacked: e.kind == Kind::Unpacked,
            })
        }
        Loc::BundleDir(..) => Some(Stat {
            dir: true,
            size: 0,
            bundled: true,
            unpacked: false,
        }),
        Loc::Missing => None,
    }
}

/// Directory entries: (name, is directory).
pub fn read_dir(path: &Path) -> Result<Vec<(String, bool)>, String> {
    match locate(path) {
        Loc::Disk(p) => std::fs::read_dir(p)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(|e| {
                let e = e.map_err(|e| e.to_string())?;
                let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false) || e.path().is_dir();
                Ok((e.file_name().to_string_lossy().into_owned(), dir))
            })
            .collect(),
        Loc::BundleDir(m, rel) => Ok(m
            .container
            .list(&rel)
            .unwrap_or_default()
            .into_iter()
            .map(|(n, d)| (n.to_owned(), d))
            .collect()),
        Loc::Bundle(..) => Err(format!("{}: Not a directory", path.display())),
        Loc::Missing => Err(not_found(path)),
    }
}

/// Path native code can open: unpacked entries resolve to `app.nsbundle.unpacked/`.
/// `None` for packed entries (they exist only in the bundle).
pub fn real_path(path: &Path) -> Option<PathBuf> {
    match locate(path) {
        Loc::Disk(p) => Some(p.to_path_buf()),
        Loc::Bundle(m, i) => m.container.unpacked_path(i),
        Loc::BundleDir(m, rel) => Some(m.container.unpacked_dir().join(rel)),
        Loc::Missing => None,
    }
}

/// Path for `dlopen`: unpacked and, with the integrity fuse, hash checked.
pub fn native_path(path: &Path) -> Result<PathBuf, String> {
    match locate(path) {
        Loc::Bundle(m, i) => {
            let disk = m.container.unpacked_path(i).ok_or_else(|| {
                format!(
                    "{}: native code must be unpacked from the bundle",
                    path.display()
                )
            })?;
            m.container.read(i)?;
            Ok(disk)
        }
        Loc::Missing => Err(not_found(path)),
        _ => Ok(path.to_path_buf()),
    }
}

/// Inside the mounted (read-only) app root.
pub fn in_bundle(path: &Path) -> bool {
    mounted().is_some_and(|m| m.relative(path).is_some())
}

/// For JS (`__nsBundle`): the app root, and the mounted bundle (sealed, unpacked folder).
pub fn info() -> (Option<PathBuf>, Option<(bool, PathBuf)>) {
    match mounted() {
        Some(m) => (
            Some(m.root.clone()),
            Some((
                m.container.sealed(),
                m.container.unpacked_dir().to_path_buf(),
            )),
        ),
        None => (crate::resolve::app_root(), None),
    }
}

/// App path of a JS argument: absolute, `~/` or root relative, or a `file://` URL.
pub fn app_path(arg: &str) -> PathBuf {
    let root = info().0.unwrap_or_default();
    if arg.starts_with("file://") {
        return crate::resolve::file_url_to_path(arg);
    }
    let rest = arg.strip_prefix("~/").unwrap_or(arg);
    if rest.starts_with('/') {
        PathBuf::from(rest)
    } else {
        root.join(rest)
    }
}

#[cfg(test)]
mod tests;
