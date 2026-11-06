//! Calling C functions through libffi, and libffi closures for JS callbacks.

use crate::interop::keys;
use crate::marshal::{self, CallCx, Mem};
use crate::state::{state, State};
use crate::{report_exception, throw_error};
use libffi::middle::{Cif, Type as FfiType};
use libffi::raw;
use nativescript_metadata::Type;
use std::ffi::c_void;
use std::rc::Rc;

/// A C function with a prepared call interface.
pub struct CFunction {
    pub name: String,
    pub address: usize,
    /// return type first
    pub signature: Vec<Type>,
    pub variadic: bool,
    /// prepared once for non variadic functions
    cif: Option<Cif>,
    /// argument slot sizes and the return slot size (non variadic)
    slots: Vec<usize>,
    ret_size: usize,
    /// owning runtime, avoids the isolate slot lookup per call
    state: *const State,
    /// register-only call without libffi, when the signature allows it
    fast: Option<FastCall>,
}

/// Int and float args use separate register sequences (x86_64 SysV, AArch64): one
/// fn type taking all registers calls any signature that fits in them.
#[derive(Clone)]
struct FastCall {
    /// per argument: true = float register
    float_args: Vec<bool>,
    float_ret: bool,
}

#[cfg(target_arch = "x86_64")]
const INT_REGS: usize = 6;
#[cfg(target_arch = "aarch64")]
const INT_REGS: usize = 8;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const INT_REGS: usize = 0;
const FLOAT_REGS: usize = 8;

type IntFn = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64) -> i64;
type FloatFn = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64) -> f64;

impl FastCall {
    fn new(signature: &[Type]) -> Option<Self> {
        let class = |t: &Type| -> Option<bool> {
            match t {
                Type::Float | Type::Double => Some(true),
                Type::Bool | Type::Char | Type::UChar | Type::Short | Type::UShort | Type::Unichar | Type::Int | Type::UInt => Some(false),
                Type::Long | Type::ULong | Type::LongLong | Type::ULongLong => Some(false),
                t if t.is_pointer_like() => Some(false),
                _ => None,
            }
        };
        let float_ret = if signature[0].is_void() { false } else { class(&signature[0])? };
        let float_args = signature[1..].iter().map(class).collect::<Option<Vec<_>>>()?;
        let floats = float_args.iter().filter(|f| **f).count();
        (INT_REGS > 0 && float_args.len() - floats <= INT_REGS.min(6) && floats <= FLOAT_REGS).then_some(FastCall { float_args, float_ret })
    }
}

/// Arguments of type `T[N]` decay to `T*`.
fn decay(ty: &Type) -> Type {
    match ty {
        Type::ConstantArray(_, inner) => Type::Pointer(inner.clone()),
        other => other.clone(),
    }
}

impl CFunction {
    pub fn new(st: &State, name: &str, address: usize, signature: Vec<Type>, variadic: bool) -> Result<Self, String> {
        let signature: Vec<Type> = signature.iter().map(decay).collect();
        let cif = if variadic {
            None
        } else {
            Some(Self::prep(st, &signature, None)?)
        };
        let fast = if variadic { None } else { FastCall::new(&signature) };
        let slots = signature[1..].iter().map(|t| st.layout(t).map(|l| l.size.max(8))).collect::<Result<_, _>>()?;
        // small integer returns are widened to ffi_arg by libffi
        let ret_size = st.layout(&signature[0])?.size.max(16);
        Ok(CFunction {
            name: name.to_owned(),
            address,
            signature,
            variadic,
            cif,
            slots,
            ret_size,
            state: st,
            fast,
        })
    }

