//! In-place reads of V8 values for the hottest checks (Smis, numbers, oddballs,
//! interop Pointers, Externals, callback arguments), using the 64-bit layout without
//! pointer compression. [`Layout::probe`] checks it against the V8 API when an isolate
//! starts; when it doesn't match, callers use the API instead.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

const MAP_INSTANCE_TYPE: usize = 12;
const HEAP_NUMBER_VALUE: usize = 8;
const EXTERNAL_VALUE: usize = 24;
const EMBEDDER_FIELDS: usize = 32;
const ODDBALL_TYPE: u16 = 0x83;
const FIRST_NONSTRING_TYPE: u16 = 0x80;
const STRING_REPRESENTATION: u16 = 0x7;
/// representation and encoding of a sequential one-byte string
const SEQ_ONE_BYTE: u16 = 0x8;
const STRING_LENGTH: usize = 12;
const SEQ_STRING_CHARS: usize = 16;
const CONS_STRING: u16 = 0x1;
const THIN_STRING: u16 = 0x5;

/// `FunctionCallbackInfo` slots (v8-function-callback.h).
const INFO_ARGC: usize = 0;
const INFO_ISOLATE: usize = 5;
const INFO_RETURN_VALUE: usize = 6;
const INFO_RECEIVER: usize = 9;
const INFO_FIRST_ARG: usize = 10;

pub struct Layout {
    undefined: usize,
    null: usize,
    true_value: usize,
    false_value: usize,
    heap_number_map: usize,
    /// handle slot of the interop.Pointer prototype instance: maps can move
    pointer_proto: *const usize,
    /// `FunctionCallbackInfo` matches the slots above
    callback_info: bool,
    /// sequential one-byte strings are read in place
    seq_strings: bool,
}

/// Tagged value in a handle slot.
#[inline(always)]
pub fn word(v: v8::Local<v8::Value>) -> usize {
    unsafe { *(&*v as *const v8::Value as *const usize) }
}

#[inline(always)]
fn is_smi(w: usize) -> bool {
    w & 1 == 0
}

#[inline(always)]
fn smi(w: usize) -> i32 {
    ((w as i64) >> 32) as i32
}

#[inline(always)]
fn smi_word(v: i32) -> usize {
    ((v as i64) << 32) as usize
}

#[inline(always)]
unsafe fn field<T: Copy>(w: usize, offset: usize) -> T {
    unsafe { ((w - 1 + offset) as *const T).read() }
}

#[inline(always)]
unsafe fn map(w: usize) -> usize {
    unsafe { field(w, 0) }
}

#[inline(always)]
unsafe fn instance_type(w: usize) -> u16 {
    unsafe { field(map(w), MAP_INSTANCE_TYPE) }
}

/// What a JS value is, as far as argument conversion cares.
#[derive(Clone, Copy)]
pub enum Value {
    /// Smi
    Int(i32),
    Number(f64),
    Bool(bool),
    Null,
    Undefined,
    /// interop.Pointer address
    Pointer(usize),
    /// flat string: written without creating handles
    FlatString,
    /// anything else, including strings that need flattening
    Other,
}

impl Layout {
    #[inline(always)]
    pub fn classify(&self, v: v8::Local<v8::Value>) -> Value {
        let w = word(v);
        if is_smi(w) {
            return Value::Int(smi(w));
        }
        if w == self.undefined {
            return Value::Undefined;
        }
        if w == self.null {
            return Value::Null;
        }
        if w == self.true_value {
            return Value::Bool(true);
        }
        if w == self.false_value {
            return Value::Bool(false);
        }
        unsafe {
            let m = map(w);
            if m == self.heap_number_map {
                return Value::Number(f64::from_bits(field(w, HEAP_NUMBER_VALUE)));
            }
            if m == map(*self.pointer_proto) {
                return Value::Pointer(field::<usize>(w, EMBEDDER_FIELDS + 8) >> 1);
            }
            let t: u16 = field(m, MAP_INSTANCE_TYPE);
            if t < FIRST_NONSTRING_TYPE {
                let r = t & STRING_REPRESENTATION;
                if r != CONS_STRING && r != THIN_STRING {
                    return Value::FlatString;
                }
            }
        }
        Value::Other
    }

