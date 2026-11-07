//! Calling C functions through libffi, and libffi closures for JS callbacks.

use crate::interop::keys;
use crate::marshal::{self, CallCx, Mem};
use crate::state::{state, State};
use crate::{report_exception, throw_error};
use libffi::raw;
use nativescript_core::call::{CallDesc, FastCall};
use nativescript_core::closure::Closure;
use nativescript_metadata::{MetaId, Type};
use std::ffi::c_void;
use std::rc::Rc;

/// A C function bound to a runtime.
pub struct CFunction {
    desc: CallDesc,
    /// owning runtime, avoids the isolate slot lookup per call
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

    /// C type used for a variadic argument, inferred from the JS value
    /// (default argument promotions: int / double / pointer).
    fn vararg_type(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Result<Type, String> {
        if value.is_int32() || value.is_boolean() {
            return Ok(Type::Int);
        }
        if value.is_uint32() {
            return Ok(Type::UInt);
        }
        if value.is_number() {
            let n = value.number_value(scope).unwrap_or(0.0);
            return Ok(if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                Type::LongLong
            } else {
                Type::Double
            });
        }
        if value.is_big_int() {
            return Ok(Type::LongLong);
        }
        if value.is_string() {
            return Ok(Type::CString);
        }
        if value.is_function() {
            return Err("functions can't be passed as variadic arguments".into());
        }
        if value.is_null_or_undefined() || value.is_object() {
            return Ok(Type::Pointer(Box::new(Type::Void)));
        }
        Err("unsupported variadic argument".into())
    }

    pub fn call<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        args: &v8::FunctionCallbackArguments<'s>,
    ) -> Result<v8::Local<'s, v8::Value>, String> {
        let fixed = self.signature.len() - 1;
        let argc = args.length() as usize;
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
            return self.call_fast(scope, st, args, fast);
        }

        let variadic_sig;
        let variadic_cif;
        let (signature, cif) = if self.variadic {
            let mut sig = self.signature.clone();
            for i in fixed..argc {
                sig.push(Self::vararg_type(scope, args.get(i as i32))?);
            }
            variadic_sig = sig;
            variadic_cif = CallDesc::prep(st, &variadic_sig, Some(fixed))?;
            (&variadic_sig[..], &variadic_cif)
        } else {
            (&self.signature[..], self.cif().unwrap())
        };

        let mut cx = CallCx::default();
        let mut argv_inline = [std::ptr::null_mut::<c_void>(); 16];
        let mut argv_heap = Vec::new();
        let argv: &mut [*mut c_void] = if argc <= 16 {
            &mut argv_inline[..argc]
        } else {
            argv_heap.resize(argc, std::ptr::null_mut());
            &mut argv_heap
        };
        for (i, ty) in signature[1..].iter().enumerate() {
            let size = match self.slots.get(i) {
                Some(s) => *s,
                None => st.layout(ty)?.size.max(8),
            };
            let slot = cx.scratch(size);
            unsafe { marshal::write(scope, st, ty, args.get(i as i32), slot, &mut cx) }
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

        let result = unsafe { marshal::read(scope, st, ret_ty, ret, Mem::Temp)? };
        cx.finish(scope, st)?;
        Ok(result)
    }
}

impl CFunction {
    fn call_fast<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        args: &v8::FunctionCallbackArguments<'s>,
        fast: &FastCall,
    ) -> Result<v8::Local<'s, v8::Value>, String> {
        let mut cx = CallCx::default();
        let mut ints = [0i64; 8];
        let mut floats = [0f64; 8];
        let (mut ni, mut nf) = (0, 0);
        for (i, ty) in self.signature[1..].iter().enumerate() {
            let mut slot = 0u128;
            unsafe {
                marshal::write(
                    scope,
                    st,
                    ty,
                    args.get(i as i32),
                    &mut slot as *mut u128 as *mut u8,
                    &mut cx,
                )
            }
            .map_err(|e| format!("{}() argument {}: {e}", self.name, i + 1))?;
            if fast.float_args[i] {
                // a float sits in the low 32 bits of the register
                floats[nf] = f64::from_bits(slot as u64);
                nf += 1;
            } else {
                ints[ni] = slot as u64 as i64;
                ni += 1;
            }
        }
        let ret = unsafe { self.desc.call_fast(fast.float_ret, ints, floats) };
        let result = unsafe {
            marshal::read(
                scope,
                st,
                &self.signature[0],
                &ret as *const u128 as *const u8,
                Mem::Temp,
            )?
        };
        cx.finish(scope, st)?;
        Ok(result)
    }
}

