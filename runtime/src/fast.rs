//! V8 fast API overloads for C functions: optimized code calls a per-arity trampoline
//! instead of the API callback. Args arrive untyped so every JS value takes this path.

use crate::ffi::CFunction;
use crate::handles::{self, Kind};
use crate::marshal;
use crate::throw_error;
use nativescript_metadata::Type;
use std::ffi::c_void;
use std::sync::OnceLock;
use v8::fast_api::{
    CFunction as FastFunction, CFunctionInfo, CTypeInfo, FastApiCallbackOptions,
    Int64Representation, Type as CType,
};

pub const MAX_ARGS: usize = 8;

/// Return kinds V8 can box without our help (64-bit ints, pointers and records can't
/// match the slow path's BigInt / Pointer results).
#[derive(Clone, Copy)]
enum Ret {
    Void,
    Bool,
    I32,
    U32,
    F64,
}

const RETS: [Ret; 5] = [Ret::Void, Ret::Bool, Ret::I32, Ret::U32, Ret::F64];

fn ret_kind(ty: &Type) -> Option<Ret> {
    Some(match ty {
        Type::Void => Ret::Void,
        Type::Bool => Ret::Bool,
        Type::Char | Type::UChar | Type::Short | Type::UShort | Type::Unichar | Type::Int => {
            Ret::I32
        }
        Type::UInt => Ret::U32,
        Type::Float | Type::Double => Ret::F64,
        _ => return None,
    })
}

struct Infos(Vec<CFunctionInfo>);
// written once, then only read by V8
unsafe impl Send for Infos {}
unsafe impl Sync for Infos {}

fn info(argc: usize, ret: Ret) -> &'static CFunctionInfo {
    static INFOS: OnceLock<Infos> = OnceLock::new();
    let infos = INFOS.get_or_init(|| {
        let mut infos = Vec::with_capacity((MAX_ARGS + 1) * RETS.len());
        for n in 0..=MAX_ARGS {
            // receiver, args, options
            let mut args = vec![CType::V8Value.as_info(); n + 1];
            args.push(CType::CallbackOptions.as_info());
            let args: &'static [CTypeInfo] = Box::leak(args.into_boxed_slice());
            for ret in RETS {
                let r = match ret {
                    Ret::Void => CType::Void,
                    Ret::Bool => CType::Bool,
                    Ret::I32 => CType::Int32,
                    Ret::U32 => CType::Uint32,
                    Ret::F64 => CType::Float64,
                };
                infos.push(CFunctionInfo::new(
                    r.as_info(),
                    args,
                    Int64Representation::Number,
                ));
            }
        }
        Infos(infos)
    });
    &infos.0[argc * RETS.len() + ret as usize]
}

/// Fast overload for `function`, if its signature has one.
pub fn overload(function: &CFunction) -> Option<FastFunction> {
    function.fast.as_ref()?;
    let argc = function.signature.len() - 1;
    // callback params mean JS functions (full marshalling) and C calling back into JS,
    // which costs more from inside a fast call
    let callbacks = function.signature[1..]
        .iter()
        .any(|t| matches!(t, Type::FunctionPointer(_) | Type::Block(_)));
    if argc > MAX_ARGS || callbacks {
        return None;
    }
    let ret = ret_kind(&function.signature[0])?;
    let address = if matches!(ret, Ret::F64) {
        FLOAT_TRAMPOLINES[argc]
    } else {
        INT_TRAMPOLINES[argc]
    };
    Some(FastFunction::new(address, info(argc, ret)))
}

const STRINGS: usize = 1024;

/// NUL terminated copies of string args, on the trampoline's stack.
struct Strings<'a> {
    isolate: &'a v8::Isolate,
    buf: [std::mem::MaybeUninit<u8>; STRINGS],
    used: usize,
}

