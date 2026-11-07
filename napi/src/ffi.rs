//! C calls from JS and libffi closures calling JS.

use crate::js::*;
use crate::marshal::{self, CallCx, Mem};
use crate::state::{state, State};
use crate::sys::*;
use libffi::raw;
use nativescript_core::call::{CallDesc, FastCall};
use nativescript_core::closure::Closure;
use nativescript_metadata::{MetaId, Type};
use std::ffi::c_void;
use std::ptr::null_mut;
use std::rc::Rc;

/// A C function bound to a runtime.
pub struct CFunction {
    desc: CallDesc,
    state: *const State,
}

impl std::ops::Deref for CFunction {
    type Target = CallDesc;
    fn deref(&self) -> &CallDesc {
        &self.desc
    }
}

impl CFunction {
    pub fn new(
        st: &State,
        name: &str,
        address: usize,
        signature: Vec<Type>,
        variadic: bool,
    ) -> Result<Self, String> {
        Ok(CFunction {
            desc: CallDesc::new(st, name, address, signature, variadic)?,
            state: st,
        })
    }

    fn vararg_type(js: Js, v: napi_value) -> Result<Type, String> {
        Ok(match js.type_of(v) {
            BOOLEAN => Type::Int,
            NUMBER => {
                let n = js.f64_of(v).unwrap_or(0.0);
                if n.fract() == 0.0 && n.abs() <= i32::MAX as f64 {
                    Type::Int
                } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                    Type::LongLong
                } else {
                    Type::Double
                }
            }
            BIGINT => Type::LongLong,
            STRING => Type::CString,
            FUNCTION => return Err("functions can't be passed as variadic arguments".into()),
            _ => Type::Pointer(Box::new(Type::Void)),
        })
    }

    pub fn call(&self, st: &State, args: &[napi_value]) -> Result<napi_value, String> {
        let fixed = self.signature.len() - 1;
        let argc = args.len();
        if argc < fixed || (!self.variadic && argc > fixed) {
            return Err(format!(
                "{}() expects {}{} argument(s), got {}",
                self.name,
                fixed,
                if self.variadic { "+" } else { "" },
                argc
            ));
        }
        if let Some(fast) = &self.fast {
            return self.call_fast(st, args, fast);
        }
        let js = st.js();
        let variadic_sig;
        let variadic_cif;
        let (signature, cif) = if self.variadic {
            let mut sig = self.signature.clone();
            for v in &args[fixed..] {
                sig.push(Self::vararg_type(js, *v)?);
            }
            variadic_sig = sig;
            variadic_cif = CallDesc::prep(st, &variadic_sig, Some(fixed))?;
            (&variadic_sig[..], &variadic_cif)
        } else {
            (&self.signature[..], self.cif().unwrap())
        };
        let mut cx = CallCx::default();
        let mut argv_inline = [null_mut::<c_void>(); 16];
        let mut argv_heap = Vec::new();
        let argv: &mut [*mut c_void] = if argc <= 16 {
            &mut argv_inline[..argc]
        } else {
            argv_heap.resize(argc, null_mut());
            &mut argv_heap
        };
        for (i, ty) in signature[1..].iter().enumerate() {
            let size = match self.slots.get(i) {
                Some(s) => *s,
                None => st.layout(ty)?.size.max(8),
            };
            let slot = cx.scratch(size);
            unsafe { marshal::write(st, ty, args[i], slot, &mut cx) }
                .map_err(|e| format!("{}() argument {}: {e}", self.name, i + 1))?;
            argv[i] = slot as *mut c_void;
        }
        let ret_ty = &signature[0];
        let ret = cx.scratch(if self.variadic {
            st.layout(ret_ty)?.size.max(16)
        } else {
            self.ret_size
        });
        unsafe { self.desc.call(cif, argv, ret) };
        let result = unsafe { marshal::read(st, ret_ty, ret, Mem::Temp)? };
        cx.finish(st)?;
        Ok(result)
    }

    fn call_fast(
        &self,
        st: &State,
        args: &[napi_value],
        fast: &FastCall,
    ) -> Result<napi_value, String> {
        let mut cx = CallCx::default();
        let mut ints = [0i64; 8];
        let mut floats = [0f64; 8];
        let (mut ni, mut nf) = (0, 0);
        for (i, ty) in self.signature[1..].iter().enumerate() {
            let mut slot = 0u128;
            unsafe { marshal::write(st, ty, args[i], &mut slot as *mut u128 as *mut u8, &mut cx) }
                .map_err(|e| format!("{}() argument {}: {e}", self.name, i + 1))?;
            if fast.float_args[i] {
                floats[nf] = f64::from_bits(slot as u64);
                nf += 1;
            } else {
                ints[ni] = slot as u64 as i64;
                ni += 1;
            }
        }
        let ret = unsafe { self.desc.call_fast(fast, ints, floats) };
        let result = unsafe {
            marshal::read(
                st,
                &self.signature[0],
                &ret as *const u128 as *const u8,
                Mem::Temp,
            )?
        };
        cx.finish(st)?;
        Ok(result)
    }
}

