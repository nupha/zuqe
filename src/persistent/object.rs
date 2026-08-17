// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_OBJECT, JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JSValue,
    ScopedContext,
    atom::Atom,
    object::{JSPropertyDescriptor, ObjectPayload},
    persistent::{BytecodeSerializeError, DeserializeCtx, SerializeCtx},
};

// Property attribute / descriptor-kind bit flags. These mirror the numeric
// values in `object/property/prop.rs` (which are not re-exported at the crate
// root). Kept in sync with that module.
const JS_PROP_CONFIGURABLE: u32 = 1;
const JS_PROP_WRITABLE: u32 = 1 << 1;
const JS_PROP_ENUMERABLE: u32 = 1 << 2;
const JS_PROP_LENGTH: u32 = 1 << 3;
const JS_PROP_GETSET: u32 = 1 << 4; // TMASK accessor bit
const JS_PROP_HAS_VALUE: u32 = 1 << 13;
const JS_PROP_HAS_GET: u32 = 1 << 11;
const JS_PROP_HAS_SET: u32 = 1 << 12;

// `own_property_names_foreach` flags (defs.h, `i32`). `JS_GPN_SET_ENUM` is not
// re-exported at the crate root, so mirror it here.
const JS_GPN_SET_ENUM: i32 = 1 << 4; // 16

/// Flags passed to `own_property_names_foreach`: include string/index/symbol
/// keys regardless of enumerability (all own properties are serialized).
const GPN_ALL: i32 = JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK | JS_GPN_SET_ENUM;

pub fn serialize_object(
    sc: &mut ScopedContext,
    mut obj: crate::object::Object,
    ctx: &mut SerializeCtx,
) -> Result<(), BytecodeSerializeError> {
    // tag the class so restore can re-bind the correct intrinsic proto.
    let cid = obj.class_id();

    // Only ordinary objects and arrays keep all their observable state in own
    // properties, so only those two classes are serializable today. Every other
    // class (Date, RegExp, Map/Set, TypedArray, ArrayBuffer, Proxy, …) stores
    // state outside own properties and is rejected explicitly instead of being
    // silently truncated (exotic objects store state outside own properties).
    if cid != JS_CLASS_OBJECT && cid != JS_CLASS_ARRAY {
        return Err(BytecodeSerializeError::UnsupportedObject(format!(
            "class_id={cid} (only Object/Array are supported)"
        )));
    }

    ctx.write_u32(cid as u32);

    // Enumerate every own property (string/index/symbol, incl. non-enumerable).
    let mut keys: Vec<(Atom, bool)> = Vec::new();
    obj.own_property_names_foreach(sc, GPN_ALL, |_cx, pk| {
        keys.push((pk.atom, pk.is_enumerable));
        Ok(())
    })
    .map_err(|_| BytecodeSerializeError::UnsupportedObject("own_property_names failed".into()))?;

    ctx.write_u32(keys.len() as u32);
    for (atom, _enumerable) in keys {
        // property keys are atoms — route them through the atom table so repeated
        // names (very common, e.g. `0..n` on an array) are deduplicated
        // inline. Array indices arrive as small-int atoms and stay small-int.
        ctx.write_atom(sc, atom)?;

        let desc = obj
            .get_own_property(sc, atom)
            .map_err(|_| {
                BytecodeSerializeError::UnsupportedObject("get_own_property failed".into())
            })?
            .ok_or_else(|| {
                BytecodeSerializeError::UnsupportedObject(
                    "missing property after enumeration".into(),
                )
            })?;

        serialize_descriptor(sc, desc, ctx)?;
    }
    Ok(())
}

fn serialize_descriptor(
    sc: &mut ScopedContext,
    desc: JSPropertyDescriptor,
    ctx: &mut SerializeCtx,
) -> Result<(), BytecodeSerializeError> {
    let has_get = desc.flags & JS_PROP_GETSET != 0;
    let has_set = has_get; // getter/setter come as a pair in JSPropertyDescriptor
    let is_accessor = has_get || has_set;

    // descriptor kind tag: 1 = accessor (get/set), 0 = data (value)
    let kind: u8 = if is_accessor { 1 } else { 0 };
    ctx.write_u8(kind);

    // attribute flags (configurable / writable / enumerable) occupy the low bits
    let attr: u8 = (if desc.flags & JS_PROP_CONFIGURABLE != 0 {
        1u8
    } else {
        0
    }) | (if desc.flags & JS_PROP_WRITABLE != 0 {
        1u8 << 1
    } else {
        0
    }) | (if desc.flags & JS_PROP_ENUMERABLE != 0 {
        1u8 << 2
    } else {
        0
    });
    ctx.write_u8(attr);

    if is_accessor {
        if has_get {
            ctx.write_u8(1);
            ctx.serialize_value(sc, desc.getter)?;
        } else {
            ctx.write_u8(0);
        }
        if has_set {
            ctx.write_u8(1);
            ctx.serialize_value(sc, desc.setter)?;
        } else {
            ctx.write_u8(0);
        }
    } else {
        // data descriptor: persist the value (or undefined if absent)
        ctx.serialize_value(sc, desc.value)?;
    }
    Ok(())
}

