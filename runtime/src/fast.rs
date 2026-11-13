//! V8 fast API overloads for C functions: optimized code calls a trampoline instead
//! of the API callback. `double` / `float` params are typed (V8 converts numbers and
//! deopts on anything else), the rest arrive as values and convert here.

use crate::ffi::CFunction;
use crate::handles::{self, Kind};
use crate::marshal::{self, CallCx};
use crate::tagged::{self, Layout, Value};
use crate::throw_error;
use nativescript_core::call::{FloatRegs, IntRegs, ValueKind, FLOAT_REGS, INT_REGS};
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::sync::{Mutex, OnceLock};
use v8::fast_api::{
    CFunction as FastFunction, CFunctionInfo, CTypeInfo, FastApiCallbackOptions,
    Int64Representation, Type as CType,
};

/// Return kinds V8 can box without our help (64-bit ints, pointers and records can't
/// match the slow path's BigInt / Pointer results).
fn ret_type(kind: ValueKind) -> Option<CType> {
    Some(match kind {
        ValueKind::Void => CType::Void,
        ValueKind::Bool => CType::Bool,
        ValueKind::I8 | ValueKind::U8 | ValueKind::I16 | ValueKind::U16 | ValueKind::I32 => {
            CType::Int32
        }
        ValueKind::U32 => CType::Uint32,
        ValueKind::F32 | ValueKind::F64 => CType::Float64,
        _ => return None,
    })
}

fn arg_type(kind: ValueKind) -> CType {
    match kind {
        ValueKind::F64 => CType::Float64,
        ValueKind::F32 => CType::Float32,
        _ => CType::V8Value,
    }
}

struct Info(&'static CFunctionInfo);
// written once, then only read by V8
unsafe impl Send for Info {}
unsafe impl Sync for Info {}

/// One leaked `CFunctionInfo` per distinct signature.
pub fn info(args: &[ValueKind], ret: CType) -> &'static CFunctionInfo {
    static INFOS: OnceLock<Mutex<HashMap<Vec<u8>, Info>>> = OnceLock::new();
    let mut key: Vec<u8> = args.iter().map(|k| arg_type(*k) as u8).collect();
    key.push(ret as u8);
    let mut infos = INFOS.get_or_init(Default::default).lock().unwrap();
    infos
        .entry(key)
        .or_insert_with(|| {
            // receiver, args, options
            let mut types = vec![CType::V8Value.as_info()];
            types.extend(args.iter().map(|k| arg_type(*k).as_info()));
            types.push(CType::CallbackOptions.as_info());
            let types: &'static [CTypeInfo] = Box::leak(types.into_boxed_slice());
            Info(Box::leak(Box::new(CFunctionInfo::new(
                ret.as_info(),
                types,
                Int64Representation::Number,
            ))))
        })
        .0
}

/// Fast overload for `function`, if its signature has one.
pub fn overload(function: &CFunction) -> Option<FastFunction> {
    function.fast.as_ref()?;
    // callback params mean JS functions (full marshalling) and C calling back into JS,
    // which costs more from inside a fast call
    if function
        .args
        .iter()
        .any(|k| matches!(k, ValueKind::Other | ValueKind::Callback))
    {
        return None;
    }
    let ret = ret_type(function.ret)?;
    let ints = function.int_args.len();
    let address = match (ints == function.args.len(), function.ret.is_float()) {
        (true, false) => INTS_INT_TRAMPOLINES[ints],
        (true, true) => INTS_FLOAT_TRAMPOLINES[ints],
        (false, false) => INT_TRAMPOLINES[ints],
        (false, true) => FLOAT_TRAMPOLINES[ints],
    };
    // per function, for CPU profiles (see crate::thunks)
    let address = crate::thunks::entry(address, function.address);
    Some(FastFunction::new(address, info(&function.args, ret)))
}

