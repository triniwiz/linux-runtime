//! Zero-copy reader for the generator's binary metadata (writer: `metadata-generator/src/Binary/`).
//!
//! ```text
//! js names / native protocols / native names tables: i32 buckets, buckets x i32 heap offset
//! modules: i32 count, count x i32 heap offset; then the heap (offset 0 = null)
//! ```

mod hash;
mod types;

pub use hash::string_hash;
pub use types::{FieldLayout, RecordLayout, Type, TypeTag};

use std::borrow::Cow;
use std::fmt;
use std::path::Path;

/// Offsets inside the heap.
pub type Offset = u32;

/// Low 3 bits of `Meta::flags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaKind {
    Undefined,
    Struct,
    Union,
    Function,
    JsCode,
    Var,
    Interface,
    Protocol,
}

impl MetaKind {
    fn from_flags(flags: u16) -> Self {
        match flags & 0x7 {
            1 => MetaKind::Struct,
            2 => MetaKind::Union,
            3 => MetaKind::Function,
            4 => MetaKind::JsCode,
            5 => MetaKind::Var,
            6 => MetaKind::Interface,
            7 => MetaKind::Protocol,
            _ => MetaKind::Undefined,
        }
    }
}

pub mod flags {
    pub const HAS_DEMANGLED_NAME: u16 = 1 << 8;
    pub const HAS_NAME: u16 = 1 << 7;
    pub const FUNCTION_IS_VARIADIC: u16 = 1 << 5;
    pub const FUNCTION_OWNS_RETURNED_COCOA_OBJECT: u16 = 1 << 4;
    pub const FUNCTION_RETURNS_UNMANAGED: u16 = 1 << 3;
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Malformed(what) => write!(f, "malformed metadata: {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

enum Storage {
    Mmap(memmap2::Mmap),
    Owned(Vec<u8>),
    Static(&'static [u8]),
}

impl Storage {
    fn bytes(&self) -> &[u8] {
        match self {
            Storage::Mmap(m) => m,
            Storage::Owned(v) => v,
            Storage::Static(s) => s,
        }
    }
}

#[derive(Clone, Copy)]
struct Table {
    /// absolute file offset of the first bucket
    start: usize,
    len: u32,
}

pub struct MetaFile {
    storage: Storage,
    js_table: Table,
    native_protocols: Table,
    native_names: Table,
    modules: Table,
    heap: usize,
}

impl fmt::Debug for MetaFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetaFile")
            .field("size", &self.storage.bytes().len())
            .field("js_buckets", &self.js_table.len)
            .field("modules", &self.modules.len)
            .finish()
    }
}

impl MetaFile {
    /// Memory maps the file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let file = std::fs::File::open(path)?;
        // SAFETY: the metadata file is treated as read only for the lifetime of the process.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Self::new(Storage::Mmap(map))
    }

    pub fn from_vec(bytes: Vec<u8>) -> Result<Self, Error> {
        Self::new(Storage::Owned(bytes))
    }

    /// Metadata embedded in the binary (e.g. with `include_bytes!`).
    pub fn from_static(bytes: &'static [u8]) -> Result<Self, Error> {
        Self::new(Storage::Static(bytes))
    }

    fn new(storage: Storage) -> Result<Self, Error> {
        let bytes = storage.bytes();
        let mut pos = 0usize;
        let table = |pos: &mut usize| -> Result<Table, Error> {
            let len = read_i32(bytes, *pos).ok_or(Error::Malformed("truncated table"))?;
            if len < 0 {
                return Err(Error::Malformed("negative table size"));
            }
            let start = *pos + 4;
            let end = start + len as usize * 4;
            if end > bytes.len() {
                return Err(Error::Malformed("table out of bounds"));
            }
            *pos = end;
            Ok(Table { start, len: len as u32 })
        };
        let js_table = table(&mut pos)?;
        let native_protocols = table(&mut pos)?;
        let native_names = table(&mut pos)?;
        let modules = table(&mut pos)?;
        if js_table.len == 0 {
            return Err(Error::Malformed("empty js table"));
        }
        if pos >= bytes.len() {
            return Err(Error::Malformed("missing heap"));
        }
        Ok(MetaFile {
            js_table,
            native_protocols,
            native_names,
            modules,
            heap: pos,
            storage,
        })
    }

