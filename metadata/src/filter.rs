//! Metadata filtering (`native-api-usage.json`): keeps the declarations allowed
//! by whitelist / blacklist patterns plus the structs and unions they reference,
//! and writes them to a new, compact file in the generator's format.
//!
//! Patterns are `module:symbol` globs (`*`, `?`), one per line, `#` and `//`
//! comments; `module` is the top-level module (`Gtk`, `GLib`, ...), an empty
//! symbol matches the whole module.

use crate::types::TypeTag;
use crate::{flags, hash::string_hash, Meta, MetaFile, MetaKind, Offset, Type};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
struct Rule {
    module: String,
    symbol: String,
}

/// Whitelist / blacklist, as written by the CLI (`whitelist.mdg`, `blacklist.mdg`).
#[derive(Debug, Clone, Default)]
pub struct Filter {
    whitelist: Option<Vec<Rule>>,
    blacklist: Vec<Rule>,
}

fn parse_rules(text: &str) -> Vec<Rule> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("//"))
        .map(|l| {
            let (module, symbol) = l.split_once(':').unwrap_or((l, ""));
            // submodule paths (`Gtk.*`) match the top-level module
            let module = module.split('.').next().unwrap_or(module);
            Rule {
                module: module.to_owned(),
                symbol: symbol.to_owned(),
            }
        })
        .collect()
}

/// Glob match with `*` and `?`.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == b'*')
}

impl Rule {
    fn matches(&self, module: &str, symbol: &str) -> bool {
        (self.module.is_empty() || glob(self.module.as_bytes(), module.as_bytes()))
            && (self.symbol.is_empty() || glob(self.symbol.as_bytes(), symbol.as_bytes()))
    }
}

impl Filter {
    /// `whitelist` = None keeps everything that isn't blacklisted.
    pub fn new(whitelist: Option<&str>, blacklist: Option<&str>) -> Self {
        Filter {
            whitelist: whitelist.map(parse_rules),
            blacklist: blacklist.map(parse_rules).unwrap_or_default(),
        }
    }

    pub fn allows(&self, module: &str, symbol: &str) -> bool {
        let listed = self
            .whitelist
            .as_ref()
            .is_none_or(|w| w.iter().any(|r| r.matches(module, symbol)));
        listed && !self.blacklist.iter().any(|r| r.matches(module, symbol))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterStats {
    pub declarations: usize,
    pub kept: usize,
    /// structs / unions kept only because a kept declaration uses them
    pub dependencies: usize,
}

/// Filters `file`, returns the new file's bytes.
pub fn filter(file: &MetaFile, filter: &Filter) -> (Vec<u8>, FilterStats) {
    let all: Vec<Meta> = file.iter().collect();
    let mut keep: Vec<Meta> = Vec::new();
    let mut seen: HashSet<Offset> = HashSet::new();
    for meta in &all {
        if filter.allows(meta.module().name(), meta.name()) && seen.insert(meta.offset) {
            keep.push(*meta);
        }
    }
    let selected = keep.len();

    // records used by kept signatures, fields and variables
    let mut i = 0;
    while i < keep.len() {
        let meta = keep[i];
        i += 1;
        let mut names = Vec::new();
        let types = match meta.kind() {
            MetaKind::Function => meta.signature().unwrap_or_default(),
            MetaKind::Var => meta.var_type().into_iter().collect(),
            MetaKind::Struct | MetaKind::Union => meta
                .fields()
                .unwrap_or_default()
                .into_iter()
                .map(|(_, t)| t)
                .collect(),
            _ => Vec::new(),
        };
        for ty in &types {
            records(ty, &mut names);
        }
        for name in names {
            if let Some(dep) = file
                .find(&name)
                .filter(|m| matches!(m.kind(), MetaKind::Struct | MetaKind::Union))
            {
                if seen.insert(dep.offset) {
                    keep.push(dep);
                }
            }
        }
    }

    let mut writer = Writer::new(file);
    let offsets: Vec<Offset> = keep.iter().map(|m| writer.meta(m)).collect();
    let bytes = writer.finish(&keep, &offsets);
    let stats = FilterStats {
        declarations: all.len(),
        kept: keep.len(),
        dependencies: keep.len() - selected,
    };
    (bytes, stats)
}

/// Named records a type refers to.
fn records(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Struct(n) | Type::Union(n) => out.push(n.clone()),
        Type::Pointer(t)
        | Type::ConstantArray(_, t)
        | Type::IncompleteArray(t)
        | Type::ExtVector(_, t) => records(t, out),
        Type::FunctionPointer(sig) | Type::Block(sig) => sig.iter().for_each(|t| records(t, out)),
        Type::AnonymousStruct(fields, _) | Type::AnonymousUnion(fields, _) => {
            fields.iter().for_each(|(_, t)| records(t, out))
        }
        _ => {}
    }
}

/// Re-serializes declarations into a fresh heap (offset 0 is the null marker).
struct Writer<'a> {
    file: &'a MetaFile,
    heap: Vec<u8>,
    strings: HashMap<&'a str, Offset>,
    modules: HashMap<Offset, Offset>,
    empty_array: Offset,
}

