//! Metadata access, symbol loading, C layouts and libffi types.

use nativescript_metadata::{Meta, MetaId, MetaKind, MetaSet, RecordLayout as ClangLayout, Type};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::hash::{BuildHasherDefault, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

static LIBRARY_DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Directories searched for metadata sonames before the system paths
/// (the app's bundled `lib/`). Set once, before any symbol is resolved.
pub fn set_library_dirs(dirs: Vec<PathBuf>) {
    let _ = LIBRARY_DIRS.set(dirs);
}

/// What to `dlopen` for a module map `link` name, in order: the name as a
/// soname or file in the bundle's `lib/`, then for a `-l` style name (`mason_c`)
/// `libmason_c.so` and `libmason_c.so.<version>` in `lib/`, then `libmason_c.so`
/// from the system paths.
fn library_candidates(name: &str) -> Vec<PathBuf> {
    if name.contains('/') {
        return vec![PathBuf::from(name)];
    }
    let dirs = LIBRARY_DIRS.get().map(Vec::as_slice).unwrap_or_default();
    let bundled = |file: &str| dirs.iter().map(|d| d.join(file)).find(|p| p.exists());
    let mut out = vec![bundled(name).unwrap_or_else(|| PathBuf::from(name))];
    if !name.contains(".so") {
        let file = if name.starts_with("lib") {
            format!("{name}.so")
        } else {
            format!("lib{name}.so")
        };
        let versioned = || {
            let prefix = format!("{file}.");
            dirs.iter().find_map(|d| {
                let mut found: Vec<PathBuf> = std::fs::read_dir(d)
                    .ok()?
                    .filter_map(|e| Some(e.ok()?.path()))
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with(&prefix))
                    })
                    .collect();
                // the shortest is the soname (libfoo.so.1 over libfoo.so.1.2.3)
                found.sort_by_key(|p| p.as_os_str().len());
                found.into_iter().next()
            })
        };
        out.extend(bundled(&file).or_else(versioned));
        out.push(PathBuf::from(file));
    }
    out.dedup();
    out
}

/// `dlopen`s a module's `link` name, see [`library_candidates`].
fn open_library(name: &str) -> Result<*mut c_void, String> {
    let candidates = library_candidates(name);
    let mut first_error = None;
    for path in &candidates {
        let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) else {
            continue;
        };
        // SAFETY: plain dlopen of a library named by the metadata
        let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
        if !handle.is_null() {
            return Ok(handle);
        }
        first_error.get_or_insert_with(dlerror);
    }
    let tried: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
    let lib = LIBRARY_DIRS
        .get()
        .and_then(|d| d.first())
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "lib/ next to the executable".into());
    Err(format!(
        "{} (tried {}). A module map `link` is a soname (libfoo.so.1), a file in {lib} or a -l style name (foo)",
        first_error.unwrap_or_default(),
        tried.join(", ")
    ))
}

