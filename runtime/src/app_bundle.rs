//! `__nsBundle` natives and zero-copy V8 values over bundle entries.

use crate::throw_error;
use nativescript_core::nsbundle::{self as vfs, Bytes};
use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

fn str<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).unwrap()
}

unsafe extern "C" fn keep_mapped(_: *mut c_void, _: usize, _: *mut c_void) {}

/// ArrayBuffer over the bytes: aliases the bundle mapping, owns decrypted bytes.
pub fn array_buffer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    bytes: Bytes<'static>,
) -> v8::Local<'s, v8::ArrayBuffer> {
    if bytes.is_empty() {
        return v8::ArrayBuffer::new(scope, 0);
    }
    let store = match bytes {
        // the mapping lives as long as the process
        Bytes::Mapped(b) => unsafe {
            v8::ArrayBuffer::new_backing_store_from_ptr(
                b.as_ptr() as *mut c_void,
                b.len(),
                keep_mapped,
                std::ptr::null_mut(),
            )
        },
        // cached decrypted entries are shared: JS gets its own copy
        Bytes::Owned(v) => v8::ArrayBuffer::new_backing_store_from_vec(Arc::unwrap_or_clone(v)),
    }
    .make_shared();
    v8::ArrayBuffer::with_backing_store(scope, &store)
}

/// Module source as a V8 string; ASCII bundle entries are external (not copied).
pub fn source_string<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    path: &Path,
) -> Result<v8::Local<'s, v8::String>, String> {
    let bytes = nativescript_core::resolve::read_bytes(path)?;
    let large_ascii = bytes.len() >= 1024 && !bytes.starts_with(b"#!") && bytes.is_ascii();
    let external = match (&bytes, large_ascii) {
        (Bytes::Mapped(b), true) => v8::String::new_external_onebyte_static(scope, b),
        (Bytes::Owned(v), true) => {
            let owned = Arc::try_unwrap(v.clone()).unwrap_or_else(|v| (*v).clone());
            v8::String::new_external_onebyte(scope, owned.into_boxed_slice())
        }
        _ => None,
    };
    if let Some(s) = external {
        return Ok(s);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| format!("{}: not UTF-8", path.display()))?;
    let text = if text.starts_with("#!") {
        std::borrow::Cow::Owned(format!("//{text}"))
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    v8::String::new(scope, &text).ok_or_else(|| format!("{}: too large", path.display()))
}

fn path_arg(scope: &mut v8::PinScope, args: &Args) -> std::path::PathBuf {
    vfs::app_path(&args.get(0).to_rust_string_lossy(scope))
}

/// [mounted, root, sealed, unpacked dir]
fn info<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, mut rv: Rv<'s>) {
    let (root, mount) = vfs::info();
    let undefined: v8::Local<v8::Value> = v8::undefined(scope).into();
    let root = root.map_or(undefined, |r| str(scope, &r.to_string_lossy()).into());
    let items = [
        v8::Boolean::new(scope, mount.is_some()).into(),
        root,
        v8::Boolean::new(scope, mount.as_ref().is_some_and(|m| m.0)).into(),
        mount.map_or(undefined, |m| str(scope, &m.1.to_string_lossy()).into()),
    ];
    rv.set(v8::Array::new_with_elements(scope, &items).into());
}

fn read<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let path = path_arg(scope, &args);
    match vfs::read(&path) {
        Ok(bytes) => rv.set(array_buffer(scope, bytes).into()),
        Err(e) => throw_error(scope, &e),
    }
}

/// undefined or [isDirectory, size, inBundle, unpacked]
fn stat<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let path = path_arg(scope, &args);
    if let Some(s) = vfs::stat(&path) {
        let items = [
            v8::Boolean::new(scope, s.dir).into(),
            v8::Number::new(scope, s.size as f64).into(),
            v8::Boolean::new(scope, s.bundled).into(),
            v8::Boolean::new(scope, s.unpacked).into(),
        ];
        rv.set(v8::Array::new_with_elements(scope, &items).into());
    }
}

/// names, directories with a trailing `/`
fn list<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let path = path_arg(scope, &args);
    match vfs::read_dir(&path) {
        Ok(entries) => {
            let items: Vec<v8::Local<v8::Value>> = entries
                .iter()
                .map(|(n, dir)| str(scope, &if *dir { format!("{n}/") } else { n.clone() }).into())
                .collect();
            rv.set(v8::Array::new_with_elements(scope, &items).into());
        }
        Err(e) => throw_error(scope, &e),
    }
}

fn realpath<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let path = path_arg(scope, &args);
    if let Some(p) = vfs::real_path(&path) {
        rv.set(str(scope, &p.to_string_lossy()).into());
    }
}

pub fn install<'s>(scope: &mut v8::PinScope<'s, '_>, natives: v8::Local<'s, v8::Object>) {
    macro_rules! set {
        ($name:literal, $f:expr) => {{
            let f = v8::Function::new(scope, $f).unwrap();
            let k = str(scope, $name);
            natives.set(scope, k.into(), f.into());
        }};
    }
    set!("bundleInfo", info);
    set!("bundleRead", read);
    set!("bundleStat", stat);
    set!("bundleList", list);
    set!("bundleRealpath", realpath);
}