/// napi callback for C functions, `data` points at the `CFunction`.
pub unsafe extern "C" fn c_function_callback(
    env: napi_env,
    info: napi_callback_info,
) -> napi_value {
    let mut argc = 16usize;
    let mut argv = [null_mut(); 16];
    let mut data: *mut c_void = null_mut();
    napi_get_cb_info(
        env,
        info,
        &mut argc,
        argv.as_mut_ptr(),
        null_mut(),
        &mut data,
    );
    let function = &*(data as *const CFunction);
    let st = &*function.state;
    let result = if argc <= 16 {
        function.call(st, &argv[..argc])
    } else {
        let mut all = vec![null_mut(); argc];
        napi_get_cb_info(
            env,
            info,
            &mut argc,
            all.as_mut_ptr(),
            null_mut(),
            null_mut(),
        );
        function.call(st, &all)
    };
    match result {
        Ok(v) => v,
        Err(e) => {
            if !Js(env).is_exception_pending() {
                Js(env).throw(&e);
            }
            null_mut()
        }
    }
}

/// Cached [`CFunction`] for the function declaration `id`.
pub fn function_for(st: &State, id: MetaId) -> Result<Rc<CFunction>, String> {
    if let Some(f) = st.functions.borrow().get(&id) {
        return Ok(f.clone());
    }
    let meta = st.meta.meta_at(id).ok_or("unknown function")?;
    let address = st.symbol(&meta)? as usize;
    let function = Rc::new(CFunction::new(
        st,
        meta.name(),
        address,
        meta.signature().unwrap_or_default(),
        meta.is_variadic(),
    )?);
    st.functions.borrow_mut().insert(id, function.clone());
    Ok(function)
}

/// JS function wrapping a C function.
pub fn js_function(st: &State, name: &str, function: &Rc<CFunction>) -> napi_value {
    let f = st.js().function(
        name,
        Some(c_function_callback),
        Rc::as_ptr(function) as *mut c_void,
    );
    crate::interop::tag_function(st, f, function.address);
    f
}

/// Callable JS function for a C function pointer.
pub fn function_from_pointer(
    st: &State,
    address: usize,
    signature: &[Type],
) -> Result<napi_value, String> {
    if signature.is_empty() {
        return Err("function pointer without signature".into());
    }
    let function = Rc::new(CFunction::new(
        st,
        "<function pointer>",
        address,
        signature.to_vec(),
        false,
    )?);
    let f = js_function(st, "", &function);
    // owned by the JS function
    unsafe extern "C" fn drop_function(_: napi_env, data: *mut c_void, _: *mut c_void) {
        drop(Rc::from_raw(data as *const CFunction));
    }
    let raw = Rc::into_raw(function) as *mut c_void;
    unsafe { napi_add_finalizer(st.env, f, raw, Some(drop_function), null_mut(), null_mut()) };
    Ok(f)
}

/// State of a libffi closure that calls into JS.
pub struct ClosureData {
    function: napi_ref,
    main_context: usize,
    signature: Vec<Type>,
    state: *const State,
    closure: Option<Closure>,
}

/// C function pointer calling `function`, cached per (function, signature).
pub fn closure_for(st: &State, function: napi_value, signature: &[Type]) -> Result<usize, String> {
    let js = st.js();
    if let Some(existing) = st
        .closures
        .borrow()
        .iter()
        .find(|c| c.signature == signature && js.strict_equals(js.deref(c.function), function))
    {
        return Ok(existing.closure.as_ref().unwrap().code as usize);
    }
    let mut data = Box::new(ClosureData {
        function: js.reference(function, 1),
        main_context: st.main_context.context,
        signature: signature.to_vec(),
        state: st,
        closure: None,
    });
    let closure = Closure::new(
        st,
        signature,
        closure_trampoline,
        &mut *data as *mut ClosureData as *mut c_void,
    )?;
    let address = closure.code as usize;
    data.closure = Some(closure);
    st.closures.borrow_mut().push(data);
    Ok(address)
}

