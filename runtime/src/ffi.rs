//! Calling C functions (in registers or through libffi), and libffi closures for JS
//! callbacks.

use crate::fast::{self, Strings};
use crate::interop::keys;
use crate::marshal::{self, CallCx, Mem};
use crate::state::State;
use crate::structs;
use crate::tagged::{self, info, Layout};
use crate::{report_exception, throw_error};
use libffi::raw;
use nativescript_core::call::{
    CallDesc, FastCall, FloatRegs, IntRegs, ValueKind, FLOAT_REGS, INT_REGS,
};
use nativescript_core::closure::Closure;
#[cfg(target_arch = "x86_64")]
use nativescript_core::closure::{RegisterClosure, Registers};
use nativescript_metadata::{MetaId, Type};
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::LazyLock;

/// A C function bound to a runtime.
pub struct CFunction {
    desc: CallDesc,
    /// owning runtime, avoids the isolate slot lookup per call
    pub(crate) state: *const State,
    /// integer-register arguments: (argument index, kind)
    pub(crate) int_args: Vec<(usize, ValueKind)>,
    /// register return of variadic calls
    variadic_ret: Option<FastCall>,
    /// view class of a record return
    ret_record: std::cell::OnceCell<structs::RecordClass>,
    /// last closure passed per callback argument, with `State::closure_generation`
    callbacks: Box<[Cell<(*const ClosureData, u64)>]>,
    /// returns a `Struct *`: unknown until the first non-NULL result tells whether the
    /// struct is a GObject type (read as JS objects) or not (plain Pointers)
    ret_gobject: Cell<Option<bool>>,
}

impl std::ops::Deref for CFunction {
    type Target = CallDesc;
    fn deref(&self) -> &CallDesc {
        &self.desc
    }
}

/// Default argument promotions of a variadic argument, by JS value.
static VARARG_TYPES: LazyLock<[Type; 6]> = LazyLock::new(|| {
    [
        Type::Int,
        Type::UInt,
        Type::LongLong,
        Type::Double,
        Type::CString,
        Type::Pointer(Box::new(Type::Void)),
    ]
});