impl Strings<'_> {
    fn push(&mut self, s: v8::Local<v8::String>) -> Option<usize> {
        // UTF-8 needs at most 3 bytes per UTF-16 unit
        let max = s.length() * 3;
        if self.used + max + 1 > STRINGS {
            return None;
        }
        let start = self.used;
        let out = unsafe {
            std::slice::from_raw_parts_mut(self.buf.as_mut_ptr().add(start) as *mut u8, max + 1)
        };
        let n = s.write_utf8_v2(
            self.isolate,
            &mut out[..max],
            v8::WriteFlags::kReplaceInvalidUtf8,
            None,
        );
        out[n] = 0;
        self.used += n + 1;
        Some(out.as_ptr() as usize)
    }
}

/// `char*` / `void*` params take JS strings, as in `marshal::write`.
fn takes_string(ty: &Type) -> bool {
    match ty {
        Type::CString | Type::ConstCString => true,
        Type::Pointer(inner) | Type::IncompleteArray(inner) => {
            matches!(**inner, Type::Char | Type::UChar | Type::Void)
        }
        _ => false,
    }
}

/// Register value for `v` as a `ty` argument without a scope, mirroring `marshal::write`.
/// `None`: needs the full marshalling (callbacks, plain objects, References...).
#[inline]
fn simple_arg(ty: &Type, v: v8::Local<v8::Value>, strings: &mut Strings) -> Option<u128> {
    let mut slot = 0u128;
    let dst = &mut slot as *mut u128 as *mut u8;
    macro_rules! put {
        ($t:ty, $v:expr) => {
            unsafe { (dst as *mut $t).write_unaligned($v as $t) }
        };
    }
    match ty {
        Type::Bool => {
            if !v.is_boolean() {
                return None;
            }
            put!(u8, v.is_true() as u8);
        }
        Type::Float | Type::Double => {
            let n = if let Ok(n) = v8::Local::<v8::Number>::try_from(v) {
                n.value()
            } else if let Ok(b) = v8::Local::<v8::BigInt>::try_from(v) {
                b.i64_value().0 as f64
            } else if v.is_boolean() {
                v.is_true() as u8 as f64
            } else if v.is_null() {
                0.0
            } else if v.is_undefined() {
                f64::NAN
            } else {
                return None;
            };
            if matches!(ty, Type::Float) {
                put!(f32, n);
            } else {
                put!(f64, n);
            }
        }
        Type::Char
        | Type::UChar
        | Type::Short
        | Type::UShort
        | Type::Unichar
        | Type::Int
        | Type::UInt
        | Type::Long
        | Type::LongLong
        | Type::ULong
        | Type::ULongLong => {
            let n: i64 = if let Ok(n) = v8::Local::<v8::Number>::try_from(v) {
                let n = n.value();
                if n.is_finite() {
                    n as i64
                } else {
                    0
                }
            } else if let Ok(b) = v8::Local::<v8::BigInt>::try_from(v) {
                if matches!(ty, Type::ULong | Type::ULongLong) {
                    b.u64_value().0 as i64
                } else {
                    b.i64_value().0
                }
            } else if v.is_boolean() {
                v.is_true() as i64
            } else if v.is_null_or_undefined() {
                0
            } else {
                handles::is(v, Kind::Pointer).map(handles::address)? as i64
            };
            match ty {
                Type::Char => put!(i8, n),
                Type::UChar => put!(u8, n),
                Type::Short => put!(i16, n),
                Type::UShort | Type::Unichar => put!(u16, n),
                Type::Int => put!(i32, n),
                Type::UInt => put!(u32, n),
                _ => put!(i64, n),
            }
        }
        Type::ObjC(_) => return None,
        t if t.is_pointer_like() => {
            let addr = if v.is_null_or_undefined() {
                0
            } else if let Some(addr) = marshal::buffer_address(v) {
                addr
            } else if let Ok(s) = v8::Local::<v8::String>::try_from(v) {
                if !takes_string(ty) {
                    return None;
                }
                strings.push(s)?
            } else {
                match handles::of(v)? {
                    (o, Kind::Pointer | Kind::Struct) => handles::address(o),
                    _ => return None,
                }
            };
            put!(usize, addr);
        }
        _ => return None,
    }
    Some(slot)
}

