//! The `interop` global.

use crate::js::*;
use crate::marshal::{self, CallCx, Mem};
use crate::state::{attach, handle, handle_of, state, Handle, Kind, State};
use crate::sys::*;
use crate::{ffi, structs};
use nativescript_metadata::Type;
use std::ffi::{c_void, CStr};
use std::ptr::null_mut;

type Cb = unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value;

unsafe fn args(
    env: napi_env,
    info: napi_callback_info,
) -> ([napi_value; 3], usize, napi_value, *mut c_void) {
    cb_args::<3>(env, info)
}

fn fail(js: Js, e: &str) -> napi_value {
    if !js.is_exception_pending() {
        js.throw(e);
    }
    null_mut()
}

pub fn new_pointer(st: &State, address: usize, owner: Option<napi_value>) -> napi_value {
    let js = st.js();
    let ctor = js.deref(st.pointer_ctor.get());
    let object = js.new_instance(ctor, &[]).unwrap_or(null_mut());
    if let Some(h) = handle_of(js, object, Kind::Pointer) {
        h.address = address;
        if let Some(owner) = owner {
            h.owner = js.reference(owner, 1);
        }
    }
    object
}

pub fn tag_function(st: &State, f: napi_value, address: usize) {
    attach(
        st.js(),
        f,
        Handle::new(Kind::Function, address, None, null_mut()),
    );
}

pub fn native_function_address(st: &State, f: napi_value) -> Option<usize> {
    handle_of(st.js(), f, Kind::Function).map(|h| h.address)
}

pub fn tag_type(st: &State, object: napi_value, ty: &Type) {
    attach(
        st.js(),
        object,
        Handle::new(Kind::Type, 0, Some(st.register_type(ty)), null_mut()),
    );
}

/// Type of an `interop.types.*` value, struct constructor or struct view.
pub fn type_from_value(st: &State, v: napi_value) -> Option<Type> {
    let h = handle(st.js(), v).filter(|h| matches!(h.kind, Kind::Type | Kind::Struct))?;
    st.type_at(h.type_index?)
}

fn address_from_value(st: &State, v: napi_value) -> Result<usize, String> {
    let js = st.js();
    match js.type_of(v) {
        UNDEFINED | NULL => Ok(0),
        NUMBER => Ok(js.f64_of(v).unwrap_or(0.0) as usize),
        BIGINT => Ok(js.bigint_u64_of(v).unwrap_or(0) as usize),
        _ => handle_of(js, v, Kind::Pointer)
            .map(|h| h.address)
            .ok_or_else(|| "expected a number, bigint or Pointer".into()),
    }
}

unsafe extern "C" fn pointer_ctor(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (argv, _, this, _) = args(env, info);
    match address_from_value(st, argv[0]) {
        Ok(address) => {
            attach(
                st.js(),
                this,
                Handle::new(Kind::Pointer, address, None, null_mut()),
            );
            this
        }
        Err(e) => fail(st.js(), &e),
    }
}

unsafe fn this_pointer<'a>(
    env: napi_env,
    info: napi_callback_info,
) -> Option<(&'a mut Handle, [napi_value; 3])> {
    let (argv, _, this, _) = args(env, info);
    match handle_of(Js(env), this, Kind::Pointer) {
        Some(h) => Some((h, argv)),
        None => {
            Js(env).throw("not an interop.Pointer");
            None
        }
    }
}

unsafe fn offset(env: napi_env, info: napi_callback_info, sign: i64) -> napi_value {
    let st = state(env);
    let Some((h, argv)) = this_pointer(env, info) else {
        return null_mut();
    };
    let delta = marshal::to_i64(st.js(), argv[0], &Type::Long).unwrap_or(0);
    let owner = (!h.owner.is_null()).then(|| st.js().deref(h.owner));
    new_pointer(
        st,
        (h.address as i64).wrapping_add(sign * delta) as usize,
        owner,
    )
}

unsafe extern "C" fn pointer_add(env: napi_env, info: napi_callback_info) -> napi_value {
    offset(env, info, 1)
}
unsafe extern "C" fn pointer_subtract(env: napi_env, info: napi_callback_info) -> napi_value {
    offset(env, info, -1)
}
unsafe extern "C" fn pointer_to_number(env: napi_env, info: napi_callback_info) -> napi_value {
    this_pointer(env, info)
        .map(|(h, _)| Js(env).number(h.address as f64))
        .unwrap_or(null_mut())
}
unsafe extern "C" fn pointer_to_bigint(env: napi_env, info: napi_callback_info) -> napi_value {
    this_pointer(env, info)
        .map(|(h, _)| Js(env).bigint_u64(h.address as u64))
        .unwrap_or(null_mut())
}
unsafe extern "C" fn pointer_to_hex(env: napi_env, info: napi_callback_info) -> napi_value {
    this_pointer(env, info)
        .map(|(h, _)| Js(env).string(&format!("{:#x}", h.address)))
        .unwrap_or(null_mut())
}
unsafe extern "C" fn pointer_to_string(env: napi_env, info: napi_callback_info) -> napi_value {
    this_pointer(env, info)
        .map(|(h, _)| Js(env).string(&format!("<Pointer: {:#x}>", h.address)))
        .unwrap_or(null_mut())
}
unsafe extern "C" fn pointer_equals(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let Some((h, argv)) = this_pointer(env, info) else {
        return null_mut();
    };
    st.js()
        .boolean(address_from_value(st, argv[0]).ok() == Some(h.address))
}