    #[inline]
    pub fn bytes(&self) -> &[u8] {
        self.storage.bytes()
    }

    /// Heap slice starting at `offset`.
    #[inline]
    pub(crate) fn heap_at(&self, offset: Offset) -> &[u8] {
        let start = self.heap + offset as usize;
        self.bytes().get(start..).unwrap_or(&[])
    }

    #[inline]
    pub(crate) fn heap_i32(&self, offset: Offset) -> i32 {
        read_i32(self.bytes(), self.heap + offset as usize).unwrap_or(0)
    }

    #[inline]
    pub(crate) fn heap_u16(&self, offset: Offset) -> u16 {
        let p = self.heap + offset as usize;
        self.bytes()
            .get(p..p + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .unwrap_or(0)
    }

    #[inline]
    pub(crate) fn heap_u8(&self, offset: Offset) -> u8 {
        self.bytes().get(self.heap + offset as usize).copied().unwrap_or(0)
    }

    #[inline]
    pub(crate) fn heap_ptr(&self, offset: Offset) -> Offset {
        self.heap_i32(offset) as u32
    }

    /// NUL terminated string stored at `offset` in the heap.
    pub(crate) fn heap_str(&self, offset: Offset) -> &str {
        if offset == 0 {
            return "";
        }
        let bytes = self.heap_at(offset);
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        // The generator writes ASCII, fall back to an empty string for anything else
        std::str::from_utf8(&bytes[..end]).unwrap_or("")
    }

    /// `i32 count, count x i32` array of offsets in the heap.
    pub(crate) fn heap_array(&self, offset: Offset) -> OffsetArray<'_> {
        if offset == 0 {
            return OffsetArray { file: self, start: 0, len: 0 };
        }
        let len = self.heap_i32(offset).max(0) as u32;
        OffsetArray { file: self, start: offset + 4, len }
    }

    fn bucket(&self, table: Table, name: &str) -> OffsetArray<'_> {
        if table.len == 0 {
            return OffsetArray { file: self, start: 0, len: 0 };
        }
        let index = string_hash(name.as_bytes()) % table.len;
        let bucket = read_i32(self.bytes(), table.start + index as usize * 4).unwrap_or(0);
        self.heap_array(bucket as u32)
    }

    /// Looks up a declaration by the name exposed to JavaScript.
    pub fn find(&self, js_name: &str) -> Option<Meta<'_>> {
        self.bucket(self.js_table, js_name)
            .iter()
            .map(|o| Meta { file: self, offset: o })
            .find(|m| m.js_name() == js_name)
    }

    /// Looks up a declaration by its native (C) name.
    pub fn find_native(&self, name: &str) -> Option<Meta<'_>> {
        self.bucket(self.native_names, name)
            .iter()
            .map(|o| Meta { file: self, offset: o })
            .find(|m| m.name() == name || m.demangled_name() == Some(name))
    }

    /// Declaration at `offset`, as returned by [`Meta::offset`]. The offset
    /// must come from this file.
    pub fn meta_at(&self, offset: Offset) -> Meta<'_> {
        Meta { file: self, offset }
    }

    /// All declarations, in hash table order.
    pub fn iter(&self) -> impl Iterator<Item = Meta<'_>> + '_ {
        (0..self.js_table.len).flat_map(move |i| {
            let bucket = read_i32(self.bytes(), self.js_table.start + i as usize * 4).unwrap_or(0);
            self.heap_array(bucket as u32)
                .iter()
                .map(move |o| Meta { file: self, offset: o })
        })
    }

    pub fn modules(&self) -> impl Iterator<Item = Module<'_>> + '_ {
        (0..self.modules.len).map(move |i| {
            let offset = read_i32(self.bytes(), self.modules.start + i as usize * 4).unwrap_or(0);
            Module { file: self, offset: offset as u32 }
        })
    }

    /// Objective-C protocol table, kept for completeness, empty for C metadata.
    pub fn protocol_bucket_count(&self) -> u32 {
        self.native_protocols.len
    }
}

