//! Interop object internal fields: [0] kind, [1] address, [2] type index + 1
//! (aligned pointer slots, stored `<< 1`), [3] owner.

pub const FIELDS: usize = 4;
const TAG: u16 = 0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Pointer = 1,
    Reference = 2,
    Struct = 3,
}

#[inline]
fn get(o: v8::Local<v8::Object>, i: i32) -> usize {
    (unsafe { o.get_aligned_pointer_from_internal_field(i, TAG) } as usize) >> 1
}

#[inline]
fn set(o: v8::Local<v8::Object>, i: i32, v: usize) {
    o.set_aligned_pointer_in_internal_field(i, (v << 1) as *const std::ffi::c_void, TAG);
}

#[inline]
pub fn of(v: v8::Local<v8::Value>) -> Option<(v8::Local<v8::Object>, Kind)> {
    if !v.is_object() {
        return None;
    }
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    if o.internal_field_count() != FIELDS {
        return None;
    }
    let kind = match get(o, 0) {
        1 => Kind::Pointer,
        2 => Kind::Reference,
        3 => Kind::Struct,
        _ => return None,
    };
    Some((o, kind))
}

#[inline]
pub fn is(v: v8::Local<v8::Value>, kind: Kind) -> Option<v8::Local<v8::Object>> {
    of(v).filter(|(_, k)| *k == kind).map(|(o, _)| o)
}

pub fn init<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: v8::Local<'s, v8::Object>,
    kind: Kind,
    address: usize,
    type_index: Option<u32>,
    owner: Option<v8::Local<'s, v8::Value>>,
) {
    set(o, 0, kind as usize);
    set(o, 1, address);
    set_type(o, type_index);
    let owner = owner.unwrap_or_else(|| v8::undefined(scope).into());
    o.set_internal_field(3, owner.into());
}

#[inline]
pub fn address(o: v8::Local<v8::Object>) -> usize {
    get(o, 1)
}

pub fn set_address(o: v8::Local<v8::Object>, address: usize) {
    set(o, 1, address);
}

#[inline]
pub fn type_index(o: v8::Local<v8::Object>) -> Option<u32> {
    get(o, 2).checked_sub(1).map(|i| i as u32)
}

pub fn set_type(o: v8::Local<v8::Object>, type_index: Option<u32>) {
    set(o, 2, type_index.map(|i| i as usize + 1).unwrap_or(0));
}

pub fn owner<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Value>> {
    let data = o.get_internal_field(scope, 3)?;
    let v = v8::Local::<v8::Value>::try_from(data).ok()?;
    (!v.is_undefined()).then_some(v)
}

pub fn set_owner<'s>(o: v8::Local<'s, v8::Object>, owner: v8::Local<'s, v8::Value>) {
    o.set_internal_field(3, owner.into());
}
