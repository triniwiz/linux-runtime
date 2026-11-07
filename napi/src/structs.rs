//! Live struct / union views: accessors read and write the C memory, an owner keeps it alive.

use crate::js::*;
use crate::marshal::{self, CallCx, Mem};
use crate::state::{attach, handle_of, state, Handle, Kind, State};
use crate::sys::*;
use nativescript_metadata::Type;
use std::ffi::{c_void, CString};
use std::ptr::null_mut;

unsafe fn this_and_data(
    env: napi_env,
    info: napi_callback_info,
    arg: &mut napi_value,
) -> (napi_value, usize) {
    let mut argc = 1;
    let mut this = null_mut();
    let mut data = null_mut();
    napi_get_cb_info(env, info, &mut argc, arg, &mut this, &mut data);
    (this, data as usize)
}

fn view_of<'a>(st: &State, this: napi_value) -> Option<(&'a mut Handle, Type)> {
    let h = handle_of(st.js(), this, Kind::Struct)?;
    let ty = st.type_at(h.type_index?)?;
    Some((h, ty))
}

unsafe extern "C" fn field_get(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let mut arg = null_mut();
    let (this, index) = this_and_data(env, info, &mut arg);
    let Some((h, ty)) = view_of(st, this) else {
        st.js().throw("not a struct");
        return null_mut();
    };
    let result = st.record(&ty).and_then(|record| {
        let field = record.fields.get(index).ok_or("unknown field")?;
        marshal::read_field(st, field, h.address as *const u8, Mem::Live(this))
    });
    result.unwrap_or_else(|e| {
        st.js().throw(&e);
        null_mut()
    })
}

unsafe extern "C" fn field_set(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let mut value = null_mut();
    let (this, index) = this_and_data(env, info, &mut value);
    let Some((h, ty)) = view_of(st, this) else {
        js.throw("not a struct");
        return null_mut();
    };
    let result = st.record(&ty).and_then(|record| {
        let field = record.fields.get(index).ok_or("unknown field")?;
        // strings stored in char* fields live as long as the struct
        if matches!(field.ty, Type::CString | Type::ConstCString) && js.type_of(value) == STRING {
            let s = js.rust_string(value).unwrap_or_default();
            let (buffer, data) = js.arraybuffer(s.len() + 1);
            std::ptr::copy_nonoverlapping(s.as_ptr(), data, s.len());
            ((h.address + field.offset) as *mut usize).write_unaligned(data as usize);
            h.keep.push(js.reference(buffer, 1));
            return Ok(());
        }
        let mut cx = CallCx::default();
        marshal::write_field(st, field, value, h.address as *mut u8, &mut cx)
    });
    if let Err(e) = result {
        js.throw(&e);
    }
    null_mut()
}

unsafe extern "C" fn to_json(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let mut arg = null_mut();
    let (this, _) = this_and_data(env, info, &mut arg);
    let Some((h, ty)) = view_of(st, this) else {
        return null_mut();
    };
    marshal::record_to_object(st, &ty, h.address as *const u8).unwrap_or_else(|e| {
        st.js().throw(&e);
        null_mut()
    })
}

unsafe extern "C" fn view_ctor(env: napi_env, info: napi_callback_info) -> napi_value {
    let mut this = null_mut();
    napi_get_cb_info(env, info, null_mut(), null_mut(), &mut this, null_mut());
    this
}

fn type_name(ty: &Type) -> &str {
    match ty {
        Type::Struct(n) | Type::Union(n) => n,
        Type::AnonymousUnion(..) => "union",
        _ => "struct",
    }
}

fn class(st: &State, ty: &Type, index: u32) -> Result<napi_value, String> {
    let js = st.js();
    if let Some(r) = st.struct_classes.borrow().get(&index) {
        return Ok(js.deref(*r));
    }
    let record = st.record(ty)?;
    let names: Vec<CString> = record
        .fields
        .iter()
        .map(|f| CString::new(f.name.as_str()).unwrap_or_default())
        .collect();
    let mut props: Vec<napi_property_descriptor> = record
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.name.is_empty())
        .map(|(i, _)| napi_property_descriptor {
            utf8name: names[i].as_ptr(),
            name: null_mut(),
            method: None,
            getter: Some(field_get),
            setter: Some(field_set),
            value: null_mut(),
            attributes: 2 | 4,
            data: i as *mut c_void,
        })
        .collect();
    props.push(napi_property_descriptor {
        utf8name: c"toJSON".as_ptr(),
        name: null_mut(),
        method: Some(to_json),
        getter: None,
        setter: None,
        value: null_mut(),
        attributes: 1 | 4,
        data: null_mut(),
    });
    let name = type_name(ty);
    let mut ctor = null_mut();
    unsafe {
        napi_define_class(
            js.0,
            name.as_ptr() as *const _,
            name.len(),
            Some(view_ctor),
            null_mut(),
            props.len(),
            props.as_ptr(),
            &mut ctor,
        )
    };
    st.struct_classes
        .borrow_mut()
        .insert(index, js.reference(ctor, 1));
    Ok(ctor)
}

/// Live view of the record `ty` at `addr`.
pub fn view(
    st: &State,
    ty: &Type,
    addr: usize,
    owner: Option<napi_value>,
) -> Result<napi_value, String> {
    let js = st.js();
    let index = st.register_type(ty);
    let ctor = class(st, ty, index)?;
    let object = js
        .new_instance(ctor, &[])
        .map_err(|_| "failed to create struct view")?;
    // static memory (C globals) has no owner
    let owner = owner
        .filter(|o| matches!(js.type_of(*o), OBJECT | FUNCTION))
        .map(|o| js.reference(o, 1))
        .unwrap_or(null_mut());
    attach(
        js,
        object,
        Handle::new(Kind::Struct, addr, Some(index), owner),
    );
    Ok(object)
}

/// View over a GC managed copy of the record at `src` (or zeroed memory).
pub fn copy(st: &State, ty: &Type, src: *const u8) -> Result<napi_value, String> {
    let size = st.layout(ty)?.size;
    let (buffer, data) = st.js().arraybuffer(size.max(1));
    if !src.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(src, data, size) };
    }
    view(st, ty, data as usize, Some(buffer))
}