fn read_i32(bytes: &[u8], at: usize) -> Option<i32> {
    let b = bytes.get(at..at + 4)?;
    Some(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[derive(Clone, Copy)]
pub(crate) struct OffsetArray<'a> {
    file: &'a MetaFile,
    start: Offset,
    len: u32,
}

impl<'a> OffsetArray<'a> {
    pub(crate) fn get(&self, i: u32) -> Option<Offset> {
        (i < self.len).then(|| self.file.heap_ptr(self.start + i * 4))
    }

    pub(crate) fn iter(self) -> impl Iterator<Item = Offset> + 'a {
        (0..self.len).map(move |i| self.file.heap_ptr(self.start + i * 4))
    }
}

/// A top level module (clang module), e.g. `libc` or `GLib`.
#[derive(Clone, Copy)]
pub struct Module<'a> {
    file: &'a MetaFile,
    offset: Offset,
}

impl<'a> Module<'a> {
    pub fn offset(&self) -> Offset {
        self.offset
    }

    fn flags(&self) -> u8 {
        self.file.heap_u8(self.offset)
    }

    pub fn is_framework(&self) -> bool {
        self.flags() & 1 != 0
    }

    pub fn is_system(&self) -> bool {
        self.flags() & 2 != 0
    }

    pub fn name(&self) -> &'a str {
        self.file.heap_str(self.file.heap_ptr(self.offset + 1))
    }

    /// Libraries from the module map `link` directives. On Linux these are
    /// sonames that can be passed to `dlopen` directly.
    pub fn libraries(&self) -> impl Iterator<Item = Library<'a>> + 'a {
        let file = self.file;
        file.heap_array(file.heap_ptr(self.offset + 5))
            .iter()
            .map(move |offset| Library { file, offset })
    }
}

impl fmt::Debug for Module<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Module")
            .field("name", &self.name())
            .field("system", &self.is_system())
            .field("libraries", &self.libraries().map(|l| l.name()).collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Clone, Copy)]
pub struct Library<'a> {
    file: &'a MetaFile,
    offset: Offset,
}

impl<'a> Library<'a> {
    pub fn is_framework(&self) -> bool {
        self.file.heap_u8(self.offset) & 1 != 0
    }

    pub fn name(&self) -> &'a str {
        self.file.heap_str(self.file.heap_ptr(self.offset + 1))
    }
}

// Meta header: names i32, topLevelModule i32, flags u16, introduced u8
const META_HEADER: Offset = 4 + 4 + 2 + 1;

/// A declaration in the metadata.
#[derive(Clone, Copy)]
pub struct Meta<'a> {
    file: &'a MetaFile,
    offset: Offset,
}

impl PartialEq for Meta<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.file, other.file) && self.offset == other.offset
    }
}

impl Eq for Meta<'_> {}

impl<'a> Meta<'a> {
    /// Unique (per file) identifier, usable as a cache key.
    pub fn offset(&self) -> Offset {
        self.offset
    }