/// Return register as the fast return type's bits (`f64` bits for float returns).
#[inline]
fn fast_ret(ty: &Type, ret: u128) -> u64 {
    match ty {
        Type::Bool => (ret as u8 != 0) as u64,
        Type::Char => ret as u8 as i8 as i32 as u32 as u64,
        Type::UChar => ret as u8 as u64,
        Type::Short => ret as u16 as i16 as i32 as u32 as u64,
        Type::UShort | Type::Unichar => ret as u16 as u64,
        Type::Int | Type::UInt => ret as u32 as u64,
        Type::Float => (f32::from_bits(ret as u32) as f64).to_bits(),
        Type::Double => ret as u64,
        _ => 0,
    }
}

#[inline]
unsafe fn invoke(opts: *mut FastApiCallbackOptions, args: &[v8::Local<v8::Value>]) -> u64 {
    let opts = &*opts;
    let ext: v8::Local<v8::External> = v8::Local::cast_unchecked(opts.data);
    let function = &*(ext.value() as *const CFunction);
    let fast = function.fast.as_ref().unwrap_unchecked();
    let mut ints = [0i64; 8];
    let mut floats = [0f64; 8];
    let (mut ni, mut nf) = (0, 0);
    let mut strings = Strings {
        isolate: opts.isolate_unchecked(),
        buf: [std::mem::MaybeUninit::uninit(); STRINGS],
        used: 0,
    };
    let mut simple = true;
    for (i, ty) in function.signature[1..].iter().enumerate() {
        let Some(slot) = simple_arg(ty, args[i], &mut strings) else {
            simple = false;
            break;
        };
        if fast.float_args[i] {
            floats[nf] = f64::from_bits(slot as u64);
            nf += 1;
        } else {
            ints[ni] = slot as u64 as i64;
            ni += 1;
        }
    }
    let ret = if simple {
        (**function).call_fast(fast, ints, floats)
    } else {
        v8::callback_scope!(unsafe scope, opts);
        // into this scope's handles before anything allocates
        let locals: Vec<v8::Local<v8::Value>> =
            args.iter().map(|a| v8::Local::new(scope, *a)).collect();
        let st = &*function.state;
        match function.call_fast_raw(scope, st, fast, |i| locals[i]) {
            Ok(ret) => ret,
            Err(e) => {
                throw_error(scope, &e);
                0
            }
        }
    };
    fast_ret(&function.signature[0], ret)
}

macro_rules! trampolines {
    ($($n:literal => $int:ident, $float:ident ($($a:ident),*);)*) => {
        $(
            unsafe extern "C" fn $int(
                _recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
            ) -> u64 {
                unsafe { invoke(opts, &[$($a),*]) }
            }
            unsafe extern "C" fn $float(
                _recv: v8::Local<v8::Value>,
                $($a: v8::Local<v8::Value>,)*
                opts: *mut FastApiCallbackOptions,
            ) -> f64 {
                f64::from_bits(unsafe { invoke(opts, &[$($a),*]) })
            }
        )*
        const INT_TRAMPOLINES: [*const c_void; MAX_ARGS + 1] = [$($int as *const c_void),*];
        const FLOAT_TRAMPOLINES: [*const c_void; MAX_ARGS + 1] = [$($float as *const c_void),*];
    };
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
    if let Some(o) = handles::is(v, Kind::Pointer) {
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
    let addr = handles::is(recv, Kind::Pointer).map(handles::address);
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
    this_pointer(recv, opts).map_or(0.0, |a| a as f64)
}

unsafe extern "C" fn pointer_to_bigint(
    recv: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) -> u64 {
    this_pointer(recv, opts).map_or(0, |a| a as u64)
}

unsafe extern "C" fn pointer_equals(
    recv: v8::Local<v8::Value>,
    other: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) -> bool {
    this_pointer(recv, opts).is_some_and(|a| address_of(other) == Some(a))
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
