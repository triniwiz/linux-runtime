//! Lazy metadata globals: a Proxy in the global's prototype chain resolves
//! unknown names; resolved values are memoized as own global properties.

use crate::js::*;
use crate::marshal::{self, Mem};
use crate::state::{state, State};
use crate::sys::*;
use crate::{ffi, interop};
use nativescript_metadata::{Meta, MetaKind, Type};
use std::ptr::null_mut;

/// JS value for a declaration, and whether it can be memoized on the global.
fn resolve(st: &State, meta: Meta) -> Result<Option<(napi_value, bool)>, String> {
    let js = st.js();
    Ok(Some(match meta.kind() {
        MetaKind::Function => match ffi::function_for(st, meta.offset()) {
            Ok(function) => (ffi::js_function(st, meta.js_name(), &function), true),
            // missing symbols resolve to undefined, like on iOS
            Err(_) => (js.undefined(), true),
        },
        // variables are read on every access, their value can change
        MetaKind::Var => {
            let ty = meta.var_type().ok_or("variable without type")?;
            let address = st.symbol(&meta)?;
            (unsafe { marshal::read(st, &ty, address as *const u8, Mem::Live(js.null()))? }, false)
        }
        MetaKind::JsCode => {
            let code = meta.js_code().unwrap_or("undefined");
            (crate::run_script(js, code, meta.js_name()).map_err(|_| "failed to evaluate constant")?, true)
        }
        MetaKind::Struct => (interop::struct_constructor(st, meta.js_name(), Type::Struct(meta.js_name().to_owned())), true),
        MetaKind::Union => (interop::struct_constructor(st, meta.js_name(), Type::Union(meta.js_name().to_owned())), true),
        MetaKind::Interface | MetaKind::Protocol | MetaKind::Undefined => return Ok(None),
    }))
}

fn is_global(kind: MetaKind) -> bool {
    !matches!(kind, MetaKind::Interface | MetaKind::Protocol | MetaKind::Undefined)
}

unsafe extern "C" fn lookup_has(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (a, ..) = cb_args::<1>(env, info);
    let name = st.js().rust_string(a[0]).unwrap_or_default();
    st.js().boolean(st.meta.find(&name).is_some_and(|m| is_global(m.kind())))
}

unsafe extern "C" fn lookup_get(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (a, ..) = cb_args::<1>(env, info);
    let Ok(name) = js.rust_string(a[0]) else { return js.undefined() };
    let Some(meta) = st.meta.find(&name) else { return js.undefined() };
    match resolve(st, meta) {
        Ok(Some((value, memoize))) => {
            if memoize {
                let key = js.string(&name);
                let desc = napi_property_descriptor {
                    utf8name: null_mut(),
                    name: key,
                    method: None,
                    getter: None,
                    setter: None,
                    value,
                    // configurable, not writable / enumerable
                    attributes: 4,
                    data: null_mut(),
                };
                napi_define_properties(env, js.global(), 1, &desc);
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

const PROXY: &str = r#"(function (has, get) {
  const g = globalThis;
  const proto = Object.getPrototypeOf(g);
  Object.setPrototypeOf(g, new Proxy(proto, {
    has: (t, k) => Reflect.has(t, k) || (typeof k === 'string' && has(k)),
    get: (t, k, r) => (typeof k !== 'string' || Reflect.has(t, k)) ? Reflect.get(t, k, r) : get(k),
  }));
})"#;

pub fn install(st: &State) -> JsResult<()> {
    let js = st.js();
    let init = crate::run_script(js, PROXY, "nativescript:globals")?;
    let has = js.function("has", Some(lookup_has), null_mut());
    let get = js.function("get", Some(lookup_get), null_mut());
    js.call(init, js.undefined(), &[has, get])?;
    Ok(())
}