/// Fast overload of `function` as a method: the receiver is its first argument.
pub fn method_overload(function: &CFunction) -> Option<FastFunction> {
    function.fast.as_ref()?;
    if function.args.first() != Some(&ValueKind::Pointer)
        || function
            .args
            .iter()
            .any(|k| matches!(k, ValueKind::Other | ValueKind::Callback))
    {
        return None;
    }
    let ret = ret_type(function.ret)?;
    let ints = function.int_args.len() - 1;
    let address = match (ints + 1 == function.args.len(), function.ret.is_float()) {
        (true, false) => METHOD_INTS_INT_TRAMPOLINES[ints],
        (true, true) => METHOD_INTS_FLOAT_TRAMPOLINES[ints],
        (false, false) => METHOD_INT_TRAMPOLINES[ints],
        (false, true) => METHOD_FLOAT_TRAMPOLINES[ints],
    };
    let address = crate::thunks::entry(address, function.address);
    Some(FastFunction::new(address, info(&function.args[1..], ret)))
}

const STRINGS: usize = 1024;

/// NUL terminated copies of string args, on the caller's stack.
pub struct Strings {
    buf: [MaybeUninit<u8>; STRINGS],
    used: usize,
}

impl Strings {
    #[inline(always)]
    pub fn new() -> Self {
        Strings {
            buf: [MaybeUninit::uninit(); STRINGS],
            used: 0,
        }
    }

    /// UTF-8 copy of Latin-1 characters, `None` if it doesn't fit.
    #[inline]
    fn push_latin1(&mut self, chars: &[u8]) -> Option<u64> {
        let n = chars.len();
        if self.used + 2 * n + 1 > STRINGS {
            return None;
        }
        let start = unsafe { self.buf.as_mut_ptr().add(self.used) as *mut u8 };
        let mut len = n;
        if chars.is_ascii() {
            unsafe { std::ptr::copy_nonoverlapping(chars.as_ptr(), start, n) };
        } else {
            len = 0;
            for &c in chars {
                if c < 0x80 {
                    unsafe { *start.add(len) = c };
                    len += 1;
                } else {
                    unsafe {
                        *start.add(len) = 0xc0 | (c >> 6);
                        *start.add(len + 1) = 0x80 | (c & 0x3f);
                    }
                    len += 2;
                }
            }
        }
        unsafe { *start.add(len) = 0 };
        self.used += len + 1;
        Some(start as u64)
    }

    /// Copy of a flat string (no handles created), `None` if it doesn't fit.
    #[inline]
    fn push(&mut self, isolate: &v8::Isolate, v: v8::Local<v8::Value>) -> Option<u64> {
        let s: v8::Local<v8::String> = unsafe { v8::Local::cast_unchecked(v) };
        // UTF-8 needs at most 3 bytes per UTF-16 unit
        let max = s.length() * 3;
        if self.used + max + 1 > STRINGS {
            return None;
        }
        let out = unsafe {
            std::slice::from_raw_parts_mut(self.buf.as_mut_ptr().add(self.used) as *mut u8, max + 1)
        };
        let n = s.write_utf8_v2(
            isolate,
            &mut out[..max],
            v8::WriteFlags::kReplaceInvalidUtf8,
            None,
        );
        out[n] = 0;
        self.used += n + 1;
        Some(out.as_ptr() as u64)
    }
}

/// [`tagged::Value`] through the V8 API (no layout): strings always need the full path.
fn classify_api(v: v8::Local<v8::Value>) -> Value {
    if let Ok(n) = v8::Local::<v8::Number>::try_from(v) {
        Value::Number(n.value())
    } else if v.is_boolean() {
        Value::Bool(v.is_true())
    } else if v.is_null() {
        Value::Null
    } else if v.is_undefined() {
        Value::Undefined
    } else if let Some(o) = handles::is(v, Kind::Pointer) {
        Value::Pointer(handles::address(o))
    } else {
        Value::Other
    }
}

