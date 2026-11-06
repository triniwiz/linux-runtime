//! Calling C functions through libffi, and libffi closures for JS callbacks.

use crate::interop::keys;
use crate::marshal::{self, CallCx};
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
        Ok(CFunction {
            name: name.to_owned(),
            address,
            signature,
            variadic,
            cif,
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
        let mut argv: Vec<*mut c_void> = Vec::with_capacity(argc);
        for (i, ty) in signature[1..].iter().enumerate() {
            let size = st.layout(ty)?.size.max(8);
            let slot = cx.scratch(size);
            unsafe { marshal::write(scope, st, ty, args.get(i as i32), slot, &mut cx) }
                .map_err(|e| format!("{}() argument {}: {e}", self.name, i + 1))?;
            argv.push(slot as *mut c_void);
        }

        let ret_ty = &signature[0];
        // small integer returns are widened to ffi_arg by libffi
        let ret = cx.scratch(st.layout(ret_ty)?.size.max(16));

        unsafe {
            raw::ffi_call(
                cif.as_raw_ptr(),
                Some(std::mem::transmute::<usize, unsafe extern "C" fn()>(self.address)),
                ret as *mut c_void,
                argv.as_mut_ptr(),
            );
        }

        let result = unsafe { marshal::read(scope, st, ret_ty, ret)? };
        cx.finish(scope, st)?;
        Ok(result)
    }
}

/// JS callback for C functions: `args.data()` holds the metadata offset.
pub fn c_function_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let st = state(scope);
    let Some(offset) = args.data().uint32_value(scope) else {
        return;
    };
    let function = match function_for(&st, offset) {
        Ok(f) => f,
        Err(e) => return throw_error(scope, &e),
    };
    match function.call(scope, &st, &args) {
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
    // Owned by the JS function through an External kept alive for the runtime
    // lifetime (function pointers are rare and usually long lived).
    let raw = Box::into_raw(Box::new(function));
    let data = v8::External::new(scope, raw as *mut c_void);
    let f = v8::Function::builder(function_pointer_callback)
    .data(data.into())
    .build(scope)
    .ok_or("failed to create function")?;
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

unsafe extern "C" fn closure_trampoline(
    _cif: *mut raw::ffi_cif,
    ret: *mut c_void,
    args: *mut *mut c_void,
    userdata: *mut c_void,
) {
    let data = &*(userdata as *const ClosureData);
    let st = &*data.state;
    let ret_ty = &data.signature[0];
    let ret_size = st.layout(ret_ty).map(|l| l.size).unwrap_or(0);
    // libffi expects integral returns widened to ffi_arg
    std::ptr::write_bytes(ret as *mut u8, 0, ret_size.max(std::mem::size_of::<usize>()));

    if std::thread::current().id() != st.thread {
        eprintln!("[nativescript] callback invoked from a foreign thread, ignored");
        return;
    }
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
        match marshal::read(tc, st, ty, *args.add(i) as *const u8) {
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
    // returning char* must return a Pointer to memory that outlives the call.
    drop(cx);
}
