//! libffi closures: C function pointers calling back into the host.

use crate::native::Native;
use libffi::middle::{Cif, Type as FfiType};
use libffi::raw;
use nativescript_metadata::Type;
use std::ffi::c_void;

pub type Trampoline = unsafe extern "C" fn(*mut raw::ffi_cif, *mut c_void, *mut *mut c_void, *mut c_void);

pub struct Closure {
    // libffi keeps a pointer to the cif: it must not move
    _cif: Box<Cif>,
    closure: *mut raw::ffi_closure,
    pub code: *mut c_void,
}

impl Closure {
    /// `userdata` must outlive the closure.
    pub fn new(native: &Native, signature: &[Type], trampoline: Trampoline, userdata: *mut c_void) -> Result<Self, String> {
        let ret = native.ffi_type(&signature[0])?;
        let args: Vec<FfiType> = signature[1..].iter().map(|t| native.ffi_type(t)).collect::<Result<_, _>>()?;
        let cif = Box::new(Cif::new(args, ret));
        let mut code: *mut c_void = std::ptr::null_mut();
        let closure = unsafe { raw::ffi_closure_alloc(std::mem::size_of::<raw::ffi_closure>(), &mut code) } as *mut raw::ffi_closure;
        if closure.is_null() {
            return Err("ffi_closure_alloc failed".into());
        }
        let status = unsafe { raw::ffi_prep_closure_loc(closure, cif.as_raw_ptr(), Some(trampoline), userdata, code) };
        if status != raw::ffi_status_FFI_OK {
            unsafe { raw::ffi_closure_free(closure as *mut c_void) };
            return Err(format!("ffi_prep_closure_loc failed ({status})"));
        }
        Ok(Closure { _cif: cif, closure, code })
    }
}

impl Drop for Closure {
    fn drop(&mut self) {
        unsafe { raw::ffi_closure_free(self.closure as *mut c_void) };
    }
}