/// Pointer-like values the layout doesn't recognize: struct views and buffers whose
/// memory is off the V8 heap.
#[inline]
fn other_address(v: v8::Local<v8::Value>) -> Option<u64> {
    if !v.is_object() {
        return None;
    }
    if let Some(addr) = marshal::buffer_address_in_place(v) {
        return addr.map(|a| a as u64);
    }
    match handles::of(v)? {
        (o, Kind::Pointer | Kind::Struct) => Some(handles::address(o) as u64),
        _ => None,
    }
}

/// Register value of `v` as a `kind` argument, mirroring `marshal::write`, without
/// allocating, running JS or creating handles. `None`: needs the full conversion.
#[inline(always)]
pub fn simple_arg(
    layout: Option<&Layout>,
    isolate: impl FnOnce() -> *const v8::Isolate,
    kind: ValueKind,
    v: v8::Local<v8::Value>,
    strings: &mut Strings,
) -> Option<u64> {
    if let Some(l) = layout {
        if kind.is_integer() {
            if let Some(n) = Layout::int(v) {
                return Some(kind.extend(n as i64) as u64);
            }
        } else if matches!(kind, ValueKind::Pointer | ValueKind::CString) {
            if let Some(a) = l.pointer(v) {
                return Some(a as u64);
            }
        }
    }
    simple_arg_general(layout, isolate(), kind, v, strings)
}

#[inline(never)]
fn simple_arg_general(
    layout: Option<&Layout>,
    isolate: *const v8::Isolate,
    kind: ValueKind,
    v: v8::Local<v8::Value>,
    strings: &mut Strings,
) -> Option<u64> {
    let class = match layout {
        Some(l) => l.classify(v),
        None => classify_api(v),
    };
    match kind {
        ValueKind::Bool => match class {
            Value::Bool(b) => Some(b as u64),
            Value::Int(n) => Some((n != 0) as u64),
            Value::Number(n) => Some((n != 0.0 && !n.is_nan()) as u64),
            Value::Null | Value::Undefined => Some(0),
            Value::Pointer(_) => Some(1),
            _ => None,
        },
        ValueKind::I8
        | ValueKind::U8
        | ValueKind::I16
        | ValueKind::U16
        | ValueKind::I32
        | ValueKind::U32
        | ValueKind::I64
        | ValueKind::U64 => {
            let n = match class {
                Value::Int(n) => n as i64,
                Value::Number(n) if n.is_finite() => n as i64,
                Value::Number(_) | Value::Null | Value::Undefined => 0,
                Value::Bool(b) => b as i64,
                Value::Pointer(a) => a as i64,
                Value::FlatString => return None,
                Value::Other => {
                    let b = v8::Local::<v8::BigInt>::try_from(v).ok()?;
                    if kind == ValueKind::U64 {
                        b.u64_value().0 as i64
                    } else {
                        b.i64_value().0
                    }
                }
            };
            Some(kind.extend(n) as u64)
        }
        ValueKind::F32 | ValueKind::F64 => {
            let n = match class {
                Value::Int(n) => n as f64,
                Value::Number(n) => n,
                Value::Bool(b) => b as u8 as f64,
                Value::Null => 0.0,
                Value::Undefined => f64::NAN,
                Value::Other => v8::Local::<v8::BigInt>::try_from(v).ok()?.i64_value().0 as f64,
                _ => return None,
            };
            Some(if kind == ValueKind::F32 {
                (n as f32).to_bits() as u64
            } else {
                n.to_bits()
            })
        }
        ValueKind::Pointer | ValueKind::CString => match class {
            Value::Null | Value::Undefined => Some(0),
            Value::Pointer(a) => Some(a as u64),
            Value::FlatString if kind == ValueKind::CString => {
                match layout.and_then(|l| l.latin1(v)) {
                    Some(chars) => strings.push_latin1(chars),
                    None => strings.push(unsafe { &*isolate }, v),
                }
            }
            Value::Other => other_address(v),
            _ => None,
        },
        _ => None,
    }
}

