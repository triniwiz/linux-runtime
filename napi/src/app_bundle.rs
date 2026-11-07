//! `__nsBundle` natives (same names and shapes as the V8 runtime).

use crate::js::*;
use crate::sys::*;
use nativescript_core::nsbundle::{self as vfs, Bytes};
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::Arc;

type Cb = unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value;

unsafe extern "C" fn drop_vec(_: napi_env, _: *mut c_void, hint: *mut c_void) {
    drop(Box::from_raw(hint as *mut Vec<u8>));
}

/// ArrayBuffer aliasing the bundle mapping, owning decrypted bytes.
fn array_buffer(js: Js, bytes: Bytes<'static>) -> napi_value {
    let mut result = null_mut();
    match bytes {
        Bytes::Mapped(b) if !b.is_empty() => unsafe {
            napi_create_external_arraybuffer(
                js.0,
                b.as_ptr() as *mut c_void,
                b.len(),
                None,
                null_mut(),
                &mut result,
            );
        },
        // cached decrypted entries are shared: JS gets its own copy
        Bytes::Owned(v) if !v.is_empty() => unsafe {
            let mut v = Box::new(Arc::unwrap_or_clone(v));
            let (ptr, len) = (v.as_mut_ptr(), v.len());
            napi_create_external_arraybuffer(
                js.0,
                ptr as *mut c_void,
                len,
                Some(drop_vec),
                Box::into_raw(v) as *mut c_void,
                &mut result,
            );
        },
        _ => return js.arraybuffer(0).0,
    }
    result
}

unsafe fn path_arg(env: napi_env, info: napi_callback_info) -> std::path::PathBuf {
    let (a, ..) = cb_args::<1>(env, info);
    vfs::app_path(&Js(env).to_string(a[0]))
}

fn opt_string(js: Js, s: Option<&std::path::Path>) -> napi_value {
    s.map_or(js.undefined(), |p| js.string(&p.to_string_lossy()))
}

unsafe extern "C" fn info(env: napi_env, _: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (root, mount) = vfs::info();
    js.array(&[
        js.boolean(mount.is_some()),
        opt_string(js, root.as_deref()),
        js.boolean(mount.as_ref().is_some_and(|m| m.0)),
        opt_string(js, mount.as_ref().map(|m| m.1.as_path())),
    ])
}

unsafe extern "C" fn read(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    match vfs::read(&path_arg(env, info)) {
        Ok(bytes) => array_buffer(js, bytes),
        Err(e) => {
            js.throw(&e);
            null_mut()
        }
    }
}

unsafe extern "C" fn stat(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    match vfs::stat(&path_arg(env, info)) {
        Some(s) => js.array(&[
            js.boolean(s.dir),
            js.number(s.size as f64),
            js.boolean(s.bundled),
            js.boolean(s.unpacked),
        ]),
        None => js.undefined(),
    }
}

unsafe extern "C" fn list(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    match vfs::read_dir(&path_arg(env, info)) {
        Ok(entries) => {
            let items: Vec<napi_value> = entries
                .iter()
                .map(|(n, dir)| js.string(&if *dir { format!("{n}/") } else { n.clone() }))
                .collect();
            js.array(&items)
        }
        Err(e) => {
            js.throw(&e);
            null_mut()
        }
    }
}

unsafe extern "C" fn realpath(env: napi_env, info: napi_callback_info) -> napi_value {
    opt_string(Js(env), vfs::real_path(&path_arg(env, info)).as_deref())
}

pub fn install(js: Js, natives: napi_value) {
    for (name, cb) in [
        ("bundleInfo", info as Cb),
        ("bundleRead", read),
        ("bundleStat", stat),
        ("bundleList", list),
        ("bundleRealpath", realpath),
    ] {
        js.set(natives, name, js.function(name, Some(cb), null_mut()));
    }
}
