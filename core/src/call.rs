//! Prepared C calls: libffi cif plus register-only paths.

use crate::native::Native;
use libffi::middle::Cif;
use libffi::raw;
use nativescript_metadata::Type;
use std::ffi::c_void;

/// How a value crosses the call boundary, decided once per signature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueKind {
    Void,
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    /// Pointer, struct view, Reference, buffer or null
    Pointer,
    /// `char*` / `void*`: also JS strings
    CString,
    /// `const char*` results, read into JS strings
    ConstCString,
    /// function pointer arguments: JS functions become closures
    Callback,
    /// records, function pointers, arrays, long double...: full conversion
    Other,
}

impl ValueKind {
    fn scalar(ty: &Type) -> Option<Self> {
        Some(match ty {
            Type::Void => ValueKind::Void,
            Type::Bool => ValueKind::Bool,
            Type::Char => ValueKind::I8,
            Type::UChar => ValueKind::U8,
            Type::Short => ValueKind::I16,
            Type::UShort | Type::Unichar => ValueKind::U16,
            Type::Int => ValueKind::I32,
            Type::UInt => ValueKind::U32,
            Type::Long | Type::LongLong => ValueKind::I64,
            Type::ULong | Type::ULongLong => ValueKind::U64,
            Type::Float => ValueKind::F32,
            Type::Double => ValueKind::F64,
            _ => return None,
        })
    }

    pub fn of_arg(ty: &Type) -> Self {
        if let Some(k) = Self::scalar(ty) {
            return k;
        }
        match ty {
            Type::CString | Type::ConstCString => ValueKind::CString,
            Type::FunctionPointer(_) | Type::Block(_) => ValueKind::Callback,
            Type::Pointer(inner) | Type::IncompleteArray(inner) => {
                if matches!(**inner, Type::Char | Type::UChar | Type::Void) {
                    ValueKind::CString
                } else {
                    ValueKind::Pointer
                }
            }
            _ => ValueKind::Other,
        }
    }

    pub fn of_ret(ty: &Type) -> Self {
        if let Some(k) = Self::scalar(ty) {
            return k;
        }
        match ty {
            Type::ConstCString => ValueKind::ConstCString,
            Type::FunctionPointer(_) => ValueKind::Other,
            t if t.is_pointer_like() => ValueKind::Pointer,
            _ => ValueKind::Other,
        }
    }

    #[inline]
    pub fn is_float(self) -> bool {
        matches!(self, ValueKind::F32 | ValueKind::F64)
    }

    #[inline]
    pub fn is_integer(self) -> bool {
        matches!(
            self,
            ValueKind::I8
                | ValueKind::U8
                | ValueKind::I16
                | ValueKind::U16
                | ValueKind::I32
                | ValueKind::U32
                | ValueKind::I64
                | ValueKind::U64
        )
    }

    /// Register value of an integer argument, extended to 64 bits as C callers do
    /// (clang relies on the caller extending 8 and 16-bit arguments).
    #[inline]
    pub fn extend(self, v: i64) -> i64 {
        match self {
            ValueKind::Bool => (v != 0) as i64,
            ValueKind::I8 => v as i8 as i64,
            ValueKind::U8 => v as u8 as i64,
            ValueKind::I16 => v as i16 as i64,
            ValueKind::U16 => v as u16 as i64,
            ValueKind::I32 => v as i32 as i64,
            ValueKind::U32 => v as u32 as i64,
            _ => v,
        }
    }
}

/// A C function with a prepared call interface.
pub struct CallDesc {
    pub name: String,
    pub address: usize,
    /// return type first
    pub signature: Vec<Type>,
    pub variadic: bool,
    /// prepared once for non variadic functions
    cif: Option<Cif>,
    /// argument slot sizes (non variadic)
    pub slots: Vec<usize>,
    pub ret_size: usize,
    pub fast: Option<FastCall>,
    /// fixed arguments
    pub args: Vec<ValueKind>,
    pub ret: ValueKind,
}

/// Int and float args use separate register sequences (x86_64 SysV, AArch64): one
/// fn type taking all registers calls any signature that fits in them.
#[derive(Clone)]
pub struct FastCall {
    /// per argument: true = float register
    pub float_args: Vec<bool>,
    pub float_ret: bool,
    /// integer-only record of up to 16 bytes, returned in two registers
    pub pair_ret: bool,
}

#[cfg(target_arch = "x86_64")]
pub const INT_REGS: usize = 6;
#[cfg(target_arch = "aarch64")]
pub const INT_REGS: usize = 8;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub const INT_REGS: usize = 0;
pub const FLOAT_REGS: usize = 8;

pub type IntRegs = [i64; INT_REGS];
pub type FloatRegs = [f64; FLOAT_REGS];