impl CFunction {
    pub fn new(
        st: &State,
        name: &str,
        address: usize,
        signature: Vec<Type>,
        variadic: bool,
    ) -> Result<Self, String> {
        let desc = CallDesc::new(st, name, address, signature, variadic)?;
        let int_args = desc
            .args
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, k)| !k.is_float())
            .collect();
        let variadic_ret = if variadic {
            desc.variadic_ret(st)
        } else {
            None
        };
        let callbacks = desc
            .args
            .iter()
            .map(|_| Cell::new((std::ptr::null(), 0)))
            .collect();
        let ret_gobject = Cell::new(crate::gobject::pointee(&desc.signature[0]).is_none().then_some(false));
        Ok(CFunction {
            desc,
            state: st,
            int_args,
            variadic_ret,
            ret_record: Default::default(),
            callbacks,
            ret_gobject,
        })
    }

    /// C function pointer for a JS function passed as callback argument `i`; the
    /// closure of the previous call is reused without a lookup. `None`: not a function.
    fn callback_arg<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        i: usize,
        ty: &Type,
        v: v8::Local<'s, v8::Value>,
    ) -> Result<Option<u64>, String> {
        let (Type::FunctionPointer(sig) | Type::Block(sig)) = ty else {
            return Ok(None);
        };
        let (data, generation) = self.callbacks[i].get();
        if !data.is_null() && generation == st.closure_generation.get() && unsafe { (*data).is(v) }
        {
            return Ok(Some(unsafe { (*data).code() } as u64));
        }
        let Ok(f) = v8::Local::<v8::Function>::try_from(v) else {
            return Ok(None);
        };
        if let Some(addr) = crate::interop::native_function_address(scope, st, f) {
            return Ok(Some(addr as u64));
        }
        let data = closure_data_for(scope, st, f, sig)
            .map_err(|e| format!("{}() argument {}: {e}", self.name, i + 1))?;
        self.callbacks[i].set((data, st.closure_generation.get()));
        Ok(Some(unsafe { (*data).code() } as u64))
    }

    /// C type used for a variadic argument, inferred from the JS value.
    fn vararg_type(value: v8::Local<v8::Value>) -> Result<(ValueKind, &'static Type), String> {
        let types = &*VARARG_TYPES;
        if value.is_int32() || value.is_boolean() {
            return Ok((ValueKind::I32, &types[0]));
        }
        if value.is_uint32() {
            return Ok((ValueKind::U32, &types[1]));
        }
        if let Ok(n) = v8::Local::<v8::Number>::try_from(value) {
            let n = n.value();
            return Ok(if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                (ValueKind::I64, &types[2])
            } else {
                (ValueKind::F64, &types[3])
            });
        }
        if value.is_big_int() {
            return Ok((ValueKind::I64, &types[2]));
        }
        if value.is_string() {
            return Ok((ValueKind::CString, &types[4]));
        }
        if value.is_function() {
            return Err("functions can't be passed as variadic arguments".into());
        }
        if value.is_null_or_undefined() || value.is_object() {
            return Ok((ValueKind::CString, &types[5]));
        }
        Err("unsupported variadic argument".into())
    }

    /// Slow API callback body.
    unsafe fn invoke(
        &self,
        info: *const v8::FunctionCallbackInfo,
        args: &v8::FunctionCallbackArguments,
    ) {
        let st = unsafe { &*self.state };
        if let Some(layout) = st.tagged().filter(|l| l.callback_info()) {
            let done = if self.variadic {
                unsafe { self.invoke_variadic_registers(info, layout) }
            } else {
                unsafe { self.invoke_registers(info, layout) }
            };
            if done {
                return;
            }
        }
        v8::callback_scope!(unsafe scope, unsafe { &*info });
        let mut rv = v8::ReturnValue::from_function_callback_info(unsafe { &*info });
        match self.call(scope, st, args) {
            Ok(v) => rv.set(v),
            Err(e) => throw_error(scope, &e),
        }
    }

    /// Register call without a scope when every argument converts in place;
    /// `false` leaves the call to [`Self::call`].
    #[inline]
    unsafe fn invoke_registers(
        &self,
        info: *const v8::FunctionCallbackInfo,
        layout: &Layout,
    ) -> bool {
        let Some(fast) = &self.fast else {
            return false;
        };
        if unsafe { info::argc(info) } != self.args.len() {
            return false;
        }
        let mut ints: IntRegs = [0; INT_REGS];
        let mut floats: FloatRegs = [0.0; FLOAT_REGS];
        let (mut ni, mut nf) = (0, 0);
        let mut strings = Strings::new();
        for (i, &kind) in self.args.iter().enumerate() {
            let v = unsafe { info::arg(info, i) };
            let isolate = || unsafe { info::isolate(info) };
            let Some(bits) = fast::simple_arg(Some(layout), isolate, kind, v, &mut strings) else {
                return false;
            };
            if kind.is_float() {
                floats[nf] = f64::from_bits(bits);
                nf += 1;
            } else {
                ints[ni] = bits as i64;
                ni += 1;
            }
        }
        let ret = if nf == 0 {
            unsafe { self.desc.call_ints(fast, &ints) }
        } else {
            unsafe { self.desc.call_fast(fast, &ints, &floats) }
        };
        unsafe { self.set_return(info, layout, ret) };
        true
    }

    /// [`Self::invoke_registers`] for variadic functions: variadic arguments are
    /// classified like [`Self::vararg_type`].
    #[inline]
    unsafe fn invoke_variadic_registers(
        &self,
        info: *const v8::FunctionCallbackInfo,
        layout: &Layout,
    ) -> bool {
        let Some(ret_class) = &self.variadic_ret else {
            return false;
        };
        let argc = unsafe { info::argc(info) };
        let fixed = self.args.len();
        if argc < fixed {
            return false;
        }
        let mut ints: IntRegs = [0; INT_REGS];
        let mut floats: FloatRegs = [0.0; FLOAT_REGS];
        let (mut ni, mut nf) = (0, 0);
        let mut strings = Strings::new();
        for i in 0..argc {
            let v = unsafe { info::arg(info, i) };
            let kind = match self.args.get(i) {
                Some(&k) => k,
                None => match vararg_kind(layout, v) {
                    Some(k) => k,
                    None => return false,
                },
            };
            if kind.is_float() && nf == FLOAT_REGS || !kind.is_float() && ni == INT_REGS {
                return false;
            }
            let isolate = || unsafe { info::isolate(info) };
            let Some(bits) = fast::simple_arg(Some(layout), isolate, kind, v, &mut strings) else {
                return false;
            };
            if kind.is_float() {
                floats[nf] = f64::from_bits(bits);
                nf += 1;
            } else {
                ints[ni] = bits as i64;
                ni += 1;
            }
        }
        let ret = unsafe { self.desc.call_variadic(ret_class, &ints, &floats) };
        unsafe { self.set_return(info, layout, ret) };
        true
    }

    /// Return value of a register call; numbers that fit a Smi, booleans and NULL are
    /// written in place.
    #[inline]
    unsafe fn set_return(&self, info: *const v8::FunctionCallbackInfo, layout: &Layout, ret: u128) {
        if unsafe { set_scalar_return(info, layout, self.ret, ret) } {
            return;
        }
        if self.ret == ValueKind::Pointer {
            let address = ret as u64 as usize;
            let st = unsafe { &*self.state };
            let done = if self.returns_gobject() {
                crate::gobject::live_slot(st, address)
                    .map(|slot| unsafe { info::set_return_handle(info, slot) })
                    .is_some()
            } else {
                unsafe { self.return_pointer(info, address) }
            };
            if done {
                return;
            }
        }
        v8::callback_scope!(unsafe scope, unsafe { &*info });
        let mut rv = v8::ReturnValue::from_function_callback_info(unsafe { &*info });
        let st = unsafe { &*self.state };
        let ret = &ret as *const u128 as *const u8;
        match unsafe { self.read_ret(scope, st, &self.signature[0], ret) } {
            Ok(v) => rv.set(v),
            Err(e) => throw_error(scope, &e),
        }
    }

    #[inline]
    fn returns_gobject(&self) -> bool {
        if let Some(known) = self.ret_gobject.get() {
            return known;
        }
        let st = unsafe { &*self.state };
        let name = crate::gobject::pointee(&self.signature[0]).unwrap_or_default();
        let known = crate::gobject::pointee_gtype(st, name) != 0;
        self.ret_gobject.set(Some(known));
        known
    }

    /// Pointer result made without a scope; `false` if the factory can't.
    #[inline]
    unsafe fn return_pointer(&self, info: *const v8::FunctionCallbackInfo, address: usize) -> bool {
        let st = unsafe { &*self.state };
        let pointers = st.pointers.borrow();
        let made = pointers
            .as_ref()
            .and_then(|f| unsafe { f.make_in_callback(info::raw_isolate(info), address) });
        match made {
            Some(o) => {
                unsafe { info::set_return_handle(info, o) };
                true
            }
            None => false,
        }
    }

    pub fn call<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        args: &v8::FunctionCallbackArguments<'s>,
    ) -> Result<v8::Local<'s, v8::Value>, String> {
        let fixed = self.args.len();
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
        if self.variadic {
            return self.call_variadic(scope, st, args);
        }
        let mut cx = CallCx::default();
        let mut strings = Strings::new();
        let ret = if let Some(fast) = &self.fast {
            let mut ints: IntRegs = [0; INT_REGS];
            let mut floats: FloatRegs = [0.0; FLOAT_REGS];
            let (mut ni, mut nf) = (0, 0);
            for (i, &kind) in self.args.iter().enumerate() {
                let bits = self.convert(
                    scope,
                    st,
                    i,
                    kind,
                    &self.signature[i + 1],
                    args.get(i as i32),
                    &mut cx,
                    &mut strings,
                )?;
                if kind.is_float() {
                    floats[nf] = f64::from_bits(bits);
                    nf += 1;
                } else {
                    ints[ni] = bits as i64;
                    ni += 1;
                }
            }
            unsafe { self.desc.call_fast(fast, &ints, &floats) }
        } else {
            let mut argv = [std::ptr::null_mut::<c_void>(); 32];
            let mut argv_heap = Vec::new();
            let argv: &mut [*mut c_void] = if fixed <= argv.len() {
                &mut argv[..fixed]
            } else {
                argv_heap.resize(fixed, std::ptr::null_mut());
                &mut argv_heap
            };
            for (i, &kind) in self.args.iter().enumerate() {
                let slot = cx.scratch(self.slots[i]);
                let ty = &self.signature[i + 1];
                self.write(
                    scope,
                    st,
                    i,
                    kind,
                    ty,
                    args.get(i as i32),
                    slot,
                    &mut cx,
                    &mut strings,
                )?;
                argv[i] = slot as *mut c_void;
            }
            let ret = cx.scratch(self.ret_size);
            unsafe { self.desc.call(self.cif().unwrap(), argv, ret) };
            let result = unsafe { self.read_ret(scope, st, &self.signature[0], ret) }?;
            cx.finish(scope, st)?;
            return Ok(result);
        };
        let result = unsafe {
            self.read_ret(
                scope,
                st,
                &self.signature[0],
                &ret as *const u128 as *const u8,
            )?
        };
        cx.finish(scope, st)?;
        Ok(result)
    }

    /// Argument `i` as register bits: cheap conversions first, then `marshal::write`.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn convert<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        i: usize,
        kind: ValueKind,
        ty: &Type,
        v: v8::Local<'s, v8::Value>,
        cx: &mut CallCx<'s>,
        strings: &mut Strings,
    ) -> Result<u64, String> {
        if kind == ValueKind::Callback {
            if let Some(addr) = self.callback_arg(scope, st, i, ty, v)? {
                return Ok(addr);
            }
        }
        let isolate: *const v8::Isolate = {
            let i: &v8::Isolate = scope;
            i
        };
        if let Some(bits) = fast::simple_arg(st.tagged(), || isolate, kind, v, strings) {
            return Ok(bits);
        }
        let mut slot = 0u128;
        let dst = &mut slot as *mut u128 as *mut u8;
        unsafe { marshal::write(scope, st, ty, v, dst, cx) }
            .map_err(|e| format!("{}() argument {}: {e}", self.name, i + 1))?;
        Ok(if kind.is_float() {
            slot as u64
        } else {
            kind.extend(slot as u64 as i64) as u64
        })
    }

    /// Argument `i` into a libffi slot (at least 8 bytes).
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn write<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        i: usize,
        kind: ValueKind,
        ty: &Type,
        v: v8::Local<'s, v8::Value>,
        slot: *mut u8,
        cx: &mut CallCx<'s>,
        strings: &mut Strings,
    ) -> Result<(), String> {
        if kind == ValueKind::Callback {
            if let Some(addr) = self.callback_arg(scope, st, i, ty, v)? {
                unsafe { (slot as *mut u64).write_unaligned(addr) };
                return Ok(());
            }
        }
        let isolate: *const v8::Isolate = {
            let i: &v8::Isolate = scope;
            i
        };
        if let Some(bits) = fast::simple_arg(st.tagged(), || isolate, kind, v, strings) {
            unsafe { (slot as *mut u64).write_unaligned(bits) };
            return Ok(());
        }
        unsafe { marshal::write(scope, st, ty, v, slot, cx) }
            .map_err(|e| format!("{}() argument {}: {e}", self.name, i + 1))
    }

    fn call_variadic<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        args: &v8::FunctionCallbackArguments<'s>,
    ) -> Result<v8::Local<'s, v8::Value>, String> {
        let fixed = self.args.len();
        let argc = args.length() as usize;
        let mut types: Vec<(ValueKind, &Type)> = Vec::with_capacity(argc);
        for i in 0..argc {
            types.push(if i < fixed {
                (self.args[i], &self.signature[i + 1])
            } else {
                Self::vararg_type(args.get(i as i32))
                    .map_err(|e| format!("{}() argument {}: {e}", self.name, i + 1))?
            });
        }
        let floats_used = types.iter().filter(|(k, _)| k.is_float()).count();
        let registers = self.variadic_ret.as_ref().filter(|_| {
            !types.iter().any(|(k, _)| *k == ValueKind::Other)
                && argc - floats_used <= INT_REGS
                && floats_used <= FLOAT_REGS
        });
        let mut cx = CallCx::default();
        let mut strings = Strings::new();
        if let Some(ret_class) = registers {
            let mut ints: IntRegs = [0; INT_REGS];
            let mut floats: FloatRegs = [0.0; FLOAT_REGS];
            let (mut ni, mut nf) = (0, 0);
            for (i, &(kind, ty)) in types.iter().enumerate() {
                let bits = self.convert(
                    scope,
                    st,
                    i,
                    kind,
                    ty,
                    args.get(i as i32),
                    &mut cx,
                    &mut strings,
                )?;
                if kind.is_float() {
                    floats[nf] = f64::from_bits(bits);
                    nf += 1;
                } else {
                    ints[ni] = bits as i64;
                    ni += 1;
                }
            }
            let ret = unsafe { self.desc.call_variadic(ret_class, &ints, &floats) };
            let ret = &ret as *const u128 as *const u8;
            let result = unsafe { self.read_ret(scope, st, &self.signature[0], ret)? };
            cx.finish(scope, st)?;
            return Ok(result);
        }
        let signature: Vec<Type> = std::iter::once(self.signature[0].clone())
            .chain(types.iter().map(|(_, t)| (*t).clone()))
            .collect();
        let cif = CallDesc::prep(st, &signature, Some(fixed))?;
        let mut argv = vec![std::ptr::null_mut::<c_void>(); argc];
        for (i, &(kind, ty)) in types.iter().enumerate() {
            let slot = cx.scratch(st.layout(ty)?.size.max(8));
            self.write(
                scope,
                st,
                i,
                kind,
                ty,
                args.get(i as i32),
                slot,
                &mut cx,
                &mut strings,
            )?;
            argv[i] = slot as *mut c_void;
        }
        let ret = cx.scratch(st.layout(&self.signature[0])?.size.max(16));
        unsafe { self.desc.call(&cif, &mut argv, ret) };
        let result = unsafe { self.read_ret(scope, st, &self.signature[0], ret)? };
        cx.finish(scope, st)?;
        Ok(result)
    }

    /// Return value at `ret`: records through the cached view class.
    unsafe fn read_ret<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        ty: &Type,
        ret: *const u8,
    ) -> Result<v8::Local<'s, v8::Value>, String> {
        if !matches!(ty, Type::Struct(_) | Type::Union(_)) {
            return unsafe { marshal::read(scope, st, ty, ret, Mem::Temp) };
        }
        let class = match self.ret_record.get() {
            Some(c) => c,
            None => {
                let c = structs::RecordClass::new(scope, st, ty)?;
                self.ret_record.get_or_init(|| c)
            }
        };
        class.copy(scope, st, ret)
    }
}

