//! Prepared C calls: libffi cif plus a register-only fast path.

use crate::native::Native;
use libffi::middle::Cif;
use libffi::raw;
use nativescript_metadata::Type;
use std::ffi::c_void;

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
}

/// Int and float args use separate register sequences (x86_64 SysV, AArch64): one
/// fn type taking all registers calls any signature that fits in them.
#[derive(Clone)]
pub struct FastCall {
    /// per argument: true = float register
    pub float_args: Vec<bool>,
    pub float_ret: bool,
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

impl CallDesc {
    pub fn new(native: &Native, name: &str, address: usize, signature: Vec<Type>, variadic: bool) -> Result<Self, String> {
        let signature: Vec<Type> = signature.iter().map(decay).collect();
        let cif = if variadic { None } else { Some(Self::prep(native, &signature, None)?) };
        let fast = if variadic { None } else { FastCall::new(&signature) };
        let slots = signature[1..].iter().map(|t| native.layout(t).map(|l| l.size.max(8))).collect::<Result<_, _>>()?;
        // small integer returns are widened to ffi_arg by libffi
        let ret_size = native.layout(&signature[0])?.size.max(16);
        Ok(CallDesc { name: name.to_owned(), address, signature, variadic, cif, slots, ret_size, fast })
    }

    /// cif for `signature`; `fixed` = number of fixed args of a variadic call.
    pub fn prep(native: &Native, signature: &[Type], fixed: Option<usize>) -> Result<Cif, String> {
        let ret = native.ffi_type(&signature[0])?;
        let args = signature[1..].iter().map(|t| native.ffi_type(t)).collect::<Result<Vec<_>, _>>()?;
        Ok(match fixed {
            Some(fixed) => Cif::new_variadic(args, fixed, ret),
            None => Cif::new(args, ret),
        })
    }

    pub fn cif(&self) -> Option<&Cif> {
        self.cif.as_ref()
    }

    /// libffi call; `argv` points at argument slots, `ret` at `ret_size` bytes.
    ///
    /// # Safety
    /// Slots must hold values of the signature's types.
    #[inline]
    pub unsafe fn call(&self, cif: &Cif, argv: &mut [*mut c_void], ret: *mut u8) {
        raw::ffi_call(
            cif.as_raw_ptr(),
            Some(std::mem::transmute::<usize, unsafe extern "C" fn()>(self.address)),
            ret as *mut c_void,
            argv.as_mut_ptr(),
        );
    }

    /// Register call (see [`FastCall`]); returns the raw return register.
    ///
    /// # Safety
    /// Only for descriptors with `fast` set, args placed per `float_args`.
    #[inline]
    pub unsafe fn call_fast(&self, float_ret: bool, ints: [i64; 8], floats: [f64; 8]) -> u128 {
        let [a, b, c, d, e, f, g, h] = ints;
        let [p, q, r, s, t, u, v, w] = floats;
        if float_ret {
            let fun: FloatFn = std::mem::transmute(self.address);
            fun(a, b, c, d, e, f, g, h, p, q, r, s, t, u, v, w).to_bits() as u128
        } else {
            let fun: IntFn = std::mem::transmute(self.address);
            fun(a, b, c, d, e, f, g, h, p, q, r, s, t, u, v, w) as u64 as u128
        }
    }
}
