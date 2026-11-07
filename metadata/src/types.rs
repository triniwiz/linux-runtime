//! Type encodings (`binary::BinaryTypeEncodingType` in the generator).

use crate::{MetaFile, Offset};

/// Tag byte of a type encoding. Values must match
/// `metadata-generator/src/Binary/binaryStructures.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TypeTag {
    Void = 0,
    Bool,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    LongLong,
    ULongLong,
    Char,
    UChar,
    Unichar,
    CharS,
    CString,
    Float,
    Double,
    InterfaceDeclarationReference,
    StructDeclarationReference,
    UnionDeclarationReference,
    Pointer,
    VaList,
    Selector,
    Class,
    ProtocolType,
    InstanceType,
    Id,
    ConstantArray,
    IncompleteArray,
    FunctionPointer,
    Block,
    AnonymousStruct,
    AnonymousUnion,
    Vector,
    LongDouble,
    ConstCString,
}

impl TypeTag {
    pub fn from_u8(v: u8) -> Option<Self> {
        if v <= TypeTag::ConstCString as u8 {
            // SAFETY: repr(u8) enum with contiguous discriminants 0..=ConstCString
            Some(unsafe { std::mem::transmute::<u8, TypeTag>(v) })
        } else {
            None
        }
    }
}

/// Decoded type encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Void,
    Bool,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    LongLong,
    ULongLong,
    /// signed char
    Char,
    UChar,
    Unichar,
    /// `char*`
    CString,
    /// `const char*`, read into JS strings
    ConstCString,
    Float,
    Double,
    LongDouble,
    /// Named struct, by JS name (look it up with [`MetaFile::find`]).
    Struct(String),
    /// Named union, by JS name.
    Union(String),
    Pointer(Box<Type>),
    ConstantArray(u32, Box<Type>),
    IncompleteArray(Box<Type>),
    /// `__attribute__((ext_vector_type(n)))`
    ExtVector(u32, Box<Type>),
    /// Return type first.
    FunctionPointer(Vec<Type>),
    /// Return type first (Apple blocks).
    Block(Vec<Type>),
    AnonymousStruct(Vec<(String, Type)>, Option<RecordLayout>),
    AnonymousUnion(Vec<(String, Type)>, Option<RecordLayout>),
    VaList,
    /// Objective-C only encodings, never produced for C headers.
    ObjC(TypeTag),
    /// Unknown tag, the metadata is newer than this reader.
    Unknown(u8),
}

impl Type {
    pub fn is_void(&self) -> bool {
        matches!(self, Type::Void)
    }

    /// Pointer-sized at the ABI level (pointers, C strings, function pointers...).
    pub fn is_pointer_like(&self) -> bool {
        matches!(
            self,
            Type::Pointer(_)
                | Type::CString
                | Type::ConstCString
                | Type::IncompleteArray(_)
                | Type::FunctionPointer(_)
                | Type::Block(_)
                | Type::ObjC(_)
        )
    }
}

/// Layout of a struct / union as computed by clang (accounts for bitfields,
/// packed and aligned attributes).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordLayout {
    pub size: u32,
    pub align: u32,
    pub fields: Vec<FieldLayout>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldLayout {
    /// offset from the start of the record, in bits
    pub offset_bits: u32,
    /// width for bitfields, 0 otherwise
    pub bit_width: u8,
}

/// `i32 size, i32 align, i32 count, count x (i32 offsetBits, u8 bitWidth)`
pub(crate) fn decode_layout(file: &MetaFile, offset: Offset) -> Option<RecordLayout> {
    if offset == 0 {
        return None;
    }
    let size = file.heap_i32(offset);
    let align = file.heap_i32(offset + 4);
    let count = file.heap_i32(offset + 8).max(0) as u32;
    let fields = (0..count)
        .map(|i| {
            let at = offset + 12 + i * 5;
            FieldLayout {
                offset_bits: file.heap_i32(at).max(0) as u32,
                bit_width: file.heap_u8(at + 4),
            }
        })
        .collect();
    Some(RecordLayout {
        size: size.max(0) as u32,
        align: align.max(1) as u32,
        fields,
    })
}

