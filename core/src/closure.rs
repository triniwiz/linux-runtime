//! C function pointers calling back into the host: libffi closures, and on x86_64
//! register closures for signatures that fit in argument registers.

use crate::native::Native;
use libffi::middle::{Cif, Type as FfiType};
use libffi::raw;
use nativescript_metadata::Type;
use std::ffi::c_void;

pub type Trampoline =
    unsafe extern "C" fn(*mut raw::ffi_cif, *mut c_void, *mut *mut c_void, *mut c_void);

pub struct Closure {
    // libffi keeps a pointer to the cif: it must not move
    _cif: Box<Cif>,
    closure: *mut raw::ffi_closure,
    pub code: *mut c_void,
}

impl Closure {
    /// `userdata` must outlive the closure.
    pub fn new(
        native: &Native,
        signature: &[Type],
        trampoline: Trampoline,
        userdata: *mut c_void,
    ) -> Result<Self, String> {
        let ret = native.ffi_type(&signature[0])?;
        let args: Vec<FfiType> = signature[1..]
            .iter()
            .map(|t| native.ffi_type(t))
            .collect::<Result<_, _>>()?;
        let cif = Box::new(Cif::new(args, ret));
        let mut code: *mut c_void = std::ptr::null_mut();
        let closure =
            unsafe { raw::ffi_closure_alloc(std::mem::size_of::<raw::ffi_closure>(), &mut code) }
                as *mut raw::ffi_closure;
        if closure.is_null() {
            return Err("ffi_closure_alloc failed".into());
        }
        let status = unsafe {
            raw::ffi_prep_closure_loc(closure, cif.as_raw_ptr(), Some(trampoline), userdata, code)
        };
        if status != raw::ffi_status_FFI_OK {
            unsafe { raw::ffi_closure_free(closure as *mut c_void) };
            return Err(format!("ffi_prep_closure_loc failed ({status})"));
        }
        Ok(Closure {
            _cif: cif,
            closure,
            code,
        })
    }
}

impl Drop for Closure {
    fn drop(&mut self) {
        unsafe { raw::ffi_closure_free(self.closure as *mut c_void) };
    }
}

/// Argument and return registers of a register closure call.
#[repr(C)]
pub struct Registers {
    pub ints: [u64; 6],
    pub floats: [f64; 8],
    /// rax, rdx
    pub ret: [u64; 2],
    /// xmm0, xmm1
    pub fret: [f64; 2],
}

/// Called with the closure's userdata, its argument registers and the caller's
/// stack arguments; writes the result into `Registers::ret` / `fret`.
pub type RegisterHandler = unsafe extern "C" fn(*mut c_void, *mut Registers, *const u64);

#[cfg(target_arch = "x86_64")]
pub use registers::RegisterClosure;

#[cfg(target_arch = "x86_64")]
mod registers {
    use super::{RegisterHandler, Registers};
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::sync::Mutex;

    const PAGE: usize = 4096;
    const STUB: usize = 32;
    const STUBS: usize = PAGE / STUB;
    /// released stubs wait this long before reuse, so a call through a stale pointer
    /// aborts instead of reaching another callback
    const QUARANTINE: usize = 256;

    /// What a stub's data slot points at.
    struct Callee {
        handler: RegisterHandler,
        userdata: *mut c_void,
    }

    struct Released(Callee);
    // immutable
    unsafe impl Sync for Released {}

    static RELEASED: Released = Released(Callee {
        handler: released,
        userdata: std::ptr::null_mut(),
    });

    /// Code pages of stubs, each followed by a data page: stub `i` loads data slot `i`
    /// (a `Callee` pointer) into r10 and jumps to `entry`. Code pages are never
    /// writable once made executable; data pages hold no code pointers.
    struct Pool {
        fresh: Vec<usize>,
        released: VecDeque<usize>,
    }

    static POOL: Mutex<Pool> = Mutex::new(Pool {
        fresh: Vec::new(),
        released: VecDeque::new(),
    });

    fn slot(stub: usize) -> *mut usize {
        (stub + PAGE) as *mut usize
    }

    /// Maps a code page plus its data page; `false` when executable memory is denied.
    fn map_page(pool: &mut Pool) -> bool {
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                2 * PAGE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return false;
        }
        let code = base as *mut u8;
        let load = (PAGE as i32 - 11).to_le_bytes();
        let target = (entry as *const () as u64).to_le_bytes();
        #[rustfmt::skip]
        let stub: [u8; STUB] = [
            0xf3, 0x0f, 0x1e, 0xfa, // endbr64
            0x4c, 0x8b, 0x15, load[0], load[1], load[2], load[3], // mov r10, [rip + PAGE - 11]
            0x49, 0xbb, target[0], target[1], target[2], target[3], // movabs r11, entry
            target[4], target[5], target[6], target[7],
            0x41, 0xff, 0xe3, // jmp r11
            0x0f, 0x0b, 0x0f, 0x0b, 0x0f, 0x0b, 0x0f, 0x0b, // ud2
        ];
        for i in 0..STUBS {
            unsafe {
                std::ptr::copy_nonoverlapping(stub.as_ptr(), code.add(i * STUB), STUB);
                *slot(code as usize + i * STUB) = &RELEASED.0 as *const Callee as usize;
            }
        }
        if unsafe { libc::mprotect(base, PAGE, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
            unsafe { libc::munmap(base, 2 * PAGE) };
            return false;
        }
        pool.fresh
            .extend((0..STUBS).rev().map(|i| code as usize + i * STUB));
        true
    }