macro_rules! register_fns {
    ($($i:ident),*) => {
        type IntFn = unsafe extern "C" fn($($i,)* f64, f64, f64, f64, f64, f64, f64, f64) -> i64;
        type PairFn = unsafe extern "C" fn($($i,)* f64, f64, f64, f64, f64, f64, f64, f64) -> u128;
        type FloatFn = unsafe extern "C" fn($($i,)* f64, f64, f64, f64, f64, f64, f64, f64) -> f64;
        // signatures without float arguments leave the float registers alone
        type IntsFn = unsafe extern "C" fn($($i),*) -> i64;
        type IntsPairFn = unsafe extern "C" fn($($i),*) -> u128;
        type IntsFloatFn = unsafe extern "C" fn($($i),*) -> f64;
    };
}
#[cfg(target_arch = "x86_64")]
register_fns!(i64, i64, i64, i64, i64, i64);
#[cfg(target_arch = "aarch64")]
register_fns!(i64, i64, i64, i64, i64, i64, i64, i64);

// x86_64 variadic callees read the number of vector registers from %al, which
// Rust sets for calls through C-variadic fn pointers
#[cfg(target_arch = "x86_64")]
type VarIntFn = unsafe extern "C" fn(i64, ...) -> i64;
#[cfg(target_arch = "x86_64")]
type VarPairFn = unsafe extern "C" fn(i64, ...) -> u128;
#[cfg(target_arch = "x86_64")]
type VarFloatFn = unsafe extern "C" fn(i64, ...) -> f64;

/// Records C returns in integer registers (rax:rdx, x0:x1): up to 16 bytes, no floats.
fn int_record(native: &Native, ty: &Type) -> bool {
    fn leaves(native: &Native, ty: &Type) -> bool {
        match ty {
            Type::Float | Type::Double | Type::LongDouble | Type::ExtVector(..) => false,
            Type::ConstantArray(_, inner) => leaves(native, inner),
            Type::Struct(_)
            | Type::Union(_)
            | Type::AnonymousStruct(..)
            | Type::AnonymousUnion(..) => native
                .record(ty)
                .is_ok_and(|r| r.fields.iter().all(|f| leaves(native, &f.ty))),
            _ => true,
        }
    }
    matches!(ty, Type::Struct(_) | Type::AnonymousStruct(..))
        && native
            .record(ty)
            .is_ok_and(|r| r.size > 0 && r.size <= 16 && r.natural)
        && leaves(native, ty)
}

/// Register class of a scalar: `Some(true)` = float register.
fn register_class(t: &Type) -> Option<bool> {
    match t {
        Type::Float | Type::Double => Some(true),
        Type::Bool
        | Type::Char
        | Type::UChar
        | Type::Short
        | Type::UShort
        | Type::Unichar
        | Type::Int
        | Type::UInt
        | Type::Long
        | Type::ULong
        | Type::LongLong
        | Type::ULongLong => Some(false),
        t if t.is_pointer_like() => Some(false),
        _ => None,
    }
}

impl FastCall {
    fn new(native: &Native, signature: &[Type]) -> Option<Self> {
        let pair_ret = int_record(native, &signature[0]);
        let float_ret = if signature[0].is_void() || pair_ret {
            false
        } else {
            register_class(&signature[0])?
        };
        let float_args = signature[1..]
            .iter()
            .map(register_class)
            .collect::<Option<Vec<_>>>()?;
        let floats = float_args.iter().filter(|f| **f).count();
        (INT_REGS > 0 && float_args.len() - floats <= INT_REGS && floats <= FLOAT_REGS).then_some(
            FastCall {
                float_args,
                float_ret,
                pair_ret,
            },
        )
    }
}

/// Arguments of type `T[N]` decay to `T*`.
fn decay(ty: &Type) -> Type {
    match ty {
        Type::ConstantArray(_, inner) => Type::Pointer(inner.clone()),
        other => other.clone(),
    }
}

impl CallDesc {
    pub fn new(
        native: &Native,
        name: &str,
        address: usize,
        signature: Vec<Type>,
        variadic: bool,
    ) -> Result<Self, String> {
        let signature: Vec<Type> = signature.iter().map(decay).collect();
        let cif = if variadic {
            None
        } else {
            Some(Self::prep(native, &signature, None)?)
        };
        let fast = if variadic {
            None
        } else {
            FastCall::new(native, &signature)
        };
        let slots = signature[1..]
            .iter()
            .map(|t| native.layout(t).map(|l| l.size.max(8)))
            .collect::<Result<_, _>>()?;
        // small integer returns are widened to ffi_arg by libffi
        let ret_size = native.layout(&signature[0])?.size.max(16);
        let args = signature[1..].iter().map(ValueKind::of_arg).collect();
        let ret = ValueKind::of_ret(&signature[0]);
        Ok(CallDesc {
            name: name.to_owned(),
            address,
            signature,
            variadic,
            cif,
            slots,
            ret_size,
            fast,
            args,
            ret,
        })
    }

    /// cif for `signature`; `fixed` = number of fixed args of a variadic call.
    pub fn prep(native: &Native, signature: &[Type], fixed: Option<usize>) -> Result<Cif, String> {
        let ret = native.ffi_type(&signature[0])?;
        let args = signature[1..]
            .iter()
            .map(|t| native.ffi_type(t))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(match fixed {
            Some(fixed) => Cif::new_variadic(args, fixed, ret),
            None => Cif::new(args, ret),
        })
    }