/// Decodes the `i32 count` + `count` back to back encodings list.
pub(crate) fn decode_list(file: &MetaFile, offset: Offset) -> Vec<Type> {
    if offset == 0 {
        return Vec::new();
    }
    let count = file.heap_i32(offset).max(0) as usize;
    let mut cursor = offset + 4;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let (ty, next) = decode(file, cursor);
        out.push(ty);
        cursor = next;
    }
    out
}

/// Decodes the encoding at `offset`, returns the type and the offset right after it.
pub(crate) fn decode(file: &MetaFile, offset: Offset) -> (Type, Offset) {
    let raw = file.heap_u8(offset);
    let mut cursor = offset + 1;
    let Some(tag) = TypeTag::from_u8(raw) else {
        return (Type::Unknown(raw), cursor);
    };

    macro_rules! ptr {
        () => {{
            let v = file.heap_ptr(cursor);
            cursor += 4;
            v
        }};
    }
    macro_rules! inner {
        () => {{
            let (t, next) = decode(file, cursor);
            cursor = next;
            Box::new(t)
        }};
    }

    let ty = match tag {
        TypeTag::Void => Type::Void,
        TypeTag::Bool => Type::Bool,
        TypeTag::Short => Type::Short,
        TypeTag::UShort => Type::UShort,
        TypeTag::Int => Type::Int,
        TypeTag::UInt => Type::UInt,
        TypeTag::Long => Type::Long,
        TypeTag::ULong => Type::ULong,
        TypeTag::LongLong => Type::LongLong,
        TypeTag::ULongLong => Type::ULongLong,
        TypeTag::Char | TypeTag::CharS => Type::Char,
        TypeTag::UChar => Type::UChar,
        TypeTag::Unichar => Type::Unichar,
        TypeTag::CString => Type::CString,
        TypeTag::ConstCString => Type::ConstCString,
        TypeTag::Float => Type::Float,
        TypeTag::Double => Type::Double,
        TypeTag::LongDouble => Type::LongDouble,
        TypeTag::VaList => Type::VaList,
        TypeTag::StructDeclarationReference => Type::Struct(file.heap_str(ptr!()).to_owned()),
        TypeTag::UnionDeclarationReference => Type::Union(file.heap_str(ptr!()).to_owned()),
        TypeTag::Pointer => Type::Pointer(inner!()),
        TypeTag::IncompleteArray => Type::IncompleteArray(inner!()),
        TypeTag::ConstantArray => {
            let size = ptr!();
            Type::ConstantArray(size, inner!())
        }
        TypeTag::Vector => {
            let size = ptr!();
            Type::ExtVector(size, inner!())
        }
        TypeTag::FunctionPointer | TypeTag::Block => {
            let count = file.heap_u8(cursor);
            cursor += 1;
            let mut sig = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let (t, next) = decode(file, cursor);
                sig.push(t);
                cursor = next;
            }
            if tag == TypeTag::Block {
                Type::Block(sig)
            } else {
                Type::FunctionPointer(sig)
            }
        }
        TypeTag::AnonymousStruct | TypeTag::AnonymousUnion => {
            let count = file.heap_u8(cursor) as u32;
            cursor += 1;
            let names: Vec<String> = (0..count)
                .map(|i| file.heap_str(file.heap_ptr(cursor + i * 4)).to_owned())
                .collect();
            cursor += count * 4;
            let mut fields = Vec::with_capacity(count as usize);
            for name in names {
                let (t, next) = decode(file, cursor);
                fields.push((name, t));
                cursor = next;
            }
            let layout = decode_layout(file, ptr!());
            if tag == TypeTag::AnonymousUnion {
                Type::AnonymousUnion(fields, layout)
            } else {
                Type::AnonymousStruct(fields, layout)
            }
        }
        TypeTag::InterfaceDeclarationReference => {
            cursor += 8; // name, protocols
            Type::ObjC(tag)
        }
        TypeTag::Id => {
            cursor += 4; // protocols
            Type::ObjC(tag)
        }
        TypeTag::Selector | TypeTag::Class | TypeTag::ProtocolType | TypeTag::InstanceType => {
            Type::ObjC(tag)
        }
    };
    (ty, cursor)
}