/// Sets a scalar result without a scope (Smis, booleans and NULL in place); `false`
/// for kinds that need one (BigInts, objects, strings).
#[inline]
pub(crate) unsafe fn set_scalar_return(
    info: *const v8::FunctionCallbackInfo,
    layout: &Layout,
    kind: ValueKind,
    ret: u128,
) -> bool {
    let mut rv = v8::ReturnValue::from_function_callback_info(unsafe { &*info });
    match kind {
        ValueKind::Void => {}
        ValueKind::Bool => unsafe { info::set_return_word(info, layout.bool_word(ret as u8 != 0)) },
        ValueKind::I8 | ValueKind::U8 | ValueKind::I16 | ValueKind::U16 | ValueKind::I32 => unsafe {
            info::set_return_int32(info, fast::fast_ret(kind, ret) as u32 as i32)
        },
        ValueKind::U32 => match i32::try_from(ret as u32) {
            Ok(v) => unsafe { info::set_return_int32(info, v) },
            Err(_) => rv.set_uint32(ret as u32),
        },
        ValueKind::I64 if (ret as u64 as i64).unsigned_abs() <= 1 << 53 => {
            match i32::try_from(ret as u64 as i64) {
                Ok(v) => unsafe { info::set_return_int32(info, v) },
                Err(_) => rv.set_double(ret as u64 as i64 as f64),
            }
        }
        ValueKind::U64 if ret as u64 <= 1 << 53 => match i32::try_from(ret as u64) {
            Ok(v) => unsafe { info::set_return_int32(info, v) },
            Err(_) => rv.set_double(ret as u64 as f64),
        },
        ValueKind::F32 => rv.set_double(f32::from_bits(ret as u32) as f64),
        ValueKind::F64 => rv.set_double(f64::from_bits(ret as u64)),
        ValueKind::Pointer if ret as u64 == 0 => unsafe {
            info::set_return_word(info, layout.null_word())
        },
        _ => return false,
    }
    true
}

