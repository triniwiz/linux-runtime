//! Module resolution (Node style) shared by the engines.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Import,
    Require,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Esm,
    CommonJs,
    Json,
    Addon,
}

// ---------- resolution ----------

pub fn normalize(path: &Path) -> PathBuf {
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

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

pub fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn resolve_path(p: &Path) -> Option<PathBuf> {
    if p.is_file() {
        return Some(normalize(p));
    }
    for ext in [".js", ".mjs", ".cjs", ".json", ".node"] {
        let c = with_suffix(p, ext);
        if c.is_file() {
            return Some(normalize(&c));
        }
    }
    if p.is_dir() {
        if let Some(main) =
            read_json(&p.join("package.json")).and_then(|j| j["main"].as_str().map(str::to_owned))
        {
            if let Some(r) = resolve_path(&p.join(main)) {
                return Some(r);
            }
        }
        for index in [
            "index.js",
            "index.mjs",
            "index.cjs",
            "index.json",
            "index.node",
        ] {
            let c = p.join(index);
            if c.is_file() {
                return Some(normalize(&c));
            }
        }
    }
    None
}

/// `package.json` "exports" target for `subpath` ("." or "./x").
fn resolve_exports(
    exports: &serde_json::Value,
    subpath: &str,
    conditions: &[&str],
) -> Option<String> {
    use serde_json::Value;
    fn target(v: &Value, conditions: &[&str], star: Option<&str>) -> Option<String> {
        match v {
            Value::String(s) => Some(match star {
                Some(m) => s.replace('*', m),
                None => s.clone(),
            }),
            Value::Array(items) => items.iter().find_map(|i| target(i, conditions, star)),
            Value::Object(map) => map
                .iter()
                .find(|(k, _)| conditions.contains(&k.as_str()))
                .and_then(|(_, v)| target(v, conditions, star)),
            _ => None,
        }
    }
    let is_subpath_map =
        matches!(exports, Value::Object(m) if m.keys().all(|k| k.starts_with('.')));
    if !is_subpath_map {
        return (subpath == ".")
            .then(|| target(exports, conditions, None))
            .flatten();
    }
    let map = exports.as_object()?;
    if let Some(v) = map.get(subpath) {
        return target(v, conditions, None);
    }
    map.iter().find_map(|(key, v)| {
        let (prefix, suffix) = key.split_once('*')?;
        let middle = subpath.strip_prefix(prefix)?.strip_suffix(suffix)?;
        target(v, conditions, Some(middle))
    })
}

fn resolve_package(spec: &str, base: &Path, kind: Kind) -> Option<PathBuf> {
    let mut parts = spec.splitn(if spec.starts_with('@') { 3 } else { 2 }, '/');
    let name = if spec.starts_with('@') {
        format!("{}/{}", parts.next()?, parts.next()?)
    } else {
        parts.next()?.to_owned()
    };
    let subpath = parts
        .next()
        .map(|s| format!("./{s}"))
        .unwrap_or_else(|| ".".into());
    let conditions: &[&str] = match kind {
        Kind::Import => &["import", "default"],
        Kind::Require => &["require", "default"],
    };
    for dir in base.ancestors() {
        let pkg_dir = dir.join("node_modules").join(&name);
        if !pkg_dir.is_dir() {
            continue;
        }
        let pkg = read_json(&pkg_dir.join("package.json"));
        if let Some(exports) = pkg.as_ref().and_then(|p| p.get("exports")) {
            let target = resolve_exports(exports, &subpath, conditions)?;
            return resolve_path(&pkg_dir.join(target));
        }
        return resolve_path(&pkg_dir.join(&subpath));
    }
    None
}

pub fn file_url_to_path(url: &str) -> PathBuf {
    let raw = url.strip_prefix("file://").unwrap_or(url);
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

pub fn path_to_file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

pub fn is_url(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.starts_with("http://") || s.starts_with("https://")
}

static APP_ROOT: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Root for `~/` specifiers (the app folder), set when the entry runs.
pub fn set_app_root(path: &Path) {
    *APP_ROOT.write().unwrap() = Some(path.to_path_buf());
}

pub fn resolve(spec: &str, base: &Path, kind: Kind) -> Result<PathBuf, String> {
    if spec.starts_with("http://") || spec.starts_with("https://") {
        return Ok(PathBuf::from(spec));
    }
    // NativeScript app-relative specifiers
    if let Some(rest) = spec.strip_prefix("~/") {
        let root = APP_ROOT
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| base.to_path_buf());
        return resolve_path(&root.join(rest))
            .ok_or_else(|| format!("Cannot find module '{spec}' from '{}'", root.display()));
    }
    // relative imports inside a remote module
    if is_url(base) && (spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../"))
    {
        let base =
            url::Url::parse(&format!("{}/", base.to_string_lossy())).map_err(|e| e.to_string())?;
        return base
            .join(spec)
            .map(|u| PathBuf::from(u.as_str()))
            .map_err(|e| e.to_string());
    }
    let found = if spec.starts_with("file://") {
        resolve_path(&file_url_to_path(spec))
    } else if spec.starts_with('/')
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec == "."
        || spec == ".."
    {
        resolve_path(&base.join(spec))
    } else {
        resolve_package(spec, base, kind)
    };
    found.ok_or_else(|| format!("Cannot find module '{spec}' from '{}'", base.display()))
}

fn looks_like_esm(source: &str) -> bool {
    source.lines().any(|l| {
        let l = l.trim_start();
        (l.starts_with("import ")
            || l.starts_with("import{")
            || l.starts_with("import*")
            || l.starts_with("export "))
            && !l.starts_with("import(")
    })
}

pub fn format_of(path: &Path) -> Format {
    if is_url(path) {
        return Format::Esm;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("mjs") => Format::Esm,
        Some("cjs") => Format::CommonJs,
        Some("json") => Format::Json,
        Some("node") => Format::Addon,
        _ => {
            for dir in path.ancestors().skip(1) {
                if let Some(pkg) = read_json(&dir.join("package.json")) {
                    match pkg["type"].as_str() {
                        Some("module") => return Format::Esm,
                        Some("commonjs") => return Format::CommonJs,
                        _ => break,
                    }
                }
            }
            match std::fs::read_to_string(path) {
                Ok(src) if looks_like_esm(&src) => Format::Esm,
                _ => Format::CommonJs,
            }
        }
    }
}

/// Source of a local module (shebang commented out).
pub fn read_source(path: &Path) -> Result<String, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(strip_shebang(src))
}

pub fn strip_shebang(src: String) -> String {
    if src.starts_with("#!") {
        format!("//{src}")
    } else {
        src
    }
}

/// Entry script for `path`: a file, or a directory resolved like `require(dir)` (package.json "main", index.js).
pub fn entry_point(path: &Path) -> Result<PathBuf, String> {
    let path = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if path.is_dir() {
        set_app_root(&path);
        return resolve_path(&path)
            .ok_or_else(|| format!("{}: no package.json \"main\" or index.js", path.display()));
    }
    if APP_ROOT.read().unwrap().is_none() {
        set_app_root(&dir_of(&path));
    }
    Ok(path)
}

pub fn dir_of(path: &Path) -> PathBuf {
    path.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"))
}
