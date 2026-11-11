//! Distinct entry points for C functions, so CPU profiles name them correctly.
//!
//! V8's profiler names a native frame by the address of its API callback (and of
//! its fast call target). Every C function shares `ffi::c_function_callback` and a
//! fast call trampoline per signature, so all native time showed up under one
//! function's name. Each C function gets its own thunk instead: a few
//! instructions that jump to the shared code through a table slot, without
//! touching argument registers or the stack. When the pool runs out, functions
//! fall back to the shared address.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

/// Thunks in the pool (each C function takes up to two: API callback and fast call).
const THUNKS: usize = 8192;
/// Bytes per thunk, see the assembly below.
const THUNK_SIZE: usize = 16;

#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".pushsection .bss.ns_thunk_targets,\"aw\",@nobits",
    ".balign 8",
    ".globl ns_thunk_targets",
    ".hidden ns_thunk_targets",
    "ns_thunk_targets:",
    ".zero {size} * 8",
    ".popsection",
    ".pushsection .text.ns_thunks,\"ax\",@progbits",
    ".balign 16",
    ".globl ns_thunks",
    ".hidden ns_thunks",
    "ns_thunks:",
    ".set ns_thunk_index, 0",
    ".rept {size}",
    // landing pad for indirect calls (a NOP without BTI); x16 is the
    // intra-procedure-call scratch register
    "bti c",
    "adrp x16, ns_thunk_targets + ns_thunk_index * 8",
    "ldr x16, [x16, :lo12:ns_thunk_targets + ns_thunk_index * 8]",
    "br x16",
    ".set ns_thunk_index, ns_thunk_index + 1",
    ".endr",
    ".popsection",
    size = const THUNKS,
);

#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    ".pushsection .bss.ns_thunk_targets,\"aw\",@nobits",
    ".balign 8",
    ".globl ns_thunk_targets",
    ".hidden ns_thunk_targets",
    "ns_thunk_targets:",
    ".zero {size} * 8",
    ".popsection",
    ".pushsection .text.ns_thunks,\"ax\",@progbits",
    ".balign 16",
    ".globl ns_thunks",
    ".hidden ns_thunks",
    "ns_thunks:",
    ".set ns_thunk_index, 0",
    ".rept {size}",
    "endbr64",
    "jmp qword ptr [rip + ns_thunk_targets + ns_thunk_index * 8]",
    // 4 + 6 bytes, padded to 16
    ".balign 16, 0xcc",
    ".set ns_thunk_index, ns_thunk_index + 1",
    ".endr",
    ".popsection",
    size = const THUNKS,
);

unsafe extern "C" {
    static ns_thunks: u8;
    static ns_thunk_targets: [AtomicUsize; THUNKS];
}

struct Pool {
    next: usize,
    /// (target, C function address) -> thunk index, shared by every runtime thread
    assigned: HashMap<(usize, usize), usize>,
}

static POOL: LazyLock<Mutex<Pool>> = LazyLock::new(|| {
    Mutex::new(Pool {
        next: 0,
        assigned: HashMap::new(),
    })
});

/// An entry point that jumps to `target`, unique to the C function at `function`
/// (`target` itself once the pool is used up).
pub fn entry(target: *const c_void, function: usize) -> *const c_void {
    let Ok(mut pool) = POOL.lock() else {
        return target;
    };
    let key = (target as usize, function);
    let index = match pool.assigned.get(&key) {
        Some(&i) => i,
        None if pool.next < THUNKS => {
            let i = pool.next;
            pool.next += 1;
            // SAFETY: the table has THUNKS slots; the slot is written before its
            // thunk's address is handed out (Release here, the jump reads it after
            // V8 got the address from this thread or a later one).
            unsafe { ns_thunk_targets[i].store(target as usize, Ordering::Release) };
            pool.assigned.insert(key, i);
            i
        }
        None => return target,
    };
    // SAFETY: inside the pool, THUNK_SIZE bytes per thunk
    unsafe { (&raw const ns_thunks).add(index * THUNK_SIZE) as *const c_void }
}

#[cfg(test)]
mod tests {
    use super::entry;
    use std::ffi::c_void;

    extern "C" fn add(a: i64, b: i64, c: f64, d: f64) -> f64 {
        (a + b) as f64 + c * d
    }
    extern "C" fn sub(a: i64, b: i64, c: f64, d: f64) -> f64 {
        (a - b) as f64 - c * d
    }

    #[test]
    fn thunks_forward_calls() {
        type F = extern "C" fn(i64, i64, f64, f64) -> f64;
        let a1 = entry(add as *const c_void, 1);
        let a2 = entry(add as *const c_void, 2);
        let s1 = entry(sub as *const c_void, 1);
        // one per (target, function), reused after that
        assert_ne!(a1, a2);
        assert_ne!(a1, s1);
        assert_eq!(a1, entry(add as *const c_void, 1));
        let (a1, a2, s1): (F, F, F) = unsafe {
            (std::mem::transmute(a1), std::mem::transmute(a2), std::mem::transmute(s1))
        };
        assert_eq!(a1(2, 3, 1.5, 2.0), 8.0);
        assert_eq!(a2(2, 3, 1.5, 2.0), 8.0);
        assert_eq!(s1(2, 3, 1.5, 2.0), -4.0);
    }
}