impl<'a> Writer<'a> {
    fn new(file: &'a MetaFile) -> Self {
        Writer {
            file,
            heap: vec![0],
            strings: HashMap::new(),
            modules: HashMap::new(),
            empty_array: 0,
        }
    }

    fn pos(&self) -> Offset {
        self.heap.len() as Offset
    }

    fn i32(&mut self, v: i32) {
        self.heap.extend_from_slice(&v.to_le_bytes());
    }

    fn string(&mut self, s: &'a str) -> Offset {
        if let Some(o) = self.strings.get(s) {
            return *o;
        }
        let offset = self.pos();
        self.heap.extend_from_slice(s.as_bytes());
        self.heap.push(0);
        self.strings.insert(s, offset);
        offset
    }

    /// `i32 count, count x i32`
    fn array(&mut self, items: &[Offset]) -> Offset {
        if items.is_empty() && self.empty_array != 0 {
            return self.empty_array;
        }
        let offset = self.pos();
        self.i32(items.len() as i32);
        for item in items {
            self.i32(*item as i32);
        }
        if items.is_empty() {
            self.empty_array = offset;
        }
        offset
    }

    fn module(&mut self, old: Offset) -> Offset {
        if old == 0 {
            return 0;
        }
        if let Some(o) = self.modules.get(&old) {
            return *o;
        }
        let f = self.file;
        let name = self.string(f.heap_str(f.heap_ptr(old + 1)));
        let libraries: Vec<Offset> = f
            .heap_array(f.heap_ptr(old + 5))
            .iter()
            .collect::<Vec<_>>()
            .into_iter()
            .map(|lib| {
                let lib_name = self.string(f.heap_str(f.heap_ptr(lib + 1)));
                let offset = self.pos();
                self.heap.push(f.heap_u8(lib));
                self.i32(lib_name as i32);
                offset
            })
            .collect();
        let libraries = self.array(&libraries);
        let offset = self.pos();
        self.heap.push(f.heap_u8(old));
        self.i32(name as i32);
        self.i32(libraries as i32);
        self.modules.insert(old, offset);
        offset
    }