    fn prep(st: &State, signature: &[Type], fixed: Option<usize>) -> Result<Cif, String> {
        let ret = st.ffi_type(&signature[0])?;
        let args = signature[1..].iter().map(|t| st.ffi_type(t)).collect::<Result<Vec<_>, _>>()?;
        Ok(match fixed {
            Some(fixed) => Cif::new_variadic(args, fixed, ret),
            None => Cif::new(args, ret),
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
            variadic_cif = Self::prep(st, &variadic_sig, Some(fixed))?;
            (&variadic_sig[..], &variadic_cif)
        } else {
            (&self.signature[..], self.cif.as_ref().unwrap())
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
        let ret = cx.scratch(if self.variadic { st.layout(ret_ty)?.size.max(16) } else { self.ret_size });

        unsafe {
            raw::ffi_call(
                cif.as_raw_ptr(),
                Some(std::mem::transmute::<usize, unsafe extern "C" fn()>(self.address)),
                ret as *mut c_void,
                argv.as_mut_ptr(),
            );
        }

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
            unsafe { marshal::write(scope, st, ty, args.get(i as i32), &mut slot as *mut u128 as *mut u8, &mut cx) }
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
        let ret: u128 = unsafe {
            let [a, b, c, d, e, f, g, h] = ints;
            let [p, q, r, s, t, u, v, w] = floats;
            if fast.float_ret {
                let fun: FloatFn = std::mem::transmute(self.address);
                fun(a, b, c, d, e, f, g, h, p, q, r, s, t, u, v, w).to_bits() as u128
            } else {
                let fun: IntFn = std::mem::transmute(self.address);
                fun(a, b, c, d, e, f, g, h, p, q, r, s, t, u, v, w) as u64 as u128
            }
        };
        let result = unsafe { marshal::read(scope, st, &self.signature[0], &ret as *const u128 as *const u8, Mem::Temp)? };
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
    let Ok(ext) = v8::Local::<v8::External>::try_from(args.data()) else { return };
    let function = unsafe { &*(ext.value() as *const CFunction) };
    let st = unsafe { &*function.state };
    match function.call(scope, st, &args) {
        Ok(v) => rv.set(v),
        Err(e) => throw_error(scope, &e),
    }
}

/// Cached [`CFunction`] for the function declared at metadata `offset`.
pub fn function_for(st: &State, offset: u32) -> Result<Rc<CFunction>, String> {
    if let Some(f) = st.functions.borrow().get(&offset) {
        return Ok(f.clone());
    }
    let meta = st.meta.meta_at(offset);
    let address = st.symbol(&meta)? as usize;
    let function = Rc::new(CFunction::new(
        st,
        meta.name(),
        address,
        meta.signature().unwrap_or_default(),
        meta.is_variadic(),
    )?);
    st.functions.borrow_mut().insert(offset, function.clone());
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
    cif: Cif,
    closure: *mut raw::ffi_closure,
}

impl Drop for ClosureData {
    fn drop(&mut self) {
        unsafe { raw::ffi_closure_free(self.closure as *mut c_void) };
    }
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

    let ret = st.ffi_type(&signature[0])?;
    let args: Vec<FfiType> = signature[1..].iter().map(|t| st.ffi_type(t)).collect::<Result<_, _>>()?;
    let cif = Cif::new(args, ret);

    let mut code: *mut c_void = std::ptr::null_mut();
    let closure = unsafe { raw::ffi_closure_alloc(std::mem::size_of::<raw::ffi_closure>(), &mut code) }
        as *mut raw::ffi_closure;
    if closure.is_null() {
        return Err("ffi_closure_alloc failed".into());
    }
    let mut data = Box::new(ClosureData {
        function: v8::Global::new(scope, function),
        main_context: st.main_context.context,
        signature: signature.to_vec(),
        state: st as *const State,
        cif,
        closure,
    });
    let status = unsafe {
        raw::ffi_prep_closure_loc(
            closure,
            data.cif.as_raw_ptr(),
            Some(closure_trampoline),
            &mut *data as *mut ClosureData as *mut c_void,
            code,
        )
    };
    if status != raw::ffi_status_FFI_OK {
        return Err(format!("ffi_prep_closure_loc failed ({status})"));
    }
    let address = code as usize;
    st.closures.borrow_mut().push(data);
    let value = v8::BigInt::new_from_u64(scope, address as u64);
    function.set_private(scope, key, value.into());
    Ok(address)
}

/// Frees the libffi closures created for `function` (`interop.release`).
pub fn release_closures<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, function: v8::Local<'s, v8::Function>) -> usize {
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
        crate::event_loop::run_on_js_thread(data.main_context, Box::new(move || unsafe {
            invoke_js(data, ret as *mut c_void, args as *mut *mut c_void);
        }));
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
    std::ptr::write_bytes(ret as *mut u8, 0, ret_size.max(std::mem::size_of::<usize>()));
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
