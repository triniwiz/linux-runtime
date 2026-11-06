//! Global object: lazy metadata resolution, console and the JS prelude.

use crate::interop::{self, keys};
use crate::state::{state, State};
use crate::{ffi, marshal, throw_error};
use nativescript_metadata::{Meta, MetaKind};

/// Resolves unknown globals from metadata (NON_MASKING: builtins and memoized values win).
pub fn install_interceptor(template: &v8::Local<v8::ObjectTemplate>) {
    template.set_named_property_handler(
        v8::NamedPropertyHandlerConfiguration::new()
            .getter(
                |scope: &mut v8::PinScope, key: v8::Local<v8::Name>, _args: v8::PropertyCallbackArguments, mut rv: v8::ReturnValue<v8::Value>| {
                    if !key.is_string() {
                        return v8::Intercepted::kNo;
                    }
                    let st = state(scope);
                    if st.memoizing.get() {
                        return v8::Intercepted::kNo;
                    }
                    let name = key.to_rust_string_lossy(scope);
                    let Some(meta) = st.meta.find(&name) else {
                        return v8::Intercepted::kNo;
                    };
                    match resolve(scope, &st, meta) {
                        Ok(Some((value, memoize))) => {
                            if memoize {
                                let context = scope.get_current_context();
                                let global = context.global(scope);
                                st.memoizing.set(true);
                                global.define_own_property(scope, key, value, v8::PropertyAttribute::READ_ONLY);
                                st.memoizing.set(false);
                            }
                            rv.set(value);
                            v8::Intercepted::kYes
                        }
                        Ok(None) => v8::Intercepted::kNo,
                        Err(e) => {
                            throw_error(scope, &format!("{name}: {e}"));
                            v8::Intercepted::kYes
                        }
                    }
                },
            )
            .query(
                |scope: &mut v8::PinScope, key: v8::Local<v8::Name>, _args: v8::PropertyCallbackArguments, mut rv: v8::ReturnValue<v8::Integer>| {
                    if !key.is_string() {
                        return v8::Intercepted::kNo;
                    }
                    let st = state(scope);
                    if st.memoizing.get() {
                        return v8::Intercepted::kNo;
                    }
                    let name = key.to_rust_string_lossy(scope);
                    match st.meta.find(&name) {
                        Some(m) if !matches!(m.kind(), MetaKind::Interface | MetaKind::Protocol | MetaKind::Undefined) => {
                            rv.set_int32(v8::PropertyAttribute::READ_ONLY.as_u32() as i32);
                            v8::Intercepted::kYes
                        }
                        _ => v8::Intercepted::kNo,
                    }
                },
            )
            .flags(v8::PropertyHandlerFlags::NON_MASKING | v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS),
    );
}

/// JS value for a declaration, and whether it can be memoized on the global.
fn resolve<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    meta: Meta,
) -> Result<Option<(v8::Local<'s, v8::Value>, bool)>, String> {
    Ok(Some(match meta.kind() {
        MetaKind::Function => {
            // missing symbols resolve to undefined, like on iOS
            let Ok(function) = ffi::function_for(st, meta.offset()) else {
                return Ok(Some((v8::undefined(scope).into(), true)));
            };
            let data = v8::External::new(scope, std::rc::Rc::as_ptr(&function) as *mut std::ffi::c_void);
            let f = v8::Function::builder(ffi::c_function_callback)
                .data(data.into())
                .build(scope)
                .ok_or("failed to create function")?;
            let name = v8::String::new(scope, meta.js_name()).unwrap();
            f.set_name(name);
            let key = keys::function_meta(scope);
            let offset = v8::Integer::new_from_unsigned(scope, meta.offset());
            f.set_private(scope, key, offset.into());
            (f.into(), true)
        }
        // variables are read on every access, their value can change
        MetaKind::Var => {
            let ty = meta.var_type().ok_or("variable without type")?;
            let address = st.symbol(&meta)?;
            let value = unsafe { marshal::read(scope, st, &ty, address as *const u8, marshal::Mem::Live(None))? };
            (value, false)
        }
        MetaKind::JsCode => {
            let code = meta.js_code().unwrap_or("undefined");
            let source = v8::String::new(scope, code).ok_or("invalid js code")?;
            let script = v8::Script::compile(scope, source, None).ok_or("failed to compile constant")?;
            let value = script.run(scope).ok_or("failed to evaluate constant")?;
            (value, true)
        }
        MetaKind::Struct | MetaKind::Union => {
            let ty = if meta.kind() == MetaKind::Struct {
                nativescript_metadata::Type::Struct(meta.js_name().to_owned())
            } else {
                nativescript_metadata::Type::Union(meta.js_name().to_owned())
            };
            let f = interop::struct_constructor(scope, st, meta.js_name(), ty).ok_or("failed to create constructor")?;
            (f.into(), true)
        }
        MetaKind::Interface | MetaKind::Protocol | MetaKind::Undefined => return Ok(None),
    }))
}

const PRELUDES: [(&str, &str); 5] = [
    ("nativescript:prelude", include_str!("js/prelude.js")),
    ("nativescript:web", include_str!("js/web.js")),
    ("nativescript:net", include_str!("js/net.js")),
    ("nativescript:workers", include_str!("js/workers.js")),
    ("nativescript:hmr", include_str!("js/hmr.js")),
];

/// Runs the JS preludes (console, web APIs, process...).
pub fn install<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State) -> Result<(), String> {
    let argv: Vec<String> = std::env::args().collect();
    let natives = crate::natives::create(scope, &argv);
    crate::net::install(scope, natives);
    crate::workers::install(scope, st, natives);
    crate::modules::install_hmr(scope, natives);
    for (name, code) in PRELUDES {
        let source = v8::String::new(scope, code).unwrap();
        let name = v8::String::new(scope, name).unwrap();
        let origin = v8::ScriptOrigin::new(scope, name.into(), 0, 0, false, 0, None, false, false, false, None);
        let script = v8::Script::compile(scope, source, Some(&origin)).ok_or("failed to compile prelude")?;
        let init = script.run(scope).ok_or("failed to run prelude")?;
        let init = v8::Local::<v8::Function>::try_from(init).map_err(|_| "prelude must evaluate to a function")?;
        let recv = v8::undefined(scope).into();
        init.call(scope, recv, &[natives.into()]).ok_or("prelude failed")?;
    }
    Ok(())
}