fn method(name: &'static CStr, cb: Cb) -> napi_property_descriptor {
    napi_property_descriptor {
        utf8name: name.as_ptr(),
        name: null_mut(),
        method: Some(cb),
        getter: None,
        setter: None,
        value: null_mut(),
        attributes: 1 | 4,
        data: null_mut(),
    }
}

fn define_class(js: Js, name: &str, ctor: Cb, props: &[napi_property_descriptor]) -> napi_value {
    let mut class = null_mut();
    unsafe {
        napi_define_class(
            js.0,
            name.as_ptr() as *const _,
            name.len(),
            Some(ctor),
            null_mut(),
            props.len(),
            props.as_ptr(),
            &mut class,
        )
    };
    class
}

pub fn reference_type(st: &State, h: &Handle) -> Option<Type> {
    st.type_at(h.type_index?)
}

fn allocate_storage(st: &State, h: &mut Handle, ty: &Type) -> Result<(), String> {
    let js = st.js();
    let size = st.layout(ty)?.size.max(1);
    let (buffer, data) = js.arraybuffer(size);
    h.type_index = Some(st.register_type(ty));
    h.address = data as usize;
    js.delete_ref(h.owner);
    h.owner = js.reference(buffer, 1);
    Ok(())
}

fn reference_write(st: &State, h: &mut Handle, value: napi_value) -> Result<(), String> {
    let js = st.js();
    if h.address == 0 {
        js.delete_ref(h.pending);
        h.pending = js.reference(value, 1);
        return Ok(());
    }
    let ty = reference_type(st, h).ok_or("reference without type")?;
    if matches!(ty, Type::CString | Type::ConstCString) && js.type_of(value) == STRING {
        let s = js.rust_string(value).unwrap_or_default();
        let (buffer, data) = js.arraybuffer(s.len() + 1);
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), data, s.len());
            (h.address as *mut usize).write_unaligned(data as usize);
        }
        h.keep.push(js.reference(buffer, 1));
        return Ok(());
    }
    let mut cx = CallCx::default();
    unsafe { marshal::write(st, &ty, value, h.address as *mut u8, &mut cx) }
}

/// Storage of a Reference passed for a `pointee*` argument, allocated on first use.
pub fn reference_ensure_storage(
    st: &State,
    _value: napi_value,
    h: &mut Handle,
    pointee: &Type,
) -> Result<usize, String> {
    if h.address != 0 {
        return Ok(h.address);
    }
    let ty = match reference_type(st, h) {
        Some(t) => t,
        None if !pointee.is_void() => pointee.clone(),
        None => return Err("a Reference passed as void* needs a type: new interop.Reference(interop.types.int32, 0)".into()),
    };
    allocate_storage(st, h, &ty)?;
    if !h.pending.is_null() {
        let pending = st.js().deref(h.pending);
        reference_write(st, h, pending)?;
    }
    Ok(h.address)
}

unsafe extern "C" fn reference_ctor(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, argc, this, _) = args(env, info);
    let h = &mut *attach(js, this, Handle::new(Kind::Reference, 0, None, null_mut()));
    let result = match type_from_value(st, argv[0]) {
        Some(ty) if handle_of(js, argv[1], Kind::Pointer).is_some() => {
            let p = handle_of(js, argv[1], Kind::Pointer).unwrap();
            h.address = p.address;
            h.type_index = Some(st.register_type(&ty));
            Ok(())
        }
        Some(ty) => allocate_storage(st, h, &ty).and_then(|_| {
            if argc > 1 {
                reference_write(st, h, argv[1])
            } else {
                Ok(())
            }
        }),
        None => {
            h.pending = js.reference(argv[0], 1);
            Ok(())
        }
    };
    match result {
        Ok(()) => this,
        Err(e) => fail(js, &e),
    }
}

unsafe extern "C" fn reference_get(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (_, _, this, _) = args(env, info);
    let Some(h) = handle_of(js, this, Kind::Reference) else {
        return fail(js, "not a Reference");
    };
    if h.address == 0 {
        return if h.pending.is_null() {
            js.undefined()
        } else {
            js.deref(h.pending)
        };
    }
    let Some(ty) = reference_type(st, h) else {
        return js.undefined();
    };
    marshal::read(st, &ty, h.address as *const u8, Mem::Live(this)).unwrap_or_else(|e| fail(js, &e))
}