    /// Smi value.
    #[inline(always)]
    pub fn int(v: v8::Local<v8::Value>) -> Option<i32> {
        let w = word(v);
        is_smi(w).then(|| smi(w))
    }

    /// Address of an interop object whose map is that of the instance in `proto_slot`.
    #[inline(always)]
    pub fn instance_address(
        &self,
        v: v8::Local<v8::Value>,
        proto_slot: *const usize,
    ) -> Option<usize> {
        let w = word(v);
        if is_smi(w) || proto_slot.is_null() {
            return None;
        }
        unsafe { (map(w) == map(*proto_slot)).then(|| field::<usize>(w, EMBEDDER_FIELDS + 8) >> 1) }
    }

    /// Address passed for null, undefined or an interop.Pointer.
    #[inline(always)]
    pub fn pointer(&self, v: v8::Local<v8::Value>) -> Option<usize> {
        let w = word(v);
        if w == self.null || w == self.undefined {
            return Some(0);
        }
        unsafe {
            (!is_smi(w) && map(w) == map(*self.pointer_proto))
                .then(|| field::<usize>(w, EMBEDDER_FIELDS + 8) >> 1)
        }
    }

    /// Latin-1 characters of a sequential one-byte string, valid until V8 allocates.
    #[inline(always)]
    pub fn latin1<'a>(&self, v: v8::Local<'a, v8::Value>) -> Option<&'a [u8]> {
        let w = word(v);
        if !self.seq_strings || is_smi(w) {
            return None;
        }
        unsafe {
            let t = instance_type(w);
            (t < FIRST_NONSTRING_TYPE && t & 0xf == SEQ_ONE_BYTE).then(|| {
                let len = field::<u32>(w, STRING_LENGTH) as usize;
                std::slice::from_raw_parts((w - 1 + SEQ_STRING_CHARS) as *const u8, len)
            })
        }
    }

    #[inline(always)]
    pub fn callback_info(&self) -> bool {
        self.callback_info
    }

    #[inline(always)]
    pub fn null_word(&self) -> usize {
        self.null
    }

    #[inline(always)]
    pub fn bool_word(&self, b: bool) -> usize {
        if b {
            self.true_value
        } else {
            self.false_value
        }
    }

    /// Checks the layout against the V8 API. `pointer` makes an interop.Pointer.
    pub fn probe<'s>(
        scope: &mut v8::PinScope<'s, '_>,
        pointer_proto: *const usize,
        pointer: impl Fn(&mut v8::PinScope<'s, '_>, usize) -> Option<v8::Local<'s, v8::Object>>,
    ) -> Option<Layout> {
        if cfg!(not(target_pointer_width = "64")) || pointer_proto.is_null() {
            return None;
        }
        for n in [0, 1, -1, 42, i32::MIN, i32::MAX] {
            let w = word(v8::Integer::new(scope, n).into());
            if !is_smi(w) || smi(w) != n || smi_word(n) != w {
                return None;
            }
        }
        let undefined = word(v8::undefined(scope).into());
        let null = word(v8::null(scope).into());
        let true_value = word(v8::Boolean::new(scope, true).into());
        let false_value = word(v8::Boolean::new(scope, false).into());
        let oddballs = [undefined, null, true_value, false_value];
        for (i, w) in oddballs.iter().enumerate() {
            if is_smi(*w)
                || unsafe { instance_type(*w) } != ODDBALL_TYPE
                || oddballs[i + 1..].contains(w)
            {
                return None;
            }
        }
        let heap_number_map = unsafe { map(word(v8::Number::new(scope, 1.5).into())) };
        for n in [1.5, -0.0, 1e300, 4503599627370497.0] {
            let w = word(v8::Number::new(scope, n).into());
            if is_smi(w)
                || unsafe { map(w) } != heap_number_map
                || unsafe { field::<u64>(w, HEAP_NUMBER_VALUE) } != n.to_bits()
            {
                return None;
            }
        }
        let flat = v8::String::new(scope, "nativescript").unwrap();
        let cons = v8::Script::compile(
            scope,
            v8::String::new(scope, "(s => s + s)('abcdefghijklmnop'.slice(1))").unwrap(),
            None,
        )
        .and_then(|s| s.run(scope))?;
        let mut layout = Layout {
            undefined,
            null,
            true_value,
            false_value,
            heap_number_map,
            pointer_proto,
            callback_info: false,
            seq_strings: true,
        };
        layout.seq_strings = probe_strings(scope, &layout);
        if !matches!(
            layout.classify(v8::Integer::new(scope, -5).into()),
            Value::Int(-5)
        ) || !matches!(layout.classify(flat.into()), Value::FlatString)
            || !cons.is_string()
            || !matches!(layout.classify(cons), Value::Other)
        {
            return None;
        }
        let object = v8::Object::new(scope);
        if !matches!(layout.classify(object.into()), Value::Other) {
            return None;
        }
        for address in [0x7f12_3456_7890usize, 0x10] {
            let p = pointer(scope, address)?;
            match layout.classify(p.into()) {
                Value::Pointer(a) if a == address => {}
                _ => return None,
            }
            unsafe { set_object_field(&p, 1, address + 2) };
            let read = unsafe { p.get_aligned_pointer_from_internal_field(1, 0) } as usize >> 1;
            if read != address + 2 {
                return None;
            }
        }
        let external = v8::External::new(scope, 0x7f00_1234_5678usize as *mut c_void);
        if unsafe { field::<usize>(word(external.into()), EXTERNAL_VALUE) } != 0x7f00_1234_5678 {
            return None;
        }
        RAW_EXTERNALS.store(true, Ordering::Relaxed);
        let callback_info = probe_callback_info(scope);
        Some(Layout {
            callback_info,
            ..layout
        })
    }
}