/// Return register as the fast return type's bits (`f64` bits for float returns).
#[inline(always)]
pub fn fast_ret(kind: ValueKind, ret: u128) -> u64 {
    match kind {
        ValueKind::Bool => (ret as u8 != 0) as u64,
        ValueKind::I8 => ret as u8 as i8 as i32 as u32 as u64,
        ValueKind::U8 => ret as u8 as u64,
        ValueKind::I16 => ret as u16 as i16 as i32 as u32 as u64,
        ValueKind::U16 => ret as u16 as u64,
        ValueKind::I32 | ValueKind::U32 => ret as u32 as u64,
        ValueKind::F32 => (f32::from_bits(ret as u32) as f64).to_bits(),
        ValueKind::F64 => ret as u64,
        _ => 0,
    }
}

/// `floats`: the typed float params, `None` when the signature has none.
#[inline(always)]
unsafe fn invoke(
    opts: *mut FastApiCallbackOptions,
    args: &[v8::Local<v8::Value>],
    floats: Option<FloatRegs>,
) -> u64 {
    let opts = unsafe { &*opts };
    let function = unsafe { &*(tagged::external_value(opts.data) as *const CFunction) };
    let st = unsafe { &*function.state };
    let layout = st.tagged();
    let mut ints: IntRegs = [0; INT_REGS];
    let mut strings = Strings::new();
    for (k, &(_, kind)) in function.int_args.iter().enumerate() {
        let isolate = || unsafe { opts.isolate_unchecked() } as *const v8::Isolate;
        match simple_arg(layout, isolate, kind, args[k], &mut strings) {
            Some(v) => ints[k] = v as i64,
            None => {
                let floats = floats.unwrap_or([0.0; FLOAT_REGS]);
                return unsafe { invoke_full(opts, function, args, floats) };
            }
        }
    }
    let fast = unsafe { function.fast.as_ref().unwrap_unchecked() };
    let ret = match &floats {
        Some(floats) => unsafe { function.call_fast(fast, &ints, floats) },
        None => unsafe { function.call_ints(fast, &ints) },
    };
    fast_ret(function.ret, ret)
}

/// Full conversion inside the fast call: References, plain objects for struct
/// pointers, strings to flatten, errors.
#[cold]
#[inline(never)]
unsafe fn invoke_full(
    opts: &FastApiCallbackOptions,
    function: &CFunction,
    args: &[v8::Local<v8::Value>],
    floats: FloatRegs,
) -> u64 {
    v8::callback_scope!(unsafe scope, opts);
    // into this scope's handles before anything allocates
    let undefined: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut locals = [undefined; INT_REGS];
    for (k, a) in args.iter().enumerate() {
        locals[k] = v8::Local::new(scope, *a);
    }
    let st = unsafe { &*function.state };
    let mut cx = CallCx::default();
    let mut ints: IntRegs = [0; INT_REGS];
    for (k, &(index, kind)) in function.int_args.iter().enumerate() {
        let mut slot = 0u128;
        let ty = &function.signature[index + 1];
        let dst = &mut slot as *mut u128 as *mut u8;
        if let Err(e) = unsafe { marshal::write(scope, st, ty, locals[k], dst, &mut cx) } {
            throw_error(
                scope,
                &format!("{}() argument {}: {e}", function.name, index + 1),
            );
            return 0;
        }
        ints[k] = kind.extend(slot as u64 as i64);
    }
    let fast = unsafe { function.fast.as_ref().unwrap_unchecked() };
    let ret = unsafe { function.call_fast(fast, &ints, &floats) };
    if let Err(e) = cx.finish(scope, st) {
        throw_error(scope, &e);
    }
    fast_ret(function.ret, ret)
}

// receiver, the integer-register params as values, options, then the typed float
// params, which V8 passes in float registers whatever their position
macro_rules! trampolines {
    ($($n:literal => $int:ident, $float:ident ($($a:ident),*);)*) => {
        $(
            unsafe extern "C" fn $int(
                _recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
                f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64,
            ) -> u64 {
                unsafe { invoke(opts, &[$($a),*], Some([f0, f1, f2, f3, f4, f5, f6, f7])) }
            }
            unsafe extern "C" fn $float(
                _recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
                f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64,
            ) -> f64 {
                f64::from_bits(unsafe {
                    invoke(opts, &[$($a),*], Some([f0, f1, f2, f3, f4, f5, f6, f7]))
                })
            }
        )*
        const INT_TRAMPOLINES: [*const c_void; 9] = [$($int as *const c_void),*];
        const FLOAT_TRAMPOLINES: [*const c_void; 9] = [$($float as *const c_void),*];
    };
}