    pub fn file(&self) -> &'a MetaFile {
        self.file
    }

    pub fn flags(&self) -> u16 {
        self.file.heap_u16(self.offset + 8)
    }

    pub fn has_flag(&self, flag: u16) -> bool {
        self.flags() & flag != 0
    }

    pub fn kind(&self) -> MetaKind {
        MetaKind::from_flags(self.flags())
    }

    /// `(major << 3) | minor` of the availability attribute, 0 = always available.
    pub fn introduced(&self) -> u8 {
        self.file.heap_u8(self.offset + 10)
    }

    fn names_offset(&self) -> Offset {
        self.file.heap_ptr(self.offset)
    }

    fn has_names_array(&self) -> bool {
        self.has_flag(flags::HAS_NAME) || self.has_flag(flags::HAS_DEMANGLED_NAME)
    }

    /// Name exposed to JavaScript (may differ from [`Meta::name`] when it
    /// collides with another declaration, e.g. `stat` the struct vs the function).
    pub fn js_name(&self) -> &'a str {
        if !self.has_names_array() {
            return self.file.heap_str(self.names_offset());
        }
        // [jsName if HasName][name][demangled if HasDemangledName]
        self.file.heap_str(self.file.heap_ptr(self.names_offset()))
    }

    /// Native (C) name, the symbol to `dlsym` for functions and variables.
    pub fn name(&self) -> &'a str {
        if !self.has_names_array() {
            return self.file.heap_str(self.names_offset());
        }
        let index = if self.has_flag(flags::HAS_NAME) { 1 } else { 0 };
        self.file.heap_str(self.file.heap_ptr(self.names_offset() + index * 4))
    }

    pub fn demangled_name(&self) -> Option<&'a str> {
        if !self.has_flag(flags::HAS_DEMANGLED_NAME) {
            return None;
        }
        let index = if self.has_flag(flags::HAS_NAME) { 2 } else { 1 };
        Some(self.file.heap_str(self.file.heap_ptr(self.names_offset() + index * 4)))
    }

    pub fn module(&self) -> Module<'a> {
        Module {
            file: self.file,
            offset: self.file.heap_ptr(self.offset + 4),
        }
    }

    pub fn is_variadic(&self) -> bool {
        self.kind() == MetaKind::Function && self.has_flag(flags::FUNCTION_IS_VARIADIC)
    }

    /// Function signature, return type first.
    pub fn signature(&self) -> Option<Vec<Type>> {
        if self.kind() != MetaKind::Function {
            return None;
        }
        let list = self.file.heap_ptr(self.offset + META_HEADER);
        Some(types::decode_list(self.file, list))
    }

    /// Type of a global variable.
    pub fn var_type(&self) -> Option<Type> {
        if self.kind() != MetaKind::Var {
            return None;
        }
        let encoding = self.file.heap_ptr(self.offset + META_HEADER);
        Some(types::decode(self.file, encoding).0)
    }

    /// JavaScript source for constants and enums (evaluates to the value).
    pub fn js_code(&self) -> Option<&'a str> {
        if self.kind() != MetaKind::JsCode {
            return None;
        }
        Some(self.file.heap_str(self.file.heap_ptr(self.offset + META_HEADER)))
    }

    /// Layout of a struct or union computed by clang.
    pub fn record_layout(&self) -> Option<RecordLayout> {
        if !matches!(self.kind(), MetaKind::Struct | MetaKind::Union) {
            return None;
        }
        types::decode_layout(self.file, self.file.heap_ptr(self.offset + META_HEADER + 8))
    }

    /// Fields of a struct or union, in declaration order.
    pub fn fields(&self) -> Option<Vec<(Cow<'a, str>, Type)>> {
        if !matches!(self.kind(), MetaKind::Struct | MetaKind::Union) {
            return None;
        }
        let names = self.file.heap_array(self.file.heap_ptr(self.offset + META_HEADER));
        let encodings = self.file.heap_ptr(self.offset + META_HEADER + 4);
        let types = types::decode_list(self.file, encodings);
        Some(
            types
                .into_iter()
                .enumerate()
                .map(|(i, ty)| {
                    let name = names.get(i as u32).map(|o| self.file.heap_str(o)).unwrap_or("");
                    (Cow::Borrowed(name), ty)
                })
                .collect(),
        )
    }
}

impl fmt::Debug for Meta<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Meta")
            .field("kind", &self.kind())
            .field("js_name", &self.js_name())
            .field("name", &self.name())
            .field("module", &self.module().name())
            .finish()
    }
}
