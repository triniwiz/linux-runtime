//! Live struct / union views: field accessors read and write the C memory;
//! an owner (ArrayBuffer or parent view) keeps it alive.

use crate::handles::{self, Kind};
use crate::interop::keys;
use crate::marshal::{self, CallCx, Mem};
use crate::state::{state, State};
use crate::throw_error;
use nativescript_metadata::Type;

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

/// Address of the memory behind a struct view.
#[inline]
pub fn address<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
) -> Option<usize> {
    handles::is(v, Kind::Struct).map(handles::address)
}

fn this_view<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    this: v8::Local<'s, v8::Object>,
) -> Option<(usize, Type)> {
    let o = handles::is(this.into(), Kind::Struct)?;
    Some((handles::address(o), st.type_at(handles::type_index(o)?)?))
}

fn field_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let this = args.this();
    let Some((addr, ty)) = this_view(scope, &st, this) else {
        return throw_error(scope, "not a struct");
    };
    let index = args.data().uint32_value(scope).unwrap_or(0) as usize;
    let result = st.record(&ty).and_then(|record| {
        let field = record.fields.get(index).ok_or("unknown field")?;
        unsafe {
            marshal::read_field(
                scope,
                &st,
                field,
                addr as *const u8,
                Mem::Live(Some(this.into())),
            )
        }
    });
    match result {
        Ok(v) => rv.set(v),
        Err(e) => throw_error(scope, &e),
    }
}

fn field_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    let this = args.this();
    let Some((addr, ty)) = this_view(scope, &st, this) else {
        return throw_error(scope, "not a struct");
    };
    let index = args.data().uint32_value(scope).unwrap_or(0) as usize;
    let value = args.get(0);
    let result = st.record(&ty).and_then(|record| {
        let field = record.fields.get(index).ok_or("unknown field")?;
        // strings stored in char* fields must live as long as the struct
        if matches!(field.ty, Type::CString | Type::ConstCString) && value.is_string() {
            let mut bytes = value.to_rust_string_lossy(scope).into_bytes();
            bytes.push(0);
            let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
            let buffer = v8::ArrayBuffer::with_backing_store(scope, &store);
            let ptr = buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0);
            unsafe { ((addr + field.offset) as *mut usize).write_unaligned(ptr) };
            let key = keys::keep_field(scope, &field.name);
            this.set_private(scope, key, buffer.into());
            return Ok(());
        }
        let mut cx = CallCx::default();
        unsafe { marshal::write_field(scope, &st, field, value, addr as *mut u8, &mut cx) }
    });
    if let Err(e) = result {
        throw_error(scope, &e);
    }
}

fn to_json<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let Some((addr, ty)) = this_view(scope, &st, args.this()) else {
        return throw_error(scope, "not a struct");
    };
    match unsafe { marshal::record_to_object(scope, &st, &ty, addr as *const u8) } {
        Ok(o) => rv.set(o.into()),
        Err(e) => throw_error(scope, &e),
    }
}

fn type_name(ty: &Type) -> &str {
    match ty {
        Type::Struct(n) | Type::Union(n) => n,
        Type::AnonymousUnion(..) => "union",
        _ => "struct",
    }
}

/// Class for views of `ty`: field accessors on the prototype, cached per type.
fn template<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    index: u32,
) -> Result<v8::Local<'s, v8::ObjectTemplate>, String> {
    if let Some(t) = st.struct_templates.borrow().get(&index) {
        return Ok(v8::Local::new(scope, t));
    }
    let record = st.record(ty)?;
    let tmpl = v8::FunctionTemplate::new(
        scope,
        |scope: &mut v8::PinScope, _: v8::FunctionCallbackArguments, _: v8::ReturnValue| {
            throw_error(
                scope,
                "struct views are created by the runtime, use the struct's constructor",
            );
        },
    );
    let name = v8::String::new(scope, type_name(ty)).unwrap();
    tmpl.set_class_name(name);
    let proto = tmpl.prototype_template(scope);
    for (i, field) in record.fields.iter().enumerate() {
        if field.name.is_empty() {
            continue;
        }
        let data = v8::Integer::new_from_unsigned(scope, i as u32);
        let getter = v8::FunctionTemplate::builder(field_get)
            .data(data.into())
            .build(scope);
        let setter = v8::FunctionTemplate::builder(field_set)
            .data(data.into())
            .build(scope);
        let key = v8::String::new(scope, &field.name).unwrap();
        proto.set_accessor_property(
            key.into(),
            Some(getter),
            Some(setter),
            v8::PropertyAttribute::NONE,
        );
    }
    let key = v8::String::new(scope, "toJSON").unwrap();
    let json = v8::FunctionTemplate::new(scope, to_json);
    proto.set(key.into(), json.into());
    let instances = tmpl.instance_template(scope);
    instances.set_internal_field_count(handles::FIELDS);
    // instantiates the class so instances get its prototype
    tmpl.get_function(scope)
        .ok_or("failed to create struct class")?;
    st.struct_templates
        .borrow_mut()
        .insert(index, v8::Global::new(scope, instances));
    Ok(instances)
}

/// Live view of the record `ty` at `addr`.
pub fn view<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    addr: usize,
    owner: Option<v8::Local<'s, v8::Value>>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let index = st.register_type(ty);
    let tmpl = template(scope, st, ty, index)?;
    let object = tmpl
        .new_instance(scope)
        .ok_or("failed to create struct view")?;
    handles::init(scope, object, Kind::Struct, addr, Some(index), owner);
    Ok(object.into())
}

/// View over a GC managed copy of the record at `src` (or zeroed memory).
pub fn copy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    src: *const u8,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let (buffer, addr) = allocate(scope, st, ty)?;
    if !src.is_null() {
        let size = st.layout(ty)?.size;
        unsafe { std::ptr::copy_nonoverlapping(src, addr as *mut u8, size) };
    }
    view(scope, st, ty, addr, Some(buffer.into()))
}

/// Zeroed, GC managed memory for a record.
pub fn allocate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
) -> Result<(v8::Local<'s, v8::ArrayBuffer>, usize), String> {
    let size = st.layout(ty)?.size;
    Ok(st.struct_memory(scope, size))
}
