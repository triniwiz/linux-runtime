use crate::js::*;
use crate::sys::*;
use nativescript_core::subtle::{dispatch, Js as Engine, Key};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::{c_void, CString};
use std::ptr::null_mut;

thread_local! {
    /// live wrapped keys, so other wrapped objects aren't read as keys
    static KEYS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

unsafe extern "C" fn finalize_key(_env: napi_env, data: *mut c_void, _hint: *mut c_void) {
    KEYS.with(|k| k.borrow_mut().remove(&(data as usize)));
    drop(Box::from_raw(data as *mut Key));
}

struct Napi {
    js: Js,
    proto: napi_value,
}

impl Napi {
    fn json_call(&self, method: &str, arg: napi_value) -> Option<napi_value> {
        let json = self.js.get(self.js.global(), "JSON").ok()?;
        let f = self.js.get(json, method).ok()?;
        let out = self.js.call(f, json, &[arg]);
        if out.is_err() {
            self.js.clear_exception();
        }
        out.ok()
    }
}

impl Engine for Napi {
    type V = napi_value;

    fn undefined(&mut self) -> napi_value {
        self.js.undefined()
    }

    fn is_nullish(&mut self, v: napi_value) -> bool {
        matches!(self.js.type_of(v), UNDEFINED | NULL)
    }

    fn string(&mut self, v: napi_value) -> Option<String> {
        (self.js.type_of(v) == STRING).then(|| self.js.to_string(v))
    }

    fn number(&mut self, v: napi_value) -> Option<f64> {
        (self.js.type_of(v) == NUMBER).then(|| self.js.f64_of(v).ok()).flatten()
    }

    fn boolean(&mut self, v: napi_value) -> bool {
        self.js.bool_of(v)
    }

    fn bytes(&mut self, v: napi_value) -> Option<Vec<u8>> {
        let (p, len) = self.js.buffer_of(v)?;
        Some(if len == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(p, len).to_vec() } })
    }

    fn get(&mut self, v: napi_value, name: &str) -> napi_value {
        if !matches!(self.js.type_of(v), OBJECT | FUNCTION) {
            return self.js.undefined();
        }
        self.js.get(v, name).unwrap_or_else(|_| {
            self.js.clear_exception();
            self.js.undefined()
        })
    }

    fn array(&mut self, v: napi_value) -> Option<Vec<napi_value>> {
        let mut is = false;
        let mut len = 0u32;
        unsafe {
            napi_is_array(self.js.0, v, &mut is);
            if !is {
                return None;
            }
            napi_get_array_length(self.js.0, v, &mut len);
        }
        (0..len)
            .map(|i| {
                let mut item = null_mut();
                (unsafe { napi_get_element(self.js.0, v, i, &mut item) } == 0).then_some(item)
            })
            .collect()
    }

    fn stringify(&mut self, v: napi_value) -> Option<String> {
        if self.js.type_of(v) != OBJECT {
            return None;
        }
        let text = self.json_call("stringify", v)?;
        self.string(text)
    }

    fn key(&mut self, v: napi_value) -> Option<Key> {
        if self.js.type_of(v) != OBJECT {
            return None;
        }
        let mut data = null_mut();
        if unsafe { napi_unwrap(self.js.0, v, &mut data) } != 0 {
            self.js.clear_exception();
            return None;
        }
        KEYS.with(|k| k.borrow().contains(&(data as usize)))
            .then(|| unsafe { (*(data as *const Key)).clone() })
    }

    fn array_buffer(&mut self, bytes: Vec<u8>) -> napi_value {
        let (v, p) = self.js.arraybuffer(bytes.len());
        if !bytes.is_empty() {
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len()) };
        }
        v
    }

    fn bool_value(&mut self, b: bool) -> napi_value {
        self.js.boolean(b)
    }

    fn json(&mut self, value: &Value) -> napi_value {
        let text = self.js.string(&value.to_string());
        self.json_call("parse", text).unwrap_or_else(|| self.js.undefined())
    }

    fn crypto_key(&mut self, key: Key) -> napi_value {
        let js = self.js;
        let object = js
            .get(js.global(), "Object")
            .and_then(|o| js.get(o, "create").and_then(|create| js.call(create, o, &[self.proto])))
            .unwrap_or_else(|_| {
                js.clear_exception();
                js.object()
            });
        let usages: Vec<napi_value> = key.usages.iter().map(|u| js.string(u)).collect();
        let values = [
            ("type", js.string(key.kind())),
            ("extractable", js.boolean(key.extractable)),
            ("algorithm", self.json(&key.algorithm.to_json())),
            ("usages", js.array(&usages)),
        ];
        let names: Vec<CString> = values.iter().map(|(n, _)| CString::new(*n).unwrap()).collect();
        let descriptors: Vec<napi_property_descriptor> = values
            .iter()
            .zip(&names)
            .map(|((_, value), name)| napi_property_descriptor {
                utf8name: name.as_ptr(),
                name: null_mut(),
                method: None,
                getter: None,
                setter: None,
                value: *value,
                attributes: 2, // napi_enumerable
                data: null_mut(),
            })
            .collect();
        let data = Box::into_raw(Box::new(key)) as *mut c_void;
        KEYS.with(|k| k.borrow_mut().insert(data as usize));
        unsafe {
            napi_define_properties(js.0, object, descriptors.len(), descriptors.as_ptr());
            napi_wrap(js.0, object, data, Some(finalize_key), null_mut(), null_mut());
        }
        object
    }
}

fn throw(js: Js, name: &str, message: &str) {
    if name == "TypeError" {
        return js.throw_type(message);
    }
    let exception = js
        .get(js.global(), "DOMException")
        .and_then(|ctor| js.new_instance(ctor, &[js.string(message), js.string(name)]));
    match exception {
        Ok(e) => unsafe {
            napi_throw(js.0, e);
        },
        Err(_) => {
            js.clear_exception();
            js.throw(&format!("{name}: {message}"));
        }
    }
}

/// subtle(op, args, CryptoKey.prototype)
pub unsafe extern "C" fn subtle(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<3>(env, info);
    let op = js.to_string(a[0]);
    let mut engine = Napi { js, proto: a[2] };
    let args = engine.array(a[1]).unwrap_or_default();
    match dispatch(&mut engine, &op, &args) {
        Ok(v) => v,
        Err(e) => {
            throw(js, e.name, &e.message);
            null_mut()
        }
    }
}