    /// C function pointer whose calls reach `handler(userdata, registers, stack)`.
    pub struct RegisterClosure {
        pub code: *mut c_void,
        // freed after `drop` points the slot away from it
        _callee: Box<Callee>,
    }

    impl RegisterClosure {
        /// `userdata` must outlive the closure. `None` without executable memory.
        pub fn new(handler: RegisterHandler, userdata: *mut c_void) -> Option<Self> {
            let callee = Box::new(Callee { handler, userdata });
            let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
            let stub = if pool.released.len() > QUARANTINE {
                pool.released.pop_front()?
            } else {
                if pool.fresh.is_empty() && !map_page(&mut pool) {
                    return None;
                }
                pool.fresh.pop()?
            };
            unsafe { *slot(stub) = &*callee as *const Callee as usize };
            Some(RegisterClosure {
                code: stub as *mut c_void,
                _callee: callee,
            })
        }
    }

    impl Drop for RegisterClosure {
        fn drop(&mut self) {
            let stub = self.code as usize;
            let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
            unsafe { *slot(stub) = &RELEASED.0 as *const Callee as usize };
            pool.released.push_back(stub);
        }
    }

    unsafe extern "C" fn dispatch(callee: *const Callee, regs: *mut Registers, stack: *const u64) {
        let callee = unsafe { &*callee };
        unsafe { (callee.handler)(callee.userdata, regs, stack) };
    }

    unsafe extern "C" fn released(_: *mut c_void, _: *mut Registers, _: *const u64) {
        eprintln!("[nativescript] a released callback was called");
        std::process::abort();
    }

    /// Saves the argument registers, calls `dispatch(r10, &registers, stack arguments)`
    /// and returns `Registers::ret` / `fret`.
    #[unsafe(naked)]
    unsafe extern "C" fn entry() {
        core::arch::naked_asm!(
            "endbr64",
            "push rbp",
            "mov rbp, rsp",
            "sub rsp, 160",
            "mov [rsp], rdi",
            "mov [rsp + 8], rsi",
            "mov [rsp + 16], rdx",
            "mov [rsp + 24], rcx",
            "mov [rsp + 32], r8",
            "mov [rsp + 40], r9",
            "movsd [rsp + 48], xmm0",
            "movsd [rsp + 56], xmm1",
            "movsd [rsp + 64], xmm2",
            "movsd [rsp + 72], xmm3",
            "movsd [rsp + 80], xmm4",
            "movsd [rsp + 88], xmm5",
            "movsd [rsp + 96], xmm6",
            "movsd [rsp + 104], xmm7",
            "mov rdi, r10",
            "mov rsi, rsp",
            "lea rdx, [rbp + 16]",
            "call {dispatch}",
            "mov rax, [rsp + 112]",
            "mov rdx, [rsp + 120]",
            "movsd xmm0, [rsp + 128]",
            "movsd xmm1, [rsp + 136]",
            "leave",
            "ret",
            dispatch = sym dispatch,
        );
    }

    const _: () = assert!(std::mem::size_of::<Registers>() == 144);

    #[cfg(test)]
    mod tests {
        use super::*;

        unsafe extern "C" fn add(data: *mut c_void, regs: *mut Registers, _: *const u64) {
            let regs = unsafe { &mut *regs };
            let base = data as u64;
            regs.ret[0] = base + regs.ints[0] + regs.ints[5];
            regs.ret[1] = regs.ints[1];
            regs.fret[0] = regs.floats[0] + regs.floats[7];
        }

        #[test]
        fn registers_reach_the_handler_and_back() {
            let closure = RegisterClosure::new(add, 100 as *mut c_void).unwrap();
            type Ints = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> u128;
            type Floats = unsafe extern "C" fn(f64, f64, f64, f64, f64, f64, f64, f64) -> f64;
            let ints: Ints = unsafe { std::mem::transmute(closure.code) };
            let ret = unsafe { ints(1, 2, 3, 4, 5, 6) };
            assert_eq!(ret as u64, 107);
            assert_eq!((ret >> 64) as u64, 2);
            let floats: Floats = unsafe { std::mem::transmute(closure.code) };
            assert_eq!(
                unsafe { floats(1.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.25) },
                3.75
            );
        }

        #[test]
        fn released_stubs_are_quarantined() {
            let first = RegisterClosure::new(add, std::ptr::null_mut()).unwrap();
            let code = first.code;
            drop(first);
            let again: Vec<_> = (0..QUARANTINE)
                .map(|_| RegisterClosure::new(add, std::ptr::null_mut()).unwrap())
                .collect();
            assert!(again.iter().all(|c| c.code != code));
        }
    }
}
