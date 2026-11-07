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

/// A Worker script path: `~/` and relative paths are in the app folder (bundled worker chunks
/// sit next to the bundle), `file://` URLs and absolute paths as given.
pub fn worker_script(spec: &str) -> PathBuf {
    if let Some(p) = spec.strip_prefix("file://") {
        return PathBuf::from(p);
    }
    let rest = spec.strip_prefix("~/").unwrap_or(spec);
    if rest.starts_with('/') {
        return PathBuf::from(rest);
    }
    let root = APP_ROOT
        .read()
        .unwrap()
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    root.join(rest.strip_prefix("./").unwrap_or(rest))
}

/// Dev loader settings (`ns:module` `configureLoader`, sent by the Vite dev server).
#[derive(Default)]
struct Loader {
    /// import map entries, longest key first
    imports: Vec<(String, String)>,
    /// URLs containing one of these are re-fetched on every import
    volatile: Vec<String>,
    strip_params: Vec<String>,
    for_prefixes: Vec<String>,
    preserve_query: Vec<String>,
}

static LOADER: std::sync::RwLock<Option<Loader>> = std::sync::RwLock::new(None);

/// `{ importMap: { imports }, volatilePatterns?, canonicalization? }`
pub fn configure_loader(config: &serde_json::Value) {
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut imports: Vec<(String, String)> = config["importMap"]["imports"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    imports.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    let c = &config["canonicalization"];
    *LOADER.write().unwrap() = Some(Loader {
        imports,
        volatile: strings(&config["volatilePatterns"]),
        strip_params: strings(&c["stripParams"]),
        for_prefixes: strings(&c["forPathPrefixes"]),
        preserve_query: strings(&c["preserveQueryFor"]),
    });
}

/// Import map target of a bare specifier (exact key, or the longest `prefix/` key).
fn import_map(spec: &str) -> Option<String> {
    let loader = LOADER.read().unwrap();
    loader.as_ref()?.imports.iter().find_map(|(key, target)| {
        if key == spec {
            Some(target.clone())
        } else if key.ends_with('/') {
            spec.strip_prefix(key.as_str())
                .map(|rest| format!("{target}{rest}"))
        } else {
            None
        }
    })
}

/// Whether `url` must be fetched and evaluated again on every import.
pub fn is_volatile(path: &Path) -> bool {
    let url = path.to_string_lossy();
    LOADER
        .read()
        .unwrap()
        .as_ref()
        .is_some_and(|l| l.volatile.iter().any(|p| url.contains(p.as_str())))
}

/// Module registry key of a dev server URL: no fragment, no cache-buster
/// params, sorted query (unless the query is the module's identity).
pub fn canonical(path: PathBuf) -> PathBuf {
    if !is_url(&path) {
        return path;
    }
    let loader = LOADER.read().unwrap();
    let Ok(mut url) = url::Url::parse(&path.to_string_lossy()) else {
        return path;
    };
    url.set_fragment(None);
    if let Some(l) = loader.as_ref() {
        let route = url.path();
        let normalize = l.for_prefixes.iter().any(|p| route.starts_with(p.as_str()))
            && !l.preserve_query.iter().any(|p| route.contains(p.as_str()));
        if normalize && url.query().is_some() {
            let mut pairs: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(k, _)| !l.strip_params.iter().any(|s| s == k))
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            pairs.sort();
            if pairs.is_empty() {
                url.set_query(None);
            } else {
                url.query_pairs_mut().clear().extend_pairs(pairs);
            }
        }
    }
    PathBuf::from(String::from(url))
}

/// Builtin modules (`ns:module`), resolved by the engine.
pub fn is_builtin(path: &Path) -> bool {
    path.to_string_lossy().starts_with("ns:")
}

/// Root for `~/` specifiers (the app folder), set when the entry runs.
pub fn set_app_root(path: &Path) {
    *APP_ROOT.write().unwrap() = Some(path.to_path_buf());
}

pub fn resolve(spec: &str, base: &Path, kind: Kind) -> Result<PathBuf, String> {
    resolve_uncanonical(spec, base, kind).map(|p| if is_volatile(&p) { p } else { canonical(p) })
}

fn resolve_uncanonical(spec: &str, base: &Path, kind: Kind) -> Result<PathBuf, String> {
    if spec.starts_with("http://") || spec.starts_with("https://") || spec.starts_with("ns:") {
        return Ok(PathBuf::from(spec));
    }
    // native libraries bundled with the app, in lib/ next to the executable (as on Android and Windows)
    if let Some(rest) = spec.strip_prefix("system_lib://") {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let path = exe
            .parent()
            .map(|d| d.join("lib").join(rest))
            .unwrap_or_else(|| PathBuf::from(rest));
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "Cannot find module '{spec}' at '{}'",
                path.display()
            ))
        };
    }
    let bare = !(spec.starts_with('/')
        || spec.starts_with('.')
        || spec.starts_with("~/")
        || spec.starts_with("file:"));
    if bare {
        if let Some(target) = import_map(spec) {
            return resolve_uncanonical(&target, base, kind);
        }
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
    // only statements count: lines that start inside a comment or template literal don't
    let (mut in_comment, mut in_template) = (false, false);
    for line in source.lines() {
        let l = line.trim_start();
        if !in_comment
            && !in_template
            && (l.starts_with("import ")
                || l.starts_with("import{")
                || l.starts_with("import*")
                || l.starts_with("export "))
            && !l.starts_with("import(")
        {
            return true;
        }
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            let next = bytes.get(i + 1).copied();
            if in_comment {
                if c == b'*' && next == Some(b'/') {
                    in_comment = false;
                    i += 1;
                }
            } else if in_template {
                if c == b'\\' {
                    i += 1;
                } else if c == b'`' {
                    in_template = false;
                }
            } else if c == b'/' && next == Some(b'/') {
                break;
            } else if c == b'/' && next == Some(b'*') {
                in_comment = true;
                i += 1;
            } else if c == b'`' {
                in_template = true;
            } else if c == b'"' || c == b'\'' {
                // a one-line string
                i += 1;
                while i < bytes.len() && bytes[i] != c {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            i += 1;
        }
    }
    false
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

#[cfg(test)]
mod esm_detection {
    use super::looks_like_esm;

    #[test]
    fn esm_detection_skips_comments() {
        assert!(looks_like_esm("import x from './x';"));
        assert!(looks_like_esm("const a = 1;\nexport const b = 2;"));
        assert!(!looks_like_esm(
            "/*\nexport class DOMMatrix {}\n*/\nmodule.exports = 1;"
        ));
        assert!(!looks_like_esm("const s = `\nexport default 1\n`;"));
        assert!(!looks_like_esm("// export const x = 1\nrequire('./x');"));
        assert!(!looks_like_esm("import('./lazy');"));
    }
}
