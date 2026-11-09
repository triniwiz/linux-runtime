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

extern "C" {
    // v8::Object::Clone(v8::Isolate*); Local<T> is trivial_abi, returned in a register
    #[link_name = "_ZN2v86Object5CloneEPNS_7IsolateE"]
    fn v8_object_clone(
        this: *const v8::Object,
        isolate: v8::UnsafeRawIsolatePtr,
    ) -> *const v8::Object;
}

/// Shallow copy, internal fields included.
#[inline]
fn clone<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    unsafe { clone_slot(scope, &*o) }
}

/// [`clone`] of the object in a handle slot (a Local's or a Global's).
#[inline]
unsafe fn clone_slot<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    slot: *const v8::Object,
) -> Option<v8::Local<'s, v8::Object>> {
    unsafe {
        let p = v8_object_clone(slot, scope.as_raw_isolate_ptr());
        std::mem::transmute::<*const v8::Object, Option<v8::Local<'s, v8::Object>>>(p)
    }
}

/// Instances of one interop template, copied from a prototype instance: several times
/// cheaper than `ObjectTemplate::NewInstance`.
pub struct Factory {
    template: v8::Global<v8::ObjectTemplate>,
    _prototype: Option<v8::Global<v8::Object>>,
    /// handle slot of `_prototype`, stable while it lives
    proto_slot: *const v8::Object,
    /// address field written in place (layout checked by `tagged::Layout::probe`)
    raw_fields: std::cell::Cell<bool>,
    kind: Kind,
    type_index: Option<u32>,
}

impl Factory {
    pub fn new<'s>(
        scope: &mut v8::PinScope<'s, '_>,
        template: v8::Local<'s, v8::ObjectTemplate>,
        kind: Kind,
        type_index: Option<u32>,
        raw_fields: bool,
    ) -> Self {
        let prototype = template.new_instance(scope).and_then(|o| {
            init(scope, o, kind, 0, type_index, None);
            // a copy must not share field storage with its source (e.g. a V8 sandbox build)
            let copy = clone(scope, o)?;
            set_address(copy, 1);
            (address(o) == 0 && address(copy) == 1 && of(copy.into()).map(|c| c.1) == Some(kind))
                .then(|| v8::Global::new(scope, o))
        });
        let proto_slot = prototype.as_ref().map_or(std::ptr::null(), |p| {
            // SAFETY: the Global belongs to this isolate and lives as long as the factory
            unsafe { v8::Handle::get_unchecked(p) as *const v8::Object }
        });
        Factory {
            template: v8::Global::new(scope, template),
            _prototype: prototype,
            proto_slot,
            raw_fields: std::cell::Cell::new(raw_fields),
            kind,
            type_index,
        }
    }

    /// Handle slot of the prototype instance (null without one).
    pub fn prototype_slot(&self) -> *const usize {
        self.proto_slot as *const usize
    }

    pub fn set_raw_fields(&self, on: bool) {
        self.raw_fields.set(on && !self.proto_slot.is_null());
    }

    /// [`Self::make`] without a scope, for API callbacks: a handle in the callback's
    /// handle scope, or `None` without a prototype instance or raw field writes.
    #[inline]
    pub unsafe fn make_in_callback(
        &self,
        isolate: v8::UnsafeRawIsolatePtr,
        address: usize,
    ) -> Option<*const v8::Object> {
        if self.proto_slot.is_null() || !self.raw_fields.get() {
            return None;
        }
        let o = unsafe { v8_object_clone(self.proto_slot, isolate) };
        if o.is_null() {
            return None;
        }
        unsafe { crate::tagged::set_object_field(&*o, 1, address) };
        Some(o)
    }

    #[inline]
    pub fn make<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        address: usize,
        owner: Option<v8::Local<'s, v8::Value>>,
    ) -> Option<v8::Local<'s, v8::Object>> {
        if !self.proto_slot.is_null() {
            let o = unsafe { clone_slot(scope, self.proto_slot) }?;
            if self.raw_fields.get() {
                unsafe { crate::tagged::set_object_field(&o, 1, address) };
            } else {
                set_address(o, address);
            }
            if let Some(owner) = owner {
                set_owner(o, owner);
            }
            return Some(o);
        }
        let o = v8::Local::new(scope, &self.template).new_instance(scope)?;
        init(scope, o, self.kind, address, self.type_index, owner);
        Some(o)
    }
}