/// Multiply-rotate hash (rustc's FxHasher): `Type` keys hash struct names, which
/// SipHash makes a hot spot.
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.add(u64::from_le_bytes(*c));
        }
        for &b in rest {
            self.add(b as u64);
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;

/// Engine independent runtime state (one per JS thread).
pub struct Native {
    pub meta: Arc<MetaSet>,
    /// dlopen handles per module
    libraries: RefCell<HashMap<MetaId, Vec<usize>>>,
    records: RefCell<HashMap<Type, Rc<RecordLayout>, FxBuildHasher>>,
    /// Types referenced from JS (interop.types.*, struct constructors, references)
    pub types: RefCell<Vec<Type>>,
    type_index: RefCell<HashMap<Type, u32, FxBuildHasher>>,
    /// record layouts by type index (struct views)
    records_at: RefCell<Vec<Option<Rc<RecordLayout>>>>,
}

impl Native {
    pub fn new(meta: Arc<MetaSet>) -> Self {
        Native {
            meta,
            libraries: Default::default(),
            records: Default::default(),
            types: Default::default(),
            type_index: Default::default(),
            records_at: Default::default(),
        }
    }

    /// Record layout of the registered type `index`, without hashing the type.
    #[inline]
    pub fn record_at(&self, index: u32) -> Result<Rc<RecordLayout>, String> {
        if let Some(Some(r)) = self.records_at.borrow().get(index as usize) {
            return Ok(r.clone());
        }
        let record = {
            let types = self.types.borrow();
            let ty = types.get(index as usize).ok_or("unknown type")?;
            self.record(ty)?
        };
        let mut at = self.records_at.borrow_mut();
        if at.len() <= index as usize {
            at.resize(index as usize + 1, None);
        }
        at[index as usize] = Some(record.clone());
        Ok(record)
    }

    pub fn register_type(&self, ty: &Type) -> u32 {
        if let Some(i) = self.type_index.borrow().get(ty) {
            return *i;
        }
        let mut types = self.types.borrow_mut();
        types.push(ty.clone());
        let i = (types.len() - 1) as u32;
        self.type_index.borrow_mut().insert(ty.clone(), i);
        i
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
                .entry(self.meta.id_in(module.file(), module.offset()))
                .or_insert_with(|| {
                    module
                        .libraries()
                        .filter_map(|lib| match open_library(lib.name()) {
                            Ok(handle) => Some(handle as usize),
                            Err(e) => {
                                eprintln!(
                                    "[nativescript] failed to load {} for module {}: {e}",
                                    lib.name(),
                                    module.name(),
                                );
                                None
                            }
                        })
                        .collect()
                })
                .clone()
        };

        let name = CString::new(meta.name()).map_err(|_| "invalid symbol name".to_string())?;
        for handle in handles
            .iter()
            .copied()
            .chain(std::iter::once(libc::RTLD_DEFAULT as usize))
        {
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
            module
                .libraries()
                .map(|l| l.name())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Layout of a struct / union type (named or anonymous).
    pub fn record(&self, ty: &Type) -> Result<Rc<RecordLayout>, String> {
        if let Some(layout) = self.records.borrow().get(ty) {
            return Ok(layout.clone());
        }
        let (name, union) = match ty {
            Type::Struct(name) => (name, false),
            Type::Union(name) => (name, true),
            Type::AnonymousStruct(fields, layout) => {
                return Ok(Rc::new(self.compute_record(
                    fields.clone(),
                    false,
                    layout.as_ref(),
                )?))
            }
            Type::AnonymousUnion(fields, layout) => {
                return Ok(Rc::new(self.compute_record(
                    fields.clone(),
                    true,
                    layout.as_ref(),
                )?))
            }
            other => return Err(format!("{other:?} is not a record type")),
        };
        let meta = self
            .meta
            .find_global(name)
            .filter(|m| matches!(m.kind(), MetaKind::Struct | MetaKind::Union))
            .ok_or_else(|| {
                format!("'{name}' is an opaque or unknown record, its layout is not available")
            })?;
        let fields = meta
            .fields()
            .unwrap_or_default()
            .into_iter()
            .map(|(n, t)| (n.into_owned(), t))
            .collect();
        let layout = Rc::new(self.compute_record(fields, union, meta.record_layout().as_ref())?);
        self.records.borrow_mut().insert(ty.clone(), layout.clone());
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
            let mut field = Field {
                name,
                ty,
                offset: natural_offset,
                bits: None,
            };
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
        Ok(RecordLayout {
            fields: out,
            size,
            align,
            union,
            natural,
        })
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
                Ok(Layout {
                    size: l.size * *n as usize,
                    align: l.align,
                })
            }
            Type::ExtVector(n, inner) => {
                let l = self.layout(inner)?;
                let size = (l.size * *n as usize).next_power_of_two();
                Ok(Layout {
                    size,
                    align: size.min(16),
                })
            }
            Type::Struct(_)
            | Type::Union(_)
            | Type::AnonymousStruct(..)
            | Type::AnonymousUnion(..) => {
                let r = self.record(ty)?;
                Ok(Layout {
                    size: r.size,
                    align: r.align,
                })
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
            Type::Struct(_)
            | Type::AnonymousStruct(..)
            | Type::Union(_)
            | Type::AnonymousUnion(..) => self.ffi_record(ty)?,
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
            return Ok(F::structure(
                (0..record.size.div_ceil(unit_size)).map(|_| unit.clone()),
            ));
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

#[cfg(test)]
mod tests {
    use super::library_candidates;

    fn names(link: &str) -> Vec<String> {
        library_candidates(link)
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    #[test]
    fn link_names() {
        // sonames and paths are used as they are
        assert_eq!(names("libgtk-4.so.1"), ["libgtk-4.so.1"]);
        assert_eq!(names("libmason_c.so"), ["libmason_c.so"]);
        assert_eq!(names("/opt/x/libx.so"), ["/opt/x/libx.so"]);
        // -l style names
        assert_eq!(names("mason_c"), ["mason_c", "libmason_c.so"]);
        assert_eq!(names("libfoo"), ["libfoo", "libfoo.so"]);
    }
}