unsafe extern "C" fn reference_set(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, this, _) = args(env, info);
    let Some(h) = handle_of(js, this, Kind::Reference) else {
        return fail(js, "not a Reference");
    };
    if let Err(e) = reference_write(st, h, argv[0]) {
        return fail(js, &e);
    }
    null_mut()
}

unsafe extern "C" fn struct_ctor(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, data) = args(env, info);
    let Some(ty) = st.type_at(data as u32) else {
        return null_mut();
    };
    let result = (|| {
        let size = st.layout(&ty)?.size;
        let (buffer, mem) = js.arraybuffer(size.max(1));
        if js.type_of(argv[0]) != UNDEFINED {
            let mut cx = CallCx::default();
            marshal::write(st, &ty, argv[0], mem, &mut cx)?;
        }
        structs::view(st, &ty, mem as usize, Some(buffer))
    })();
    result.unwrap_or_else(|e| fail(js, &e))
}

/// Constructor for struct / union globals: `new tm({ tm_year: 100 })`.
pub fn struct_constructor(st: &State, name: &str, ty: Type) -> napi_value {
    let index = st.register_type(&ty);
    let f = st
        .js()
        .function(name, Some(struct_ctor), index as usize as *mut c_void);
    tag_type(st, f, &ty);
    f
}

unsafe extern "C" fn interop_alloc(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (argv, _, _, _) = args(env, info);
    let size = st.js().f64_of(argv[0]).unwrap_or(0.0).max(1.0) as usize;
    let (buffer, data) = st.js().arraybuffer(size);
    new_pointer(st, data as usize, Some(buffer))
}

unsafe extern "C" fn interop_free(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (argv, _, _, _) = args(env, info);
    match handle_of(js, argv[0], Kind::Pointer) {
        // memory from interop.alloc is garbage collected
        Some(h) if h.owner.is_null() => libc::free(h.address as *mut c_void),
        Some(_) => {}
        None => return fail(js, "interop.free expects a Pointer"),
    }
    null_mut()
}

unsafe extern "C" fn interop_sizeof(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = args(env, info);
    let v = argv[0];
    let size = if let Some(ty) = type_from_value(st, v) {
        match st.layout(&ty) {
            Ok(l) => l.size,
            Err(e) => return fail(js, &e),
        }
    } else if handle(js, v).is_some() || matches!(js.type_of(v), FUNCTION | STRING) {
        std::mem::size_of::<usize>()
    } else {
        return fail(js, "interop.sizeof expects a type, struct or Pointer");
    };
    js.number(size as f64)
}

unsafe extern "C" fn interop_handleof(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = args(env, info);
    let v = argv[0];
    if let Some(h) = handle(js, v) {
        return match h.kind {
            Kind::Pointer => v,
            Kind::Reference | Kind::Struct | Kind::Function if h.address != 0 => {
                new_pointer(st, h.address, Some(v))
            }
            _ => js.null(),
        };
    }
    match js.buffer_of(v) {
        Some((p, _)) => new_pointer(st, p as usize, Some(v)),
        None => js.null(),
    }
}

unsafe extern "C" fn interop_string_from_cstring(
    env: napi_env,
    info: napi_callback_info,
) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, argc, _, _) = args(env, info);
    if js.type_of(argv[0]) == STRING {
        return argv[0];
    }
    let addr = match address_from_value(st, argv[0]) {
        Ok(a) => a,
        Err(e) => return fail(js, &e),
    };
    if addr == 0 {
        return js.null();
    }
    let bytes: &[u8] = if argc > 1 && js.type_of(argv[1]) == NUMBER {
        std::slice::from_raw_parts(
            addr as *const u8,
            js.f64_of(argv[1]).unwrap_or(0.0) as usize,
        )
    } else {
        CStr::from_ptr(addr as *const libc::c_char).to_bytes()
    };
    js.string(&String::from_utf8_lossy(bytes))
}

unsafe extern "C" fn release_external(_: napi_env, data: *mut c_void, free: *mut c_void) {
    if !free.is_null() {
        let free: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(free);
        free(data);
    }
}