/// Frees the closures made for `function` (`interop.release`).
pub fn release_closures(st: &State, function: napi_value) -> usize {
    let js = st.js();
    let mut closures = st.closures.borrow_mut();
    let before = closures.len();
    closures.retain(|c| {
        let keep = !js.strict_equals(js.deref(c.function), function);
        if !keep {
            js.delete_ref(c.function);
        }
        keep
    });
    before - closures.len()
}

unsafe extern "C" fn closure_trampoline(
    _cif: *mut raw::ffi_cif,
    ret: *mut c_void,
    args: *mut *mut c_void,
    userdata: *mut c_void,
) {
    let data = &*(userdata as *const ClosureData);
    let st = &*data.state;
    thread_local!(static THREAD: std::thread::ThreadId = std::thread::current().id());
    if THREAD.with(|t| *t) != st.thread {
        // called by C on another thread: run on the JS thread and wait
        let (ret, args, data_ptr) = (ret as usize, args as usize, userdata as usize);
        let done = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let signal = done.clone();
        nativescript_core::glib::invoke(
            data.main_context,
            Box::new(move || {
                invoke_js(
                    &*(data_ptr as *const ClosureData),
                    ret as *mut c_void,
                    args as *mut *mut c_void,
                );
                *signal.0.lock().unwrap() = true;
                signal.1.notify_all();
            }),
        );
        let mut finished = done.0.lock().unwrap();
        while !*finished {
            finished = done.1.wait(finished).unwrap();
        }
        return;
    }
    invoke_js(data, ret, args);
    crate::event_loop::schedule_jobs(st);
}

unsafe fn invoke_js(data: &ClosureData, ret: *mut c_void, args: *mut *mut c_void) {
    let st = &*data.state;
    let js = st.js();
    let ret_ty = &data.signature[0];
    let ret_size = st.layout(ret_ty).map(|l| l.size).unwrap_or(0);
    std::ptr::write_bytes(
        ret as *mut u8,
        0,
        ret_size.max(std::mem::size_of::<usize>()),
    );
    let mut scope = null_mut();
    napi_open_handle_scope(js.0, &mut scope);
    let argc = data.signature.len() - 1;
    let mut inline = [null_mut(); 8];
    let mut heap = Vec::new();
    let argv = if argc <= inline.len() {
        &mut inline[..argc]
    } else {
        heap.resize(argc, null_mut());
        &mut heap[..]
    };
    for (i, ty) in data.signature[1..].iter().enumerate() {
        match marshal::read(st, ty, *args.add(i) as *const u8, Mem::Temp) {
            Ok(v) => argv[i] = v,
            Err(e) => {
                eprintln!("[nativescript] callback argument {}: {e}", i + 1);
                napi_close_handle_scope(js.0, scope);
                return;
            }
        }
    }
    let result = js.call(js.deref(data.function), js.undefined(), argv);
    match result {
        Err(_) => crate::event_loop::report_pending(st),
        Ok(result) if !ret_ty.is_void() => {
            let mut cx = CallCx::default();
            let res = match ret_ty {
                Type::Bool | Type::Char | Type::Short | Type::Int | Type::Long | Type::LongLong => {
                    marshal::to_i64(js, result, ret_ty)
                        .map(|v| (ret as *mut i64).write_unaligned(v))
                }
                Type::UChar | Type::UShort | Type::Unichar | Type::UInt => {
                    marshal::to_i64(js, result, ret_ty)
                        .map(|v| (ret as *mut u64).write_unaligned(v as u64))
                }
                _ => marshal::write(st, ret_ty, result, ret as *mut u8, &mut cx),
            };
            if let Err(e) = res {
                eprintln!("[nativescript] callback return value: {e}");
            }
        }
        Ok(_) => {}
    }
    napi_close_handle_scope(js.0, scope);
}

/// Runtime of a napi callback.
#[inline]
pub fn st<'a>(env: napi_env) -> &'a State {
    state(env)
}