/// JS callback for C functions: `args.data()` points at the `CFunction`
/// (kept alive by `State::functions`).
pub fn c_function_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let Ok(ext) = v8::Local::<v8::External>::try_from(args.data()) else {
        return;
    };
    let function = unsafe { &*(ext.value() as *const CFunction) };
    let st = unsafe { &*function.state };
    match function.call(scope, st, &args) {
        Ok(v) => rv.set(v),
        Err(e) => throw_error(scope, &e),
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

/// Callable JS function for a C function pointer (e.g. returned by a call).
pub fn function_from_pointer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    address: usize,
    signature: &[Type],
) -> Result<v8::Local<'s, v8::Value>, String> {
    if signature.is_empty() {
        return Err("function pointer without signature".into());
    }
    let function = CFunction::new(st, "<function pointer>", address, signature.to_vec(), false)?;
    // The CFunction is owned by an ArrayBuffer backing store attached to the JS
    // function: when the function is garbage collected the deleter frees it.
    let raw = Box::into_raw(Box::new(function));
    unsafe extern "C" fn drop_function(data: *mut c_void, _: usize, _: *mut c_void) {
        drop(Box::from_raw(data as *mut CFunction));
    }
    let store = unsafe {
        v8::ArrayBuffer::new_backing_store_from_ptr(
            raw as *mut c_void,
            std::mem::size_of::<CFunction>(),
            drop_function,
            std::ptr::null_mut(),
        )
    }
    .make_shared();
    let owner = v8::ArrayBuffer::with_backing_store(scope, &store);
    let data = v8::External::new(scope, raw as *mut c_void);
    let f = v8::Function::builder(function_pointer_callback)
        .data(data.into())
        .build(scope)
        .ok_or("failed to create function")?;
    let key = keys::owner(scope);
    f.set_private(scope, key, owner.into());
    let key = keys::function_address(scope);
    let addr = v8::BigInt::new_from_u64(scope, address as u64);
    f.set_private(scope, key, addr.into());
    Ok(f.into())
}

fn function_pointer_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let st = state(scope);
    let Ok(ext) = v8::Local::<v8::External>::try_from(args.data()) else {
        return;
    };
    let function = unsafe { &*(ext.value() as *const CFunction) };
    match function.call(scope, &st, &args) {
        Ok(v) => rv.set(v),
        Err(e) => throw_error(scope, &e),
    }
}

/// State of a libffi closure that calls into JS.
pub struct ClosureData {
    function: v8::Global<v8::Function>,
    /// GMainContext of the JS thread, read from foreign threads
    main_context: usize,
    signature: Vec<Type>,
    state: *const State,
    closure: Option<Closure>,
}