/// `interop.bufferFromData(ptr, length, free?)`: zero-copy ArrayBuffer; `free` (e.g. g_free) runs on GC.
unsafe extern "C" fn interop_buffer_from_data(
    env: napi_env,
    info: napi_callback_info,
) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = args(env, info);
    let Some(h) = handle_of(js, argv[0], Kind::Pointer) else {
        return fail(js, "interop.bufferFromData expects a Pointer and a length");
    };
    let len = js.f64_of(argv[1]).unwrap_or(0.0) as usize;
    let free = if js.type_of(argv[2]) == FUNCTION {
        match native_function_address(st, argv[2]) {
            Some(a) => a,
            None => {
                return fail(
                    js,
                    "the free function must be a native function such as free or g_free",
                )
            }
        }
    } else {
        0
    };
    let mut result = null_mut();
    napi_create_external_arraybuffer(
        env,
        h.address as *mut c_void,
        len,
        Some(release_external),
        free as *mut c_void,
        &mut result,
    );
    result
}

unsafe extern "C" fn interop_callback(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = args(env, info);
    if js.type_of(argv[0]) != FUNCTION {
        return fail(js, "interop.callback expects a function");
    }
    let result = (|| -> Result<usize, String> {
        let returns = js
            .get(argv[1], "returns")
            .map_err(|_| "expected { returns, args }")?;
        let ret = if js.type_of(returns) == UNDEFINED {
            Type::Void
        } else {
            type_from_value(st, returns).ok_or("invalid return type")?
        };
        let mut signature = vec![ret];
        let list = js
            .get(argv[1], "args")
            .map_err(|_| "expected { returns, args }")?;
        let mut len = 0;
        napi_get_array_length(env, list, &mut len);
        for i in 0..len {
            let mut t = null_mut();
            napi_get_element(env, list, i, &mut t);
            signature.push(
                type_from_value(st, t)
                    .ok_or_else(|| format!("invalid type for argument {}", i + 1))?,
            );
        }
        ffi::closure_for(st, argv[0], &signature)
    })();
    match result {
        Ok(addr) => new_pointer(st, addr, Some(argv[0])),
        Err(e) => fail(js, &e),
    }
}

unsafe extern "C" fn interop_cstring(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = args(env, info);
    let s = js.to_string(argv[0]);
    let (buffer, data) = js.arraybuffer(s.len() + 1);
    std::ptr::copy_nonoverlapping(s.as_ptr(), data, s.len());
    new_pointer(st, data as usize, Some(buffer))
}

unsafe extern "C" fn interop_release(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (argv, _, _, _) = args(env, info);
    st.js().int(ffi::release_closures(st, argv[0]) as i32)
}

pub fn install(st: &State, global: napi_value) {
    let js = st.js();
    let interop = js.object();
    let pointer = define_class(
        js,
        "Pointer",
        pointer_ctor,
        &[
            method(c"add", pointer_add),
            method(c"subtract", pointer_subtract),
            method(c"toNumber", pointer_to_number),
            method(c"toBigInt", pointer_to_bigint),
            method(c"toHexString", pointer_to_hex),
            method(c"toString", pointer_to_string),
            method(c"equals", pointer_equals),
        ],
    );
    st.pointer_ctor.set(js.reference(pointer, 1));
    js.set(interop, "Pointer", pointer);
    let value = napi_property_descriptor {
        utf8name: c"value".as_ptr(),
        name: null_mut(),
        method: None,
        getter: Some(reference_get),
        setter: Some(reference_set),
        value: null_mut(),
        attributes: 4,
        data: null_mut(),
    };
    let reference = define_class(js, "Reference", reference_ctor, &[value]);
    st.reference_ctor.set(js.reference(reference, 1));
    js.set(interop, "Reference", reference);

    let types = js.object();
    for (name, ty) in [
        ("void", Type::Void),
        ("bool", Type::Bool),
        ("int8", Type::Char),
        ("uint8", Type::UChar),
        ("int16", Type::Short),
        ("uint16", Type::UShort),
        ("int32", Type::Int),
        ("uint32", Type::UInt),
        ("int64", Type::LongLong),
        ("uint64", Type::ULongLong),
        ("long", Type::Long),
        ("ulong", Type::ULong),
        ("float", Type::Float),
        ("double", Type::Double),
        ("longdouble", Type::LongDouble),
        ("unichar", Type::Unichar),
        ("UTF8CString", Type::CString),
        ("pointer", Type::Pointer(Box::new(Type::Void))),
    ] {
        let d = js.object();
        js.set(d, "name", js.string(name));
        tag_type(st, d, &ty);
        js.set(types, name, d);
    }
    js.set(interop, "types", types);
    for (name, cb) in [
        ("alloc", interop_alloc as Cb),
        ("free", interop_free),
        ("sizeof", interop_sizeof),
        ("handleof", interop_handleof),
        ("stringFromCString", interop_string_from_cstring),
        ("bufferFromData", interop_buffer_from_data),
        ("callback", interop_callback),
        ("cstring", interop_cstring),
        ("release", interop_release),
    ] {
        js.set(interop, name, js.function(name, Some(cb), null_mut()));
    }
    js.set(global, "interop", interop);
}