/// Externals don't depend on the isolate: set by the first successful probe.
static RAW_EXTERNALS: AtomicBool = AtomicBool::new(false);

/// Value of a `v8::External`.
#[inline(always)]
pub fn external_value(v: v8::Local<v8::Value>) -> *mut c_void {
    if RAW_EXTERNALS.load(Ordering::Relaxed) {
        unsafe { field(word(v), EXTERNAL_VALUE) }
    } else {
        let ext: v8::Local<v8::External> = unsafe { v8::Local::cast_unchecked(v) };
        ext.value()
    }
}

/// Writes interop object field `index` (`handles` encoding) of a fresh object;
/// only after a successful [`Layout::probe`].
#[inline(always)]
pub unsafe fn set_object_field(o: &v8::Object, index: usize, value: usize) {
    let w = unsafe { *(o as *const v8::Object as *const usize) };
    unsafe { ((w - 1 + EMBEDDER_FIELDS + 8 * index) as *mut usize).write(value << 1) };
}

/// Raw `FunctionCallbackInfo` access, valid when [`Layout::callback_info`].
pub mod info {
    use super::*;

    #[inline(always)]
    pub unsafe fn argc(info: *const v8::FunctionCallbackInfo) -> usize {
        unsafe { *(info as *const usize).add(INFO_ARGC) }
    }

    /// Isolate pointer for raw V8 calls.
    #[inline(always)]
    pub unsafe fn raw_isolate(info: *const v8::FunctionCallbackInfo) -> v8::UnsafeRawIsolatePtr {
        unsafe { std::mem::transmute(*(info as *const usize).add(INFO_ISOLATE)) }
    }

    /// Sets the return value to the object in a handle slot.
    #[inline(always)]
    pub unsafe fn set_return_handle(
        info: *const v8::FunctionCallbackInfo,
        slot: *const v8::Object,
    ) {
        unsafe { set_return_word(info, *(slot as *const usize)) };
    }