/// [`CFunction::vararg_type`] from the value's layout; `None` for values that need the
/// API (bigints, objects, functions, strings to flatten).
#[inline]
fn vararg_kind(layout: &Layout, v: v8::Local<v8::Value>) -> Option<ValueKind> {
    Some(match layout.classify(v) {
        tagged::Value::Int(_) | tagged::Value::Bool(_) => ValueKind::I32,
        tagged::Value::Number(n) => {
            let integral = n.fract() == 0.0 && !(n == 0.0 && n.is_sign_negative());
            if integral && n >= i32::MIN as f64 && n <= i32::MAX as f64 {
                ValueKind::I32
            } else if integral && n >= 0.0 && n <= u32::MAX as f64 {
                ValueKind::U32
            } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                ValueKind::I64
            } else {
                ValueKind::F64
            }
        }
        tagged::Value::Null
        | tagged::Value::Undefined
        | tagged::Value::Pointer(_)
        | tagged::Value::FlatString => ValueKind::CString,
        tagged::Value::Other => return None,
    })
}

/// API callback of C functions: `data` is an External with the `CFunction` (kept
/// alive by `State::functions`, or by the JS function for function pointers).
pub unsafe extern "C" fn c_function_callback(info: *const v8::FunctionCallbackInfo) {
    let args = v8::FunctionCallbackArguments::from_function_callback_info(unsafe { &*info });
    let function = unsafe { &*(tagged::external_value(args.data()) as *const CFunction) };
    unsafe { function.invoke(info, &args) }
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
        drop(unsafe { Box::from_raw(data as *mut CFunction) });
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
    let f = v8::Function::builder_raw(c_function_callback)
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

/// State of a libffi closure that calls into JS.
pub struct ClosureData {
    function: v8::Global<v8::Function>,
    /// handle slot of `function`
    function_slot: *const v8::Function,
    /// GMainContext of the JS thread, read from foreign threads
    main_context: usize,
    signature: Vec<Type>,
    /// kinds of the arguments read from C
    args: Vec<ValueKind>,
    /// kind of the value returned to C
    ret: ValueKind,
    ret_size: usize,
    /// per argument: (float register, register index), for register closures
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    slots: Vec<(bool, u8)>,
    state: *const State,
    closure: Option<ClosureCode>,
    /// a vfunc of a JS GObject class: `function` is called on the JS object of the
    /// first argument (true: dispose, which mustn't resurrect it)
    method: Option<bool>,
    /// a signal handler's: freed when GLib finalizes its closure, not by `interop.release`
    handler: bool,
}

enum ClosureCode {
    Ffi(Closure),
    #[cfg(target_arch = "x86_64")]
    Registers(RegisterClosure),
}

impl ClosureData {
    #[inline]
    fn code(&self) -> usize {
        match &self.closure {
            Some(ClosureCode::Ffi(c)) => c.code as usize,
            #[cfg(target_arch = "x86_64")]
            Some(ClosureCode::Registers(c)) => c.code as usize,
            None => 0,
        }
    }

    /// Whether `v` is this closure's function.
    #[inline]
    fn is(&self, v: v8::Local<v8::Value>) -> bool {
        tagged::word(v) == unsafe { *(self.function_slot as *const usize) }
    }
}

/// Closure state calling `function`: cached per signature, freed by `interop.release`.
fn closure_data_for<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    function: v8::Local<'s, v8::Function>,
    signature: &[Type],
) -> Result<*const ClosureData, String> {
    let key = keys::closure(scope, signature);
    if let Some(existing) = function.get_private(scope, key) {
        if let Ok(b) = v8::Local::<v8::BigInt>::try_from(existing) {
            return Ok(b.u64_value().0 as usize as *const ClosureData);
        }
    }
    let ptr = new_closure_data(scope, st, function, signature, None, false)?;
    let value = v8::BigInt::new_from_u64(scope, ptr as usize as u64);
    function.set_private(scope, key, value.into());
    Ok(ptr)
}