    /// Copies one encoding starting at `at` into `out`, returns the offset after it.
    fn encoding(&mut self, at: Offset, out: &mut Vec<u8>) -> Offset {
        let f = self.file;
        let raw = f.heap_u8(at);
        out.push(raw);
        let mut cursor = at + 1;
        let Some(tag) = TypeTag::from_u8(raw) else {
            return cursor;
        };
        match tag {
            TypeTag::StructDeclarationReference | TypeTag::UnionDeclarationReference => {
                self.pointer(&mut cursor, out, true)
            }
            TypeTag::Pointer | TypeTag::IncompleteArray => {
                return self.encoding(cursor, out);
            }
            TypeTag::ConstantArray | TypeTag::Vector => {
                self.pointer(&mut cursor, out, false);
                return self.encoding(cursor, out);
            }
            TypeTag::FunctionPointer | TypeTag::Block => {
                let count = f.heap_u8(cursor);
                out.push(count);
                cursor += 1;
                for _ in 0..count {
                    cursor = self.encoding(cursor, out);
                }
            }
            TypeTag::AnonymousStruct | TypeTag::AnonymousUnion => {
                let count = f.heap_u8(cursor);
                out.push(count);
                cursor += 1;
                for _ in 0..count {
                    self.pointer(&mut cursor, out, true);
                }
                for _ in 0..count {
                    cursor = self.encoding(cursor, out);
                }
                let layout = f.heap_ptr(cursor);
                cursor += 4;
                let layout = self.layout(layout);
                out.extend_from_slice(&(layout as i32).to_le_bytes());
            }
            // Objective-C references never appear in C metadata
            TypeTag::InterfaceDeclarationReference => {
                out.extend_from_slice(&[0; 8]);
                cursor += 8;
            }
            TypeTag::Id => {
                out.extend_from_slice(&[0; 4]);
                cursor += 4;
            }
            _ => {}
        }
        cursor
    }

    /// Copies the pointer at `cursor`; string pointers are re-interned.
    fn pointer(&mut self, cursor: &mut Offset, out: &mut Vec<u8>, string: bool) {
        let f = self.file;
        let v = f.heap_ptr(*cursor);
        *cursor += 4;
        let v = if string && v != 0 {
            self.string(f.heap_str(v))
        } else {
            v
        };
        out.extend_from_slice(&(v as i32).to_le_bytes());
    }

    fn encoding_list(&mut self, old: Offset) -> Offset {
        if old == 0 {
            return 0;
        }
        let count = self.file.heap_i32(old).max(0);
        let mut out = Vec::new();
        out.extend_from_slice(&count.to_le_bytes());
        let mut cursor = old + 4;
        for _ in 0..count {
            cursor = self.encoding(cursor, &mut out);
        }
        let offset = self.pos();
        self.heap.extend_from_slice(&out);
        offset
    }

    fn single_encoding(&mut self, old: Offset) -> Offset {
        if old == 0 {
            return 0;
        }
        let mut out = Vec::new();
        self.encoding(old, &mut out);
        let offset = self.pos();
        self.heap.extend_from_slice(&out);
        offset
    }

    /// `i32 size, i32 align, i32 count, count x (i32, u8)`, no pointers
    fn layout(&mut self, old: Offset) -> Offset {
        if old == 0 {
            return 0;
        }
        let count = self.file.heap_i32(old + 8).max(0) as usize;
        let len = 12 + count * 5;
        let offset = self.pos();
        let bytes = &self.file.heap_at(old)[..len];
        self.heap.extend_from_slice(bytes);
        offset
    }