/// C function pointer calling `function`; cached per signature, freed by `interop.release`.
pub fn closure_for<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    function: v8::Local<'s, v8::Function>,
    signature: &[Type],
) -> Result<usize, String> {
    let key = keys::closure(scope, signature);
    if let Some(existing) = function.get_private(scope, key) {
        if let Ok(b) = v8::Local::<v8::BigInt>::try_from(existing) {
            return Ok(b.u64_value().0 as usize);
        }
    }
    let mut data = Box::new(ClosureData {
        function: v8::Global::new(scope, function),
        main_context: st.main_context.context,
        signature: signature.to_vec(),
        state: st as *const State,
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
    let value = v8::BigInt::new_from_u64(scope, address as u64);
    function.set_private(scope, key, value.into());
    Ok(address)
}

/// Frees the libffi closures created for `function` (`interop.release`).
pub fn release_closures<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    function: v8::Local<'s, v8::Function>,
) -> usize {
    let mut released = Vec::new();
    {
        let mut closures = st.closures.borrow_mut();
        let mut i = 0;
        while i < closures.len() {
            let f = v8::Local::new(scope, &closures[i].function);
            if f.strict_equals(function.into()) {
                released.push(closures.swap_remove(i));
            } else {
                i += 1;
            }
        }
    }
    for data in &released {
        let key = keys::closure(scope, &data.signature);
        let undefined = v8::undefined(scope);
        function.set_private(scope, key, undefined.into());
    }
    released.len()
}

unsafe extern "C" fn closure_trampoline(
    _cif: *mut raw::ffi_cif,
    ret: *mut c_void,
    args: *mut *mut c_void,
    userdata: *mut c_void,
) {
    let data = &*(userdata as *const ClosureData);
    let st = &*data.state;
    if std::thread::current().id() != st.thread {
        // called by C on another thread: run on the JS thread and wait
        let (ret, args) = (ret as usize, args as usize);
        crate::event_loop::run_on_js_thread(
            data.main_context,
            Box::new(move || unsafe {
                invoke_js(data, ret as *mut c_void, args as *mut *mut c_void);
            }),
        );
        return;
    }
    invoke_js(data, ret, args);
    crate::event_loop::schedule_microtasks(st);
}

unsafe fn invoke_js(data: &ClosureData, ret: *mut c_void, args: *mut *mut c_void) {
    let st = &*data.state;
    let ret_ty = &data.signature[0];
    let ret_size = st.layout(ret_ty).map(|l| l.size).unwrap_or(0);
    // libffi expects integral returns widened to ffi_arg
    std::ptr::write_bytes(
        ret as *mut u8,
        0,
        ret_size.max(std::mem::size_of::<usize>()),
    );
    let isolate = st.isolate.get();
    let Some(context) = st.context.borrow().clone() else {
        return;
    };
    let isolate = &mut *isolate;
    v8::scope!(let scope, isolate);
    let context = v8::Local::new(scope, &context);
    let scope = &mut v8::ContextScope::new(scope, context);
    v8::tc_scope!(let tc, scope);

    let mut argv = Vec::with_capacity(data.signature.len() - 1);
    for (i, ty) in data.signature[1..].iter().enumerate() {
        match marshal::read(tc, st, ty, *args.add(i) as *const u8, Mem::Temp) {
            Ok(v) => argv.push(v),
            Err(e) => {
                eprintln!("[nativescript] callback argument {}: {e}", i + 1);
                return;
            }
        }
    }
    let function = v8::Local::new(tc, &data.function);
    let recv = v8::undefined(tc).into();
    let result = function.call(tc, recv, &argv);
    if let Some(exception) = tc.exception() {
        let message = tc.message();
        report_exception(tc, exception, message);
        st.uncaught.set(st.uncaught.get() + 1);
        return;
    }
    let Some(result) = result else { return };
    if ret_ty.is_void() {
        return;
    }
    let mut cx = CallCx::default();
    let res = match ret_ty {
        // write integers as full width ffi_arg values
        Type::Bool | Type::Char | Type::Short | Type::Int | Type::Long | Type::LongLong => {
            marshal::to_i64(tc, result, ret_ty).map(|v| (ret as *mut i64).write_unaligned(v))
        }
        Type::UChar | Type::UShort | Type::Unichar | Type::UInt => {
            marshal::to_i64(tc, result, ret_ty).map(|v| (ret as *mut u64).write_unaligned(v as u64))
        }
        _ => marshal::write(tc, st, ret_ty, result, ret as *mut u8, &mut cx),
    };
    if let Err(e) = res {
        eprintln!("[nativescript] callback return value: {e}");
    }
    // NOTE: temporaries (e.g. strings) returned to C would dangle; callbacks
    // returning char* must return memory that outlives the call (interop.cstring).
    drop(cx);
}