fn new_closure_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    function: v8::Local<'s, v8::Function>,
    signature: &[Type],
    method: Option<bool>,
    handler: bool,
) -> Result<*const ClosureData, String> {
    let global = v8::Global::new(scope, function);
    // SAFETY: the Global belongs to this isolate and lives in the ClosureData
    let function_slot = unsafe { v8::Handle::get_unchecked(&global) as *const v8::Function };
    let ret = match ValueKind::of_arg(&signature[0]) {
        // strings returned to C would dangle: only the full conversion takes them
        ValueKind::CString => ValueKind::Pointer,
        k => k,
    };
    // signatures that fit in registers skip libffi's per-call argument classification
    let registers = CallDesc::new(st, "", 0, signature.to_vec(), false)?.fast;
    let mut slots = Vec::new();
    if let Some(fast) = &registers {
        let (mut ni, mut nf) = (0u8, 0u8);
        for float in &fast.float_args {
            slots.push(if *float { (true, nf) } else { (false, ni) });
            if *float {
                nf += 1;
            } else {
                ni += 1;
            }
        }
    }
    let mut data = Box::new(ClosureData {
        function: global,
        function_slot,
        main_context: st.main_context.context,
        signature: signature.to_vec(),
        args: signature[1..].iter().map(ValueKind::of_ret).collect(),
        ret,
        ret_size: st.layout(&signature[0])?.size,
        slots,
        state: st as *const State,
        closure: None,
        method,
        handler,
    });
    let userdata = &mut *data as *mut ClosureData as *mut c_void;
    #[cfg(target_arch = "x86_64")]
    if registers.is_some() {
        data.closure =
            RegisterClosure::new(register_trampoline, userdata).map(ClosureCode::Registers);
    }
    if data.closure.is_none() {
        let closure = Closure::new(st, signature, closure_trampoline, userdata)?;
        data.closure = Some(ClosureCode::Ffi(closure));
    }
    let ptr = &*data as *const ClosureData;
    st.closures.borrow_mut().push(data);
    Ok(ptr)
}

