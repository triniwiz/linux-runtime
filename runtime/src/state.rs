//! Per-isolate runtime state, stored in the isolate slot as `Rc<State>`.

use crate::ffi::{CFunction, ClosureData};
use nativescript_metadata::{Meta, MetaFile, MetaKind, Offset, RecordLayout as ClangLayout, Type};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::rc::Rc;

pub struct State {
    pub meta: std::sync::Arc<MetaFile>,
    pub isolate: Cell<*mut v8::Isolate>,
    pub context: RefCell<Option<v8::Global<v8::Context>>>,
    pub thread: std::thread::ThreadId,
    /// dlopen handles per module (keyed by module offset)
    libraries: RefCell<HashMap<Offset, Vec<usize>>>,
    pub functions: RefCell<HashMap<Offset, Rc<CFunction>>>,
    records: RefCell<HashMap<String, Rc<RecordLayout>>>,
    /// Types referenced from JS (interop.types.*, struct constructors, references)
    pub types: RefCell<Vec<Type>>,
    /// libffi closures handed to C, kept alive for the lifetime of the runtime
    pub closures: RefCell<Vec<Box<ClosureData>>>,
    pub pointer_prototype: RefCell<Option<v8::Global<v8::Object>>>,
    pub reference_prototype: RefCell<Option<v8::Global<v8::Object>>>,
    /// struct view classes, keyed by type index
    pub struct_templates: RefCell<HashMap<u32, v8::Global<v8::FunctionTemplate>>>,
    pub main_context: crate::event_loop::MainContext,
    pub modules: RefCell<crate::modules::ModuleMap>,
    pub next_request: Cell<u32>,
    pub requests: RefCell<HashMap<u32, v8::Global<v8::PromiseResolver>>>,
    pub websockets: RefCell<HashMap<u32, std::sync::mpsc::Sender<crate::net::WsCommand>>>,
    pub ws_dispatch: RefCell<Option<v8::Global<v8::Function>>>,
    pub workers: RefCell<HashMap<u32, std::sync::Arc<crate::workers::Link>>>,
    pub worker_dispatch: RefCell<Option<v8::Global<v8::Function>>>,
    /// set in worker runtimes
    pub parent: RefCell<Option<crate::workers::Parent>>,
    pub timers: RefCell<HashMap<u32, crate::event_loop::Timer>>,
    pub next_timer: Cell<u32>,
    pub loop_refs: Cell<i32>,
    pub microtasks_scheduled: Cell<bool>,
    pub rejections: RefCell<Vec<(v8::Global<v8::Promise>, v8::Global<v8::Value>)>>,
    /// uncaught errors reported from the event loop
    pub uncaught: Cell<u32>,
    /// Set while a resolved global is being defined on the global object:
    /// defining a property consults the interceptor again, which must not recurse.
    pub memoizing: Cell<bool>,
}

impl State {
    pub fn new(meta: std::sync::Arc<MetaFile>) -> Self {
        State {
            meta,
            isolate: Cell::new(std::ptr::null_mut()),
            context: RefCell::new(None),
            thread: std::thread::current().id(),
            libraries: Default::default(),
            functions: Default::default(),
            records: Default::default(),
            types: Default::default(),
            closures: Default::default(),
            pointer_prototype: Default::default(),
            reference_prototype: Default::default(),
            struct_templates: Default::default(),
            main_context: crate::event_loop::MainContext::for_current_thread(),
            modules: Default::default(),
            next_request: Cell::new(0),
            requests: Default::default(),
            websockets: Default::default(),
            ws_dispatch: Default::default(),
            workers: Default::default(),
            worker_dispatch: Default::default(),
            parent: Default::default(),
            timers: Default::default(),
            next_timer: Cell::new(0),
            loop_refs: Cell::new(0),
            microtasks_scheduled: Cell::new(false),
            rejections: Default::default(),
            uncaught: Cell::new(0),
            memoizing: Cell::new(false),
        }
    }

    /// Registers a type and returns its index (used to tag JS objects).
    pub fn register_type(&self, ty: Type) -> u32 {
        let mut types = self.types.borrow_mut();
        if let Some(i) = types.iter().position(|t| *t == ty) {
            return i as u32;
        }
        types.push(ty);
        (types.len() - 1) as u32
    }

    pub fn type_at(&self, index: u32) -> Option<Type> {
        self.types.borrow().get(index as usize).cloned()
    }