// signatures without float params
macro_rules! int_trampolines {
    ($($n:literal => $int:ident, $float:ident ($($a:ident),*);)*) => {
        $(
            unsafe extern "C" fn $int(
                _recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
            ) -> u64 {
                unsafe { invoke(opts, &[$($a),*], None) }
            }
            unsafe extern "C" fn $float(
                _recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
            ) -> f64 {
                f64::from_bits(unsafe { invoke(opts, &[$($a),*], None) })
            }
        )*
        const INTS_INT_TRAMPOLINES: [*const c_void; 9] = [$($int as *const c_void),*];
        const INTS_FLOAT_TRAMPOLINES: [*const c_void; 9] = [$($float as *const c_void),*];
    };
}

int_trampolines! {
    0 => ints_int0, ints_float0 ();
    1 => ints_int1, ints_float1 (a);
    2 => ints_int2, ints_float2 (a, b);
    3 => ints_int3, ints_float3 (a, b, c);
    4 => ints_int4, ints_float4 (a, b, c, d);
    5 => ints_int5, ints_float5 (a, b, c, d, e);
    6 => ints_int6, ints_float6 (a, b, c, d, e, f);
    7 => ints_int7, ints_float7 (a, b, c, d, e, f, g);
    8 => ints_int8, ints_float8 (a, b, c, d, e, f, g, h);
}

trampolines! {
    0 => int0, float0 ();
    1 => int1, float1 (a);
    2 => int2, float2 (a, b);
    3 => int3, float3 (a, b, c);
    4 => int4, float4 (a, b, c, d);
    5 => int5, float5 (a, b, c, d, e);
    6 => int6, float6 (a, b, c, d, e, f);
    7 => int7, float7 (a, b, c, d, e, f, g);
    8 => int8, float8 (a, b, c, d, e, f, g, h);
}

// methods: the receiver is the C function's first argument
macro_rules! method_trampolines {
    ($($n:literal => $int:ident, $float:ident, $ints_int:ident, $ints_float:ident ($($a:ident),*);)*) => {
        $(
            unsafe extern "C" fn $int(
                recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
                f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64,
            ) -> u64 {
                unsafe { invoke(opts, &[recv, $($a),*], Some([f0, f1, f2, f3, f4, f5, f6, f7])) }
            }
            unsafe extern "C" fn $float(
                recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
                f0: f64, f1: f64, f2: f64, f3: f64, f4: f64, f5: f64, f6: f64, f7: f64,
            ) -> f64 {
                f64::from_bits(unsafe {
                    invoke(opts, &[recv, $($a),*], Some([f0, f1, f2, f3, f4, f5, f6, f7]))
                })
            }
            unsafe extern "C" fn $ints_int(
                recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
            ) -> u64 {
                unsafe { invoke(opts, &[recv, $($a),*], None) }
            }
            unsafe extern "C" fn $ints_float(
                recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
            ) -> f64 {
                f64::from_bits(unsafe { invoke(opts, &[recv, $($a),*], None) })
            }
        )*
        const METHOD_INT_TRAMPOLINES: [*const c_void; 8] = [$($int as *const c_void),*];
        const METHOD_FLOAT_TRAMPOLINES: [*const c_void; 8] = [$($float as *const c_void),*];
        const METHOD_INTS_INT_TRAMPOLINES: [*const c_void; 8] = [$($ints_int as *const c_void),*];
        const METHOD_INTS_FLOAT_TRAMPOLINES: [*const c_void; 8] = [$($ints_float as *const c_void),*];
    };
}