    pub fn cif(&self) -> Option<&Cif> {
        self.cif.as_ref()
    }

    /// Return class of a variadic call that fits in registers, `None` otherwise.
    pub fn variadic_ret(&self, native: &Native) -> Option<FastCall> {
        FastCall::new(native, &self.signature[..1])
    }

    /// libffi call; `argv` points at argument slots, `ret` at `ret_size` bytes.
    ///
    /// # Safety
    /// Slots must hold values of the signature's types.
    #[inline]
    pub unsafe fn call(&self, cif: &Cif, argv: &mut [*mut c_void], ret: *mut u8) {
        raw::ffi_call(
            cif.as_raw_ptr(),
            Some(std::mem::transmute::<usize, unsafe extern "C" fn()>(
                self.address,
            )),
            ret as *mut c_void,
            argv.as_mut_ptr(),
        );
    }

    /// Register call (see [`FastCall`]); returns the raw return register(s).
    ///
    /// # Safety
    /// Only for descriptors with `fast` set, args placed per `float_args`.
    #[inline(always)]
    pub unsafe fn call_fast(&self, fast: &FastCall, ints: &IntRegs, floats: &FloatRegs) -> u128 {
        call_registers(self.address, fast.float_ret, fast.pair_ret, ints, floats)
    }

    /// [`Self::call_fast`] for signatures without float arguments.
    ///
    /// # Safety
    /// Only for descriptors with `fast` set and no float arguments.
    #[inline(always)]
    pub unsafe fn call_ints(&self, fast: &FastCall, ints: &IntRegs) -> u128 {
        let address = self.address;
        macro_rules! call {
            ($($i:ident),*) => {{
                let [$($i),*] = *ints;
                if fast.float_ret {
                    let fun: IntsFloatFn = std::mem::transmute(address);
                    fun($($i),*).to_bits() as u128
                } else if fast.pair_ret {
                    let fun: IntsPairFn = std::mem::transmute(address);
                    fun($($i),*)
                } else {
                    let fun: IntsFn = std::mem::transmute(address);
                    fun($($i),*) as u64 as u128
                }
            }};
        }
        #[cfg(target_arch = "x86_64")]
        {
            call!(a, b, c, d, e, f)
        }
        #[cfg(target_arch = "aarch64")]
        {
            call!(a, b, c, d, e, f, g, h)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = (address, fast, ints);
            unreachable!()
        }
    }

    /// Variadic call with every argument in registers; `ret` from [`Self::variadic_ret`].
    ///
    /// # Safety
    /// Args placed in register order with C default argument promotions applied.
    #[inline]
    pub unsafe fn call_variadic(&self, ret: &FastCall, ints: &IntRegs, floats: &FloatRegs) -> u128 {
        #[cfg(target_arch = "x86_64")]
        {
            let [a, b, c, d, e, f] = *ints;
            let [p, q, r, s, t, u, v, w] = *floats;
            if ret.float_ret {
                let fun: VarFloatFn = std::mem::transmute(self.address);
                fun(a, b, c, d, e, f, p, q, r, s, t, u, v, w).to_bits() as u128
            } else if ret.pair_ret {
                let fun: VarPairFn = std::mem::transmute(self.address);
                fun(a, b, c, d, e, f, p, q, r, s, t, u, v, w)
            } else {
                let fun: VarIntFn = std::mem::transmute(self.address);
                fun(a, b, c, d, e, f, p, q, r, s, t, u, v, w) as u64 as u128
            }
        }
        // AAPCS64 (Linux) passes variadic arguments like fixed ones
        #[cfg(not(target_arch = "x86_64"))]
        call_registers(self.address, ret.float_ret, ret.pair_ret, ints, floats)
    }
}

#[inline(always)]
unsafe fn call_registers(
    address: usize,
    float_ret: bool,
    pair_ret: bool,
    ints: &IntRegs,
    floats: &FloatRegs,
) -> u128 {
    macro_rules! call {
        ($fun:ident, $($i:ident),*) => {{
            let [$($i),*] = *ints;
            let [p, q, r, s, t, u, v, w] = *floats;
            $fun($($i,)* p, q, r, s, t, u, v, w)
        }};
    }
    macro_rules! dispatch {
        ($($i:ident),*) => {
            if float_ret {
                let fun: FloatFn = std::mem::transmute(address);
                call!(fun, $($i),*).to_bits() as u128
            } else if pair_ret {
                let fun: PairFn = std::mem::transmute(address);
                call!(fun, $($i),*)
            } else {
                let fun: IntFn = std::mem::transmute(address);
                call!(fun, $($i),*) as u64 as u128
            }
        };
    }
    #[cfg(target_arch = "x86_64")]
    {
        dispatch!(a, b, c, d, e, f)
    }
    #[cfg(target_arch = "aarch64")]
    {
        dispatch!(a, b, c, d, e, f, g, h)
    }
    // INT_REGS is 0: no descriptor gets a register path
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = (address, float_ret, pair_ret, ints, floats);
        unreachable!()
    }
}
