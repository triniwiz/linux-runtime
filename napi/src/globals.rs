//! Lazy metadata globals: a Proxy in the global's prototype chain resolves
//! unknown names; resolved values are memoized as own global properties.

use crate::js::*;
use crate::marshal::{self, Mem};
use crate::state::{state, State};
use crate::sys::*;
use crate::{ffi, interop};
use nativescript_metadata::{Meta, MetaKind, Type};
use std::ptr::null_mut;

/// `__tsEnum` of core/js/prelude.js: C enums as TypeScript style two-way maps.
#[cfg(not(feature = "engine"))]
const TS_ENUM: &str = "function (obj) { const r = {}; for (const k of Object.keys(obj)) { r[k] = obj[k]; r[obj[k]] = k; } return r; }";

/// JS value for a declaration, and whether it can be memoized on the global.
fn resolve(st: &State, meta: Meta) -> Result<Option<(napi_value, bool)>, String> {
    let js = st.js();
    Ok(Some(match meta.kind() {
        MetaKind::Function => match ffi::function_for(st, st.meta.id(&meta)) {
            Ok(function) => (ffi::js_function(st, meta.js_name(), &function), true),
            // missing symbols resolve to undefined
            Err(_) => (js.undefined(), true),
        },
        // variables are read on every access, their value can change
        MetaKind::Var => {
            let ty = meta.var_type().ok_or("variable without type")?;
            let address = st.symbol(&meta)?;
            (
                unsafe { marshal::read(st, &ty, address as *const u8, Mem::Live(js.null()))? },
                false,
            )
        }
        MetaKind::JsCode => {
            let code = meta.js_code().unwrap_or("undefined");
            // an addon has no prelude: its enum helper stays local
            #[cfg(not(feature = "engine"))]
            let code = &format!("(function (__tsEnum) {{ return ({code}); }})({TS_ENUM})");
            (
                crate::run_script(js, code, meta.js_name())
                    .map_err(|_| "failed to evaluate constant")?,
                true,
            )
        }
        MetaKind::Struct => (
            interop::struct_constructor(
                st,
                meta.js_name(),
                Type::Struct(meta.js_name().to_owned()),
            ),
            true,
        ),
        MetaKind::Union => (
            interop::struct_constructor(st, meta.js_name(), Type::Union(meta.js_name().to_owned())),
            true,
        ),
        MetaKind::Interface | MetaKind::Protocol | MetaKind::Undefined => return Ok(None),
    }))
}

fn is_global(kind: MetaKind) -> bool {
    !matches!(
        kind,
        MetaKind::Interface | MetaKind::Protocol | MetaKind::Undefined
    )
}

pub(crate) unsafe extern "C" fn lookup_has(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (a, ..) = cb_args::<1>(env, info);
    let name = st.js().rust_string(a[0]).unwrap_or_default();
    st.js()
        .boolean(st.meta.find_global(&name).is_some_and(|m| is_global(m.kind())))
}

pub(crate) unsafe extern "C" fn lookup_get(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (a, ..) = cb_args::<1>(env, info);
    let Ok(name) = js.rust_string(a[0]) else {
        return js.undefined();
    };
    let Some(meta) = st.meta.find_global(&name) else {
        return js.undefined();
    };
    let target = match st.globals_target.get() {
        t if t.is_null() => js.global(),
        t => js.deref(t),
    };
    let define = |key: &str, value: napi_value| {
        let desc = napi_property_descriptor {
            utf8name: null_mut(),
            name: js.string(key),
            method: None,
            getter: None,
            setter: None,
            value,
            // configurable, not writable / enumerable
            attributes: 4,
            data: null_mut(),
        };
        napi_define_properties(env, target, 1, &desc);
    };
    // a struct tag (`_GdkRGBA`) shares the constructor of its typedef name
    let canonical = meta.js_name();
    if canonical != name {
        let mut own = false;
        napi_has_own_property(env, target, js.string(canonical), &mut own);
        if own {
            if let Ok(value) = js.get(target, canonical) {
                define(&name, value);
                return value;
            }
        }
    }
    match resolve(st, meta) {
        Ok(Some((value, memoize))) => {
            if memoize {
                define(&name, value);
                if canonical != name {
                    define(canonical, value);
                }
            }
            value
        }
        Ok(None) => js.undefined(),
        Err(e) => {
            js.throw(&format!("{name}: {e}"));
            null_mut()
        }
    }
}

/// Whether the Proxy could go in the global's prototype chain.
const PROXY: &str = r#"(function (has, get) {
  const g = globalThis;
  try {
    Object.setPrototypeOf(g, new Proxy(Object.getPrototypeOf(g), {
      has: (t, k) => Reflect.has(t, k) || (typeof k === 'string' && has(k)),
      get: (t, k, r) => (typeof k !== 'string' || Reflect.has(t, k)) ? Reflect.get(t, k, r) : get(k),
    }));
    return true;
  } catch (e) {
    return false;
  }
})"#;

/// `js_set_global_lookup(env, has, get)`: engines that forbid proxies in the
/// global's prototype chain (JavaScriptCore) resolve unknown globals with these.
type GlobalLookup = unsafe extern "C" fn(napi_env, napi_value, napi_value) -> napi_status;

pub fn install(st: &State) -> JsResult<()> {
    let js = st.js();
    let init = crate::run_script(js, PROXY, "nativescript:globals")?;
    let has = js.function("has", Some(lookup_has), null_mut());
    let get = js.function("get", Some(lookup_get), null_mut());
    let installed = js.call(init, js.undefined(), &[has, get])?;
    if js.bool_of(installed) {
        return Ok(());
    }
    let hook = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"js_set_global_lookup".as_ptr()) };
    if hook.is_null() {
        js.throw("this engine can't resolve globals from metadata");
        return Err(Thrown);
    }
    let hook: GlobalLookup = unsafe { std::mem::transmute(hook) };
    ok(unsafe { hook(js.0, has, get) })
}