    /// The isolate, borrowed from its slot.
    #[inline(always)]
    pub unsafe fn isolate(info: *const v8::FunctionCallbackInfo) -> *const v8::Isolate {
        unsafe { (info as *const usize).add(INFO_ISOLATE) as *const v8::Isolate }
    }

    #[inline(always)]
    pub unsafe fn arg<'s>(
        info: *const v8::FunctionCallbackInfo,
        i: usize,
    ) -> v8::Local<'s, v8::Value> {
        unsafe { std::mem::transmute((info as *const usize).add(INFO_FIRST_ARG + i)) }
    }

    /// Sets the return value to a Smi, oddball or other immortal word.
    #[inline(always)]
    pub unsafe fn set_return_word(info: *const v8::FunctionCallbackInfo, w: usize) {
        unsafe { *(info as *mut usize).add(INFO_RETURN_VALUE) = w };
    }

    /// Sets the return value to an int32 (a Smi).
    #[inline(always)]
    pub unsafe fn set_return_int32(info: *const v8::FunctionCallbackInfo, v: i32) {
        unsafe { set_return_word(info, smi_word(v)) };
    }
}

thread_local! {
    static PROBED: Cell<bool> = const { Cell::new(false) };
    static PROBED_ISOLATE: Cell<usize> = const { Cell::new(0) };
}

unsafe extern "C" fn probe_callback(info: *const v8::FunctionCallbackInfo) {
    let args = v8::FunctionCallbackArguments::from_function_callback_info(unsafe { &*info });
    let slot = |i: usize| unsafe { (info as *const usize).add(i) } as *const v8::Value;
    let ok = unsafe { info::argc(info) } == args.length() as usize
        && args.length() == 2
        && slot(INFO_RECEIVER) == &*args.this() as *const v8::Object as *const v8::Value
        && slot(INFO_FIRST_ARG) == &*args.get(0) as *const v8::Value
        && slot(INFO_FIRST_ARG + 1) == &*args.get(1) as *const v8::Value
        && word(args.get(0)) == smi_word(7);
    let mut rv = v8::ReturnValue::from_function_callback_info(unsafe { &*info });
    rv.set_int32(41);
    let set = unsafe { *(info as *const usize).add(INFO_RETURN_VALUE) } == smi_word(41);
    if ok && set {
        unsafe { info::set_return_int32(info, 42) };
    }
    PROBED_ISOLATE.with(|p| p.set(unsafe { *(info as *const usize).add(INFO_ISOLATE) }));
    PROBED.with(|p| p.set(ok && set));
}

fn probe_strings(scope: &mut v8::PinScope, layout: &Layout) -> bool {
    for text in ["nativescript", "", "caf\u{e9} cr\u{e8}me"] {
        let latin1: Vec<u8> = text.chars().map(|c| c as u8).collect();
        for kind in [v8::NewStringType::Normal, v8::NewStringType::Internalized] {
            let Some(s) = v8::String::new_from_utf8(scope, text.as_bytes(), kind) else {
                return false;
            };
            if layout.latin1(s.into()) != Some(&latin1[..]) {
                return false;
            }
        }
    }
    let two_byte = v8::String::new(scope, "\u{20ac}").unwrap();
    layout.latin1(two_byte.into()).is_none()
}

fn probe_callback_info(scope: &mut v8::PinScope) -> bool {
    PROBED.with(|p| p.set(false));
    let Some(f) = v8::Function::builder_raw(probe_callback).build(scope) else {
        return false;
    };
    let recv = v8::Object::new(scope);
    let args = [
        v8::Integer::new(scope, 7).into(),
        v8::String::new(scope, "x").unwrap().into(),
    ];
    let result = f.call(scope, recv.into(), &args);
    let isolate: usize = unsafe { std::mem::transmute(scope.as_raw_isolate_ptr()) };
    PROBED.with(|p| p.get())
        && PROBED_ISOLATE.with(|p| p.get()) == isolate
        && result.is_some_and(|r| word(r) == smi_word(42))
}