pub fn deserialize_object(inp: &mut DeserializeCtx) -> Result<JSValue, BytecodeSerializeError> {
    // Read the header before borrowing the context, so the two mutable
    // borrows never overlap (the context field and the read cursor are
    // disjoint, but `&mut self` methods borrow the whole struct).
    let cid = inp.read_u32()? as u16;
    let count = inp.read_u32()?;

    // Re-bind to the intrinsic prototype for the recorded class.
    let proto = {
        let sc = &mut *inp.ctx;
        if cid == JS_CLASS_ARRAY {
            sc.class_proto(JS_CLASS_ARRAY).ok_or_else(|| {
                BytecodeSerializeError::InvalidValue("missing Array.prototype".into())
            })?
        } else {
            sc.class_proto(JS_CLASS_OBJECT).ok_or_else(|| {
                BytecodeSerializeError::InvalidValue("missing Object.prototype".into())
            })?
        }
    };

    let mut obj = {
        let sc = &mut *inp.ctx;
        if cid == JS_CLASS_ARRAY {
            sc.create_instance_with_proto_payload(
                Some(proto),
                ObjectPayload::ARRAY(Default::default()),
            )
            .map_err(|_| BytecodeSerializeError::InvalidValue("cannot create array".into()))?
        } else {
            sc.create_base_object_with_proto(Some(proto))
                .map_err(|_| BytecodeSerializeError::InvalidValue("cannot create object".into()))?
        }
    };

    // register the materialized object *before* its contents are read, so a
    // self/back reference (cyclical or shared) inside resolves to this instance.
    inp.register_seen(JSValue::from(obj));

    for _ in 0..count {
        // the key was written as a (deduplicated) atom.
        let atom = inp.read_atom()?;
        // Reconstruct the key value for the array fast-path decision below.
        let key_val = {
            let sc = &mut *inp.ctx;
            atom.to_value(sc)
                .map_err(|_| BytecodeSerializeError::InvalidValue("atom.to_value failed".into()))?
        };

        let kind = inp.read_u8()?;
        let attr = inp.read_u8()?;
        let flags = build_flags(attr, kind == 1);

        if kind == 1 {
            // accessor
            let has_get = inp.read_u8()? == 1;
            let getter = if has_get {
                inp.deserialize_value()?
            } else {
                JSValue::UNDEFINED
            };
            let has_set = inp.read_u8()? == 1;
            let setter = if has_set {
                inp.deserialize_value()?
            } else {
                JSValue::UNDEFINED
            };
            let _ = {
                let sc = &mut *inp.ctx;
                obj.define_property_getset(sc, atom, getter, setter, flags)
                    .map_err(|_| {
                        BytecodeSerializeError::InvalidValue("define_property_getset".into())
                    })?
            };
        } else {
            let val = inp.deserialize_value()?;
            let _ = {
                let sc = &mut *inp.ctx;
                // Array elements must go through the assignment path so they
                // land in the fast array; `define_property_value` would create
                // a sparse shape property whose value the fast array reads as
                // null. For every other (string/symbol) key, define the
                // descriptor with its recorded attributes.
                if cid == JS_CLASS_ARRAY {
                    if let Some(i) = key_val.as_int() {
                        if i >= 0 {
                            obj.set_property_u32(sc, i as u32, val)
                        } else {
                            obj.define_property_value(sc, atom, val, flags)
                        }
                    } else {
                        obj.define_property_value(sc, atom, val, flags)
                    }
                } else {
                    obj.define_property_value(sc, atom, val, flags)
                }
                .map_err(|_| BytecodeSerializeError::InvalidValue("define_property_value".into()))?
            };
        }
    }

    Ok(JSValue::from(obj))
}

#[inline]
fn build_flags(attr: u8, is_accessor: bool) -> u32 {
    let mut f = 0u32;
    if attr & 1 != 0 {
        f |= JS_PROP_CONFIGURABLE;
    }
    if attr & 2 != 0 {
        f |= JS_PROP_WRITABLE;
    }
    if attr & 4 != 0 {
        f |= JS_PROP_ENUMERABLE;
    }
    if is_accessor {
        f |= JS_PROP_HAS_GET | JS_PROP_HAS_SET;
    } else {
        f |= JS_PROP_HAS_VALUE;
    }
    f
}