method_trampolines! {
    0 => method_int0, method_float0, method_ints_int0, method_ints_float0 ();
    1 => method_int1, method_float1, method_ints_int1, method_ints_float1 (a);
    2 => method_int2, method_float2, method_ints_int2, method_ints_float2 (a, b);
    3 => method_int3, method_float3, method_ints_int3, method_ints_float3 (a, b, c);
    4 => method_int4, method_float4, method_ints_int4, method_ints_float4 (a, b, c, d);
    5 => method_int5, method_float5, method_ints_int5, method_ints_float5 (a, b, c, d, e);
    6 => method_int6, method_float6, method_ints_int6, method_ints_float6 (a, b, c, d, e, f);
    7 => method_int7, method_float7, method_ints_int7, method_ints_float7 (a, b, c, d, e, f, g);
}

struct StaticInfo(CFunctionInfo);
// immutable, only read by V8
unsafe impl Sync for StaticInfo {}

const RECV: [CTypeInfo; 2] = [CType::V8Value.as_info(), CType::CallbackOptions.as_info()];
const RECV_ARG: [CTypeInfo; 3] = [
    CType::V8Value.as_info(),
    CType::V8Value.as_info(),
    CType::CallbackOptions.as_info(),
];
static TO_NUMBER: StaticInfo = StaticInfo(CFunctionInfo::new(
    CType::Float64.as_info(),
    &RECV,
    Int64Representation::Number,
));
static TO_BIGINT: StaticInfo = StaticInfo(CFunctionInfo::new(
    CType::Uint64.as_info(),
    &RECV,
    Int64Representation::BigInt,
));
static EQUALS: StaticInfo = StaticInfo(CFunctionInfo::new(
    CType::Bool.as_info(),
    &RECV_ARG,
    Int64Representation::Number,
));

/// Address of a Pointer / struct view, number, bigint or null (`None`: anything else).
#[inline]
pub fn address_of(v: v8::Local<v8::Value>) -> Option<usize> {
    if v.is_null_or_undefined() {
        return Some(0);
    }
    if let Some((o, Kind::Pointer | Kind::Struct)) = handles::of(v) {
        return Some(handles::address(o));
    }
    if let Ok(b) = v8::Local::<v8::BigInt>::try_from(v) {
        return Some(b.u64_value().0 as usize);
    }
    v8::Local::<v8::Number>::try_from(v)
        .ok()
        .map(|n| n.value() as usize)
}

#[inline]
unsafe fn this_pointer(
    recv: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) -> Option<usize> {
    let addr = match handles::of(recv) {
        Some((o, Kind::Pointer | Kind::Struct)) => Some(handles::address(o)),
        _ => None,
    };
    if addr.is_none() {
        v8::callback_scope!(unsafe scope, &*opts);
        throw_error(scope, "not an interop.Pointer");
    }
    addr
}

unsafe extern "C" fn pointer_to_number(
    recv: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) -> f64 {
    unsafe { this_pointer(recv, opts) }.map_or(0.0, |a| a as f64)
}

unsafe extern "C" fn pointer_to_bigint(
    recv: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) -> u64 {
    unsafe { this_pointer(recv, opts) }.map_or(0, |a| a as u64)
}

unsafe extern "C" fn pointer_equals(
    recv: v8::Local<v8::Value>,
    other: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) -> bool {
    unsafe { this_pointer(recv, opts) }.is_some_and(|a| address_of(other) == Some(a))
}

/// Fast overload of an `interop.Pointer` method, by name.
pub fn pointer_method(name: &str) -> Option<FastFunction> {
    let (address, info): (*const c_void, &'static StaticInfo) = match name {
        "toNumber" => (pointer_to_number as *const c_void, &TO_NUMBER),
        "toBigInt" => (pointer_to_bigint as *const c_void, &TO_BIGINT),
        "equals" => (pointer_equals as *const c_void, &EQUALS),
        _ => return None,
    };
    Some(FastFunction::new(address, &info.0))
}