    /// Address of `meta`'s symbol: dlopen()s the module's link sonames, then the global namespace.
    pub fn symbol(&self, meta: &Meta) -> Result<*mut c_void, String> {
        let module = meta.module();
        let handles = {
            let mut libraries = self.libraries.borrow_mut();
            libraries
                .entry(module.offset())
                .or_insert_with(|| {
                    module
                        .libraries()
                        .filter_map(|lib| {
                            let name = CString::new(lib.name()).ok()?;
                            // SAFETY: plain dlopen of a soname from the metadata
                            let handle = unsafe {
                                libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL)
                            };
                            if handle.is_null() {
                                eprintln!(
                                    "[nativescript] failed to load {} for module {}: {}",
                                    lib.name(),
                                    module.name(),
                                    dlerror()
                                );
                                None
                            } else {
                                Some(handle as usize)
                            }
                        })
                        .collect()
                })
                .clone()
        };

        let name = CString::new(meta.name()).map_err(|_| "invalid symbol name".to_string())?;
        for handle in handles.iter().copied().chain(std::iter::once(libc::RTLD_DEFAULT as usize)) {
            // SAFETY: valid handle from dlopen (or RTLD_DEFAULT) and a NUL terminated name
            let sym = unsafe { libc::dlsym(handle as *mut c_void, name.as_ptr()) };
            if !sym.is_null() {
                return Ok(sym);
            }
        }
        Err(format!(
            "symbol '{}' not found in module {} ({})",
            meta.name(),
            module.name(),
            module.libraries().map(|l| l.name()).collect::<Vec<_>>().join(", ")
        ))
    }

    /// Layout of a struct / union type (named or anonymous).
    pub fn record(&self, ty: &Type) -> Result<Rc<RecordLayout>, String> {
        let (key, union) = match ty {
            Type::Struct(name) => (format!("s:{name}"), false),
            Type::Union(name) => (format!("u:{name}"), true),
            Type::AnonymousStruct(fields, layout) => {
                return Ok(Rc::new(self.compute_record(fields.clone(), false, layout.as_ref())?))
            }
            Type::AnonymousUnion(fields, layout) => {
                return Ok(Rc::new(self.compute_record(fields.clone(), true, layout.as_ref())?))
            }
            other => return Err(format!("{other:?} is not a record type")),
        };
        if let Some(layout) = self.records.borrow().get(&key) {
            return Ok(layout.clone());
        }
        let name = &key[2..];
        let meta = self
            .meta
            .find(name)
            .filter(|m| matches!(m.kind(), MetaKind::Struct | MetaKind::Union))
            .ok_or_else(|| format!("'{name}' is an opaque or unknown record, its layout is not available"))?;
        let fields = meta
            .fields()
            .unwrap_or_default()
            .into_iter()
            .map(|(n, t)| (n.into_owned(), t))
            .collect();
        let layout = Rc::new(self.compute_record(fields, union, meta.record_layout().as_ref())?);
        self.records.borrow_mut().insert(key, layout.clone());
        Ok(layout)
    }

    /// Field offsets come from clang's layout when the metadata has it
    /// (bitfields, packed / aligned records), natural C layout otherwise.
    fn compute_record(
        &self,
        fields: Vec<(String, Type)>,
        union: bool,
        clang: Option<&ClangLayout>,
    ) -> Result<RecordLayout, String> {
        let mut out = Vec::with_capacity(fields.len());
        let mut offset = 0usize;
        let mut size = 0usize;
        let mut align = 1usize;
        let mut natural = true;
        for (i, (name, ty)) in fields.into_iter().enumerate() {
            let l = self.layout(&ty)?;
            let natural_offset = if union { 0 } else { align_up(offset, l.align) };
            offset = natural_offset + l.size;
            size = size.max(offset);
            align = align.max(l.align);
            let mut field = Field { name, ty, offset: natural_offset, bits: None };
            if let Some(f) = clang.and_then(|c| c.fields.get(i)) {
                if f.bit_width > 0 {
                    field.bits = Some((f.offset_bits, f.bit_width));
                    field.offset = (f.offset_bits / 8) as usize;
                    natural = false;
                } else {
                    field.offset = (f.offset_bits / 8) as usize;
                    natural &= field.offset == natural_offset;
                }
            }
            out.push(field);
        }
        let (size, align) = match clang {
            Some(c) => {
                natural &= c.size as usize == align_up(size, align) && c.align as usize == align;
                (c.size as usize, c.align as usize)
            }
            None => (align_up(size, align), align),
        };
        Ok(RecordLayout { fields: out, size, align, union, natural })
    }

    /// C layout of a type on this (LP64 Linux) target.
    pub fn layout(&self, ty: &Type) -> Result<Layout, String> {
        let prim = |size: usize| Ok(Layout { size, align: size });
        match ty {
            Type::Void => Ok(Layout { size: 0, align: 1 }),
            Type::Bool | Type::Char | Type::UChar => prim(1),
            Type::Short | Type::UShort | Type::Unichar => prim(2),
            Type::Int | Type::UInt | Type::Float => prim(4),
            Type::Long | Type::ULong | Type::LongLong | Type::ULongLong | Type::Double => prim(8),
            Type::LongDouble => prim(16),
            t if t.is_pointer_like() => prim(std::mem::size_of::<usize>()),
            Type::ConstantArray(n, inner) => {
                let l = self.layout(inner)?;
                Ok(Layout { size: l.size * *n as usize, align: l.align })
            }
            Type::ExtVector(n, inner) => {
                let l = self.layout(inner)?;
                let size = (l.size * *n as usize).next_power_of_two();
                Ok(Layout { size, align: size.min(16) })
            }
            Type::Struct(_) | Type::Union(_) | Type::AnonymousStruct(..) | Type::AnonymousUnion(..) => {
                let r = self.record(ty)?;
                Ok(Layout { size: r.size, align: r.align })
            }
            other => Err(format!("unsupported type {other:?}")),
        }
    }

    /// libffi type for passing `ty` by value.
    pub fn ffi_type(&self, ty: &Type) -> Result<libffi::middle::Type, String> {
        use libffi::middle::Type as F;
        Ok(match ty {
            Type::Void => F::void(),
            Type::Bool | Type::UChar => F::u8(),
            Type::Char => F::i8(),
            Type::Short => F::i16(),
            Type::UShort | Type::Unichar => F::u16(),
            Type::Int => F::i32(),
            Type::UInt => F::u32(),
            Type::Long | Type::LongLong => F::i64(),
            Type::ULong | Type::ULongLong => F::u64(),
            Type::Float => F::f32(),
            Type::Double => F::f64(),
            Type::LongDouble => F::longdouble(),
            // C arrays decay to pointers when passed to functions
            Type::ConstantArray(..) => F::pointer(),
            t if t.is_pointer_like() => F::pointer(),
            Type::Struct(_) | Type::AnonymousStruct(..) | Type::Union(_) | Type::AnonymousUnion(..) => {
                self.ffi_record(ty)?
            }
            Type::ExtVector(n, inner) => {
                let elem = self.ffi_type(inner)?;
                F::structure((0..*n).map(|_| elem.clone()))
            }
            other => return Err(format!("unsupported type {other:?}")),
        })
    }

    fn ffi_record(&self, ty: &Type) -> Result<libffi::middle::Type, String> {
        use libffi::middle::Type as F;
        let record = self.record(ty)?;
        if record.size == 0 {
            return Err("empty records can't be passed by value".into());
        }
        if record.union || !record.natural {
            // libffi can't express these: whole-record member, else integer chunks
            for field in record.fields.iter().filter(|_| record.union) {
                let l = self.layout(&field.ty)?;
                if l.size == record.size && l.align == record.align {
                    return Ok(F::structure(self.ffi_field(&field.ty)?));
                }
            }
            let unit = match record.align {
                1 => F::u8(),
                2 => F::u16(),
                4 => F::u32(),
                _ => F::u64(),
            };
            let unit_size = record.align.min(8);
            return Ok(F::structure((0..record.size.div_ceil(unit_size)).map(|_| unit.clone())));
        }
        let mut elements = Vec::new();
        for field in &record.fields {
            elements.extend(self.ffi_field(&field.ty)?);
        }
        Ok(F::structure(elements))
    }

    /// Struct members: arrays are laid out inline as N elements.
    fn ffi_field(&self, ty: &Type) -> Result<Vec<libffi::middle::Type>, String> {
        match ty {
            Type::ConstantArray(n, inner) => {
                let elem = self.ffi_field(inner)?;
                Ok((0..*n).flat_map(|_| elem.clone()).collect())
            }
            other => Ok(vec![self.ffi_type(other)?]),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub size: usize,
    pub align: usize,
}

#[derive(Debug)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    /// byte offset (for bitfields: of the byte containing the first bit)
    pub offset: usize,
    /// (offset in bits from the record start, width) for bitfields
    pub bits: Option<(u32, u8)>,
}

#[derive(Debug)]
pub struct RecordLayout {
    pub fields: Vec<Field>,
    pub size: usize,
    pub align: usize,
    pub union: bool,
    /// false for bitfields / packed / over-aligned records
    pub natural: bool,
}

pub fn align_up(v: usize, align: usize) -> usize {
    if align <= 1 {
        v
    } else {
        v.div_ceil(align) * align
    }
}

pub fn dlerror() -> String {
    // SAFETY: dlerror returns a thread local static string or null
    unsafe {
        let e = libc::dlerror();
        if e.is_null() {
            "unknown error".into()
        } else {
            std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    }
}

/// Fetches the runtime state from the isolate.
pub fn state(scope: &v8::PinScope) -> Rc<State> {
    scope
        .get_slot::<Rc<State>>()
        .expect("nativescript runtime state not installed")
        .clone()
}