/// C function pointer for a vfunc slot: calls `method` on the JS object of its first
/// argument, the instance. Lives as long as the runtime, like the class.
pub fn method_closure<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    method: v8::Local<'s, v8::Function>,
    signature: &[Type],
    dispose: bool,
) -> Result<usize, String> {
    new_closure_data(scope, st, method, signature, Some(dispose), false).map(|d| unsafe { (*d).code() })
}

/// A closure of its own for one signal connection: (C function pointer, closure data
/// for [`release_handler`]). Connecting a function twice mustn't share one, as either
/// disconnect frees it.
pub fn handler_closure<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    function: v8::Local<'s, v8::Function>,
    signature: &[Type],
) -> Result<(usize, usize), String> {
    let data = new_closure_data(scope, st, function, signature, None, true)?;
    Ok((unsafe { (*data).code() }, data as usize))
}

/// Frees a signal handler's closure: GLib finalized its GClosure.
pub fn release_handler(st: &State, data: usize) {
    let released = {
        let mut closures = st.closures.borrow_mut();
        let i = closures.iter().position(|c| &**c as *const ClosureData as usize == data);
        i.map(|i| closures.swap_remove(i))
    };
    if released.is_some() {
        st.closure_generation.set(st.closure_generation.get() + 1);
    }
}