    fn meta(&mut self, meta: &Meta<'a>) -> Offset {
        let f = self.file;
        let at = meta.offset;
        let has_names = meta.has_flag(flags::HAS_NAME) || meta.has_flag(flags::HAS_DEMANGLED_NAME);
        let names = if has_names {
            let count = 1
                + meta.has_flag(flags::HAS_NAME) as u32
                + meta.has_flag(flags::HAS_DEMANGLED_NAME) as u32;
            let old = f.heap_ptr(at);
            let strings: Vec<Offset> = (0..count)
                .map(|i| self.string(f.heap_str(f.heap_ptr(old + i * 4))))
                .collect();
            let offset = self.pos();
            for s in strings {
                self.i32(s as i32);
            }
            offset
        } else {
            self.string(f.heap_str(f.heap_ptr(at)))
        };
        let module = self.module(f.heap_ptr(at + 4));
        let body = at + 11;
        let fields: Vec<Offset> = match meta.kind() {
            MetaKind::JsCode => vec![self.string(f.heap_str(f.heap_ptr(body)))],
            MetaKind::Function => vec![self.encoding_list(f.heap_ptr(body))],
            MetaKind::Var => vec![self.single_encoding(f.heap_ptr(body))],
            MetaKind::Struct | MetaKind::Union => {
                let names: Vec<Offset> = f
                    .heap_array(f.heap_ptr(body))
                    .iter()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|n| self.string(f.heap_str(n)))
                    .collect();
                let names = self.array(&names);
                let encodings = self.encoding_list(f.heap_ptr(body + 4));
                let layout = self.layout(f.heap_ptr(body + 8));
                vec![names, encodings, layout]
            }
            _ => Vec::new(),
        };
        let offset = self.pos();
        self.i32(names as i32);
        self.i32(module as i32);
        self.heap
            .extend_from_slice(&f.heap_u16(at + 8).to_le_bytes());
        self.heap.push(f.heap_u8(at + 10));
        for field in fields {
            self.i32(field as i32);
        }
        offset
    }

    /// Hash tables in front of the heap. Buckets average two entries (the
    /// generator uses ten) to keep lookups short.
    fn finish(mut self, metas: &[Meta<'a>], offsets: &[Offset]) -> Vec<u8> {
        let buckets = |n: usize| (n / 2).max(16);
        let mut js: Vec<Vec<Offset>> = vec![Vec::new(); buckets(metas.len())];
        let mut native: Vec<Vec<Offset>> = vec![Vec::new(); buckets(metas.len())];
        for (meta, offset) in metas.iter().zip(offsets) {
            let index = string_hash(meta.js_name().as_bytes()) as usize % js.len();
            js[index].push(*offset);
            for name in std::iter::once(meta.name()).chain(meta.demangled_name()) {
                let index = string_hash(name.as_bytes()) as usize % native.len();
                native[index].push(*offset);
            }
        }
        let mut tables = Vec::new();
        for table in [&js, &Vec::new(), &native] {
            let table = if table.is_empty() {
                &vec![Vec::new()]
            } else {
                table
            };
            let heads: Vec<Offset> = table
                .iter()
                .map(|b| if b.is_empty() { 0 } else { self.array(b) })
                .collect();
            tables.push(heads);
        }
        // every module of the source, so module lookups stay stable
        let modules: Vec<Offset> = self
            .file
            .modules()
            .map(|m| m.offset())
            .collect::<Vec<_>>()
            .into_iter()
            .map(|m| self.module(m))
            .collect();
        tables.push(modules);

        let mut out = Vec::with_capacity(
            self.heap.len() + tables.iter().map(|t| 4 + t.len() * 4).sum::<usize>(),
        );
        for table in tables {
            out.extend_from_slice(&(table.len() as i32).to_le_bytes());
            for head in table {
                out.extend_from_slice(&(head as i32).to_le_bytes());
            }
        }
        out.extend_from_slice(&self.heap);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob(b"gtk_*", b"gtk_widget_show"));
        assert!(glob(b"*_new", b"gtk_button_new"));
        assert!(glob(b"g?k", b"gtk"));
        assert!(!glob(b"gtk_*", b"gdk_rgba"));
        assert!(glob(b"*", b""));
    }

    #[test]
    fn rules() {
        let f = Filter::new(
            Some("Gtk:gtk_button_*\n// comment\nGLib.*:*\n"),
            Some("Gtk:gtk_button_new_with_mnemonic"),
        );
        assert!(f.allows("Gtk", "gtk_button_new"));
        assert!(!f.allows("Gtk", "gtk_button_new_with_mnemonic"));
        assert!(f.allows("GLib", "g_free"));
        assert!(!f.allows("pango", "pango_font_description_new"));
        assert!(Filter::new(None, Some("libc")).allows("Gtk", "x"));
        assert!(!Filter::new(None, Some("libc")).allows("libc", "printf"));
    }
}