/// C function pointer calling `function`; cached per signature, freed by `interop.release`.
pub fn closure_for<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    function: v8::Local<'s, v8::Function>,
    signature: &[Type],
) -> Result<usize, String> {
    closure_data_for(scope, st, function, signature).map(|d| unsafe { (*d).code() })
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
            // vfuncs live as long as their class, handlers as long as their connection
            if closures[i].method.is_none() && !closures[i].handler && f.strict_equals(function.into()) {
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
    if !released.is_empty() {
        st.closure_generation.set(st.closure_generation.get() + 1);
    }
    released.len()
}

unsafe extern "C" fn closure_trampoline(
    _cif: *mut raw::ffi_cif,
    ret: *mut c_void,
    args: *mut *mut c_void,
    userdata: *mut c_void,
) {
    let data = unsafe { &*(userdata as *const ClosureData) };
    let st = unsafe { &*data.state };
    thread_local!(static THREAD: std::thread::ThreadId = std::thread::current().id());
    if THREAD.with(|t| *t) != st.thread {
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
    unsafe { invoke_js(data, ret, args) };
    crate::event_loop::schedule_microtasks(st);
}

/// Register closure entry: points libffi-style argument pointers at the saved registers.
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn register_trampoline(
    userdata: *mut c_void,
    regs: *mut Registers,
    _stack: *const u64,
) {
    let data = unsafe { &*(userdata as *const ClosureData) };
    let regs = unsafe { &mut *regs };
    regs.ret = [0; 2];
    regs.fret = [0.0; 2];
    let mut args = [std::ptr::null_mut::<c_void>(); 14];
    for (i, &(float, k)) in data.slots.iter().enumerate() {
        args[i] = if float {
            &mut regs.floats[k as usize] as *mut f64 as *mut c_void
        } else {
            &mut regs.ints[k as usize] as *mut u64 as *mut c_void
        };
    }
    let ret = if data.ret.is_float() {
        regs.fret.as_mut_ptr() as *mut c_void
    } else {
        regs.ret.as_mut_ptr() as *mut c_void
    };
    unsafe { closure_trampoline(std::ptr::null_mut(), ret, args.as_mut_ptr(), userdata) };
}

unsafe fn invoke_js(data: &ClosureData, ret: *mut c_void, args: *mut *mut c_void) {
    let st = unsafe { &*data.state };
    // libffi expects integral returns widened to ffi_arg
    unsafe {
        std::ptr::write_bytes(
            ret as *mut u8,
            0,
            data.ret_size.max(std::mem::size_of::<usize>()),
        )
    };
    let Some(context) = st.context_local() else {
        return;
    };
    let isolate = unsafe { &mut *st.isolate.get() };
    v8::scope!(let _handles, isolate);
    // C usually calls back from inside a JS call, with the context entered;
    // Function::Call enters it when it isn't
    v8::callback_scope!(unsafe scope, context);
    v8::tc_scope!(let tc, scope);

    let undefined: v8::Local<v8::Value> = v8::undefined(tc).into();
    let mut recv = undefined;
    let mut first = 0;
    let mut disposing = None;
    if let Some(dispose) = data.method {
        let address = unsafe { (*args as *const usize).read_unaligned() };
        if dispose {
            crate::gobject::enter_dispose(st, address);
            disposing = Some(address);
        }
        match crate::gobject::instance(tc, st, address, dispose) {
            Some(object) => recv = object.into(),
            None => {
                if let Some(a) = disposing {
                    crate::gobject::leave_dispose(st, a);
                }
                return;
            }
        }
        first = 1;
    }
    let argc = data.signature.len() - 1 - first;
    let mut inline = [undefined; 8];
    let mut heap = Vec::new();
    let argv = if argc <= inline.len() {
        &mut inline[..argc]
    } else {
        heap.resize(argc, undefined);
        &mut heap[..]
    };
    for (i, ty) in data.signature[1 + first..].iter().enumerate() {
        let src = unsafe { *args.add(i + first) } as *const u8;
        argv[i] = match data.args[i + first] {
            ValueKind::Pointer if crate::gobject::pointee(ty).is_none() => {
                match unsafe { (src as *const usize).read_unaligned() } {
                    0 => v8::null(tc).into(),
                    a => crate::interop::new_pointer(tc, st, a, None).into(),
                }
            }
            ValueKind::I32 => {
                v8::Integer::new(tc, unsafe { (src as *const i32).read_unaligned() }).into()
            }
            _ => match unsafe { marshal::read(tc, st, ty, src, Mem::Temp) } {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[nativescript] callback argument {}: {e}", i + 1);
                    return;
                }
            },
        };
    }
    let function = unsafe { &*data.function_slot };
    let called = function.call(tc, recv, argv);
    if let Some(a) = disposing {
        crate::gobject::leave_dispose(st, a);
    }
    // an empty result means an exception or termination
    let Some(result) = called else {
        if !tc.has_terminated() {
            if let Some(exception) = tc.exception() {
                let message = tc.message();
                report_exception(tc, exception, message);
                st.uncaught.set(st.uncaught.get() + 1);
            }
        }
        return;
    };
    if data.ret == ValueKind::Void {
        return;
    }
    let isolate: *const v8::Isolate = {
        let i: &v8::Isolate = tc;
        i
    };
    let mut strings = Strings::new();
    // integers as full width ffi_arg values
    if let Some(bits) = fast::simple_arg(st.tagged(), || isolate, data.ret, result, &mut strings) {
        let ret = ret as *mut u8;
        unsafe {
            match data.ret {
                ValueKind::F32 => (ret as *mut u32).write_unaligned(bits as u32),
                _ => (ret as *mut u64).write_unaligned(bits),
            }
        }
        return;
    }
    let ret_ty = &data.signature[0];
    let mut cx = CallCx::default();
    let res = match data.ret {
        ValueKind::I8 | ValueKind::I16 | ValueKind::I32 | ValueKind::I64 | ValueKind::Bool => {
            marshal::to_i64(tc, result, ret_ty)
                .map(|v| unsafe { (ret as *mut i64).write_unaligned(data.ret.extend(v)) })
        }
        ValueKind::U8 | ValueKind::U16 | ValueKind::U32 => marshal::to_i64(tc, result, ret_ty)
            .map(|v| unsafe { (ret as *mut u64).write_unaligned(data.ret.extend(v) as u64) }),
        _ => unsafe { marshal::write(tc, st, ret_ty, result, ret as *mut u8, &mut cx) },
    };
    if let Err(e) = res {
        eprintln!("[nativescript] callback return value: {e}");
    }
    // NOTE: temporaries (e.g. strings) returned to C would dangle; callbacks
    // returning char* must return memory that outlives the call (interop.cstring).
}
