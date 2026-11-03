// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Property and array-element access: the inline fast paths and the
//! helpers that own the full semantics.

use super::*;

/// Shared `get_array_el*` lookup: the site's `(shape, prop)` cache (a
/// non-fast array element is an ordinary own shape property), then the full
/// lookup that fills the site.
#[inline]
unsafe fn array_el_get(
    f: &JitExecFrame,
    scope: &mut ScopedContext,
    target: JSValue,
    prop: JSValue,
) -> JSValue {
    #[cfg(feature = "inline_cache")]
    {
        let ic_idx = f.imm2 as usize;
        // SAFETY: single-threaded; the cache is interior-mutable by design.
        let cache = unsafe { &mut *(*f.bytecode).inline_cache.get() };
        if let Some(site) = cache.get_mut(ic_idx) {
            return target
                .get_property_value_ic(scope, prop, site)
                .unwrap_or(JSValue::EXCEPTION);
        }
    }
    target
        .get_property_value(scope, prop)
        .unwrap_or(JSValue::EXCEPTION)
}

/// `get_array_el`: `[array, prop, ^] -> [value, ^]`.
pub(super) unsafe extern "C" fn h_get_array_el(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let value = array_el_get(f, scope, *f.sp.offset(-2), *f.sp.offset(-1));
        let is_err = value.is_exception();
        *f.sp.offset(-2) = value;
        f.sp = f.sp.offset(-1);
        if is_err { HELPER_EXCEPTION } else { HELPER_OK }
    }
}

/// `get_array_el2`: `[array, prop, ^] -> [array, value, ^]`.
pub(super) unsafe extern "C" fn h_get_array_el2(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let value = array_el_get(f, scope, *f.sp.offset(-2), *f.sp.offset(-1));
        let is_err = value.is_exception();
        *f.sp.offset(-1) = value;
        if is_err { HELPER_EXCEPTION } else { HELPER_OK }
    }
}

/// `get_array_el3`: `[array, prop, ^] -> [array, prop1, value, ^]`.
pub(super) unsafe extern "C" fn h_get_array_el3(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let array = *f.sp.offset(-2);
        // The target's null/undefined check comes first: a null target throws
        // before `ToPropertyKey` can run user code.
        if array.is_undefined_or_null() {
            let _ = scope.throw_type_error("value has no property ");
            return HELPER_EXCEPTION;
        }
        // Normalise the key **once**: the read and the later `put_array_el`
        // must use the same property key, or `a[k]++` would run the key's
        // `toString`/`valueOf` twice. An Int/String/Symbol key is already the
        // form both paths handle, so it needs no allocation.
        let prop = *f.sp.offset(-1);
        if !prop.is_int() && !prop.is_string_or_symbol() {
            let Ok(prop1) = prop.to_propkey(scope) else {
                return HELPER_EXCEPTION;
            };
            *f.sp.offset(-1) = prop1;
        }
        let value = array_el_get(f, scope, array, *f.sp.offset(-1));
        let is_err = value.is_exception();
        *f.sp = value;
        f.sp = f.sp.add(1);
        if is_err { HELPER_EXCEPTION } else { HELPER_OK }
    }
}

/// `put_array_el`: `[arr, prop, value, ^] -> [^]`.
pub(super) unsafe extern "C" fn h_put_array_el(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let mut array = *f.sp.offset(-3);
        let prop = *f.sp.offset(-2);
        let value = *f.sp.offset(-1);
        let result = {
            #[cfg(feature = "inline_cache")]
            {
                let ic_idx = f.imm2 as usize;
                // SAFETY: single-threaded; interior-mutable by design.
                let cache = &mut *(*f.bytecode).inline_cache.get();
                match cache.get_mut(ic_idx) {
                    Some(site) => array.set_property_value_ic(
                        scope,
                        prop,
                        value,
                        ThrowPolicy::StrictOnly,
                        false,
                        site,
                    ),
                    None => {
                        array.set_property_value(scope, prop, value, ThrowPolicy::StrictOnly, false)
                    }
                }
            }
            #[cfg(not(feature = "inline_cache"))]
            {
                array.set_property_value(scope, prop, value, ThrowPolicy::StrictOnly, false)
            }
        };
        f.sp = f.sp.offset(-3);
        if result.is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// The property inline guard (the write half): the collector's write barrier for the inline
/// `put_array_el`.
///
/// The generated code stored a value into an array's elements. When that value
/// references a GC node, the tri-color barrier has to run: a black master (the
/// array) pointing at a white slave (the value) would let the collector sweep
/// the value away. `gc_lite`'s `bind` owns that (and the cross-partition
/// bookkeeping), so the generated code only checks the value's tag and leaves
/// the rest to this helper.
///
/// `owner` is the array's GC node pointer (the low 48 bits of the target's
/// `JSValue`), `value` the value that was stored.
pub(crate) unsafe extern "C" fn h_gc_bind(f: *mut JitExecFrame, owner: u64, value: JSValue) {
    unsafe {
        // A value that references no node (a number, a symbol with a constant
        // atom) needs no barrier.
        if value.as_gc_node().is_none() {
            return;
        }
        let Some(master) = std::ptr::NonNull::new(owner as *mut GcHead) else {
            return;
        };
        let scope = scope_of(&*f);
        scope.bind_node_value(master, value);
    }
}

/// Write a `get_field`/`get_field2` result: replace the target, or push (for
/// `get_field2`, which keeps the target).
fn finish_field_get(f: &mut JitExecFrame, value: JSValue, push: bool) -> u32 {
    if push {
        unsafe { *f.sp = value };
        f.sp = unsafe { f.sp.add(1) };
    } else {
        unsafe { *f.sp.offset(-1) = value };
    }
    HELPER_OK
}

/// `get_field` / `get_field2`: the interpreter's inline-cache fast path, then
/// the full lookup that also refreshes the cache. Kept out of generated code so
/// the guard sequence can never drift from `exec_stackless`.
unsafe fn field_get(f: *mut JitExecFrame, push: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let target = *f.sp.offset(-1);
        let atom = crate::atom::Atom::from_id(f.imm);

        #[cfg(feature = "inline_cache")]
        {
            let ic_idx = f.imm2 as usize;
            let cache = &*(*f.bytecode).inline_cache.get();
            if ic_idx < cache.len()
                && let Some(o) = target.as_object()
                && let Some(shape) = o.shape()
            {
                let shape_ptr = shape.gc_ref().as_ptr().as_ptr();
                // The site holds up to IC_WAYS shapes (the property inline cache).
                if let Some((slot, _)) = cache[ic_idx].find(shape_ptr, shape.uid, atom.id()) {
                    let slot = slot as usize;
                    if !matches!(o.prop_slot(slot), JSProperty::Deleted) {
                        return match o.prop_slot(slot).value(scope, o.into()) {
                            Ok(value) => finish_field_get(f, value, push),
                            Err(_) => HELPER_EXCEPTION,
                        };
                    }
                }
            }
        }

        match target.get_property(scope, atom) {
            Ok(value) => {
                #[cfg(feature = "inline_cache")]
                {
                    let ic_idx = f.imm2 as usize;
                    if let Some(o) = target.as_object()
                        && let Some(shape) = o.shape()
                        && let Some((idx, flags)) = o.find_shape_property(atom)
                    {
                        let cache = &mut *(*f.bytecode).inline_cache.get();
                        if ic_idx < cache.len() {
                            cache[ic_idx].insert(
                                shape.gc_ref().as_ptr().as_ptr(),
                                shape.uid,
                                atom.id(),
                                idx as u32,
                                flags as u8,
                            );
                        }
                    }
                    // The typed array's `length` (the property inline guard's tail): the property
                    // lives on the prototype, so the ordinary slot cache
                    // cannot serve it. The site records the instance's shape
                    // and the shape that owns `length` in the chain; the
                    // generated code re-checks both on every access, so a
                    // patched prototype (or an own `length`) is a miss.
                    if atom.id() == crate::atom::Atom::CONST_length.id()
                        && let Some(o) = target.as_object()
                        && (crate::JS_CLASS_UINT8C_ARRAY..=crate::JS_CLASS_FLOAT64_ARRAY)
                            .contains(&o.class_id())
                        && let Some((owner, depth)) = proto_chain_owner_of(&o, atom)
                        // The owner must sit where the generated code looks
                        // for it: two levels up (the typed array's own
                        // prototype is in between). Anything else is served
                        // by the helper.
                        && depth == 2
                        && let Some((idx, _)) = owner.find_shape_property(atom)
                        // ... and it must still carry the intrinsic getter: a
                        // patched prototype has another one, and the guard
                        // would answer the count instead of the getter's
                        // value.
                        && let crate::object::JSProperty::GetSet(gs) = owner.prop_slot(idx)
                        && let Some(getter) = gs.getter
                        && getter.match_native_callback(
                            crate::function::Callback::Getter(
                                crate::builtins::typed_array::js_typed_array_get_length
                                    as crate::function::GetterFn,
                            ),
                            0,
                        )
                        && let Some(owner_shape) = owner.shape()
                        && let Some(instance_shape) = o.shape()
                    {
                        let cache = &mut *(*f.bytecode).inline_cache.get();
                        if ic_idx < cache.len() {
                            cache[ic_idx].insert_typed_array_length(
                                instance_shape.gc_ref().as_ptr().as_ptr(),
                                instance_shape.uid,
                                owner_shape.uid,
                            );
                        }
                    }
                }
                finish_field_get(f, value, push)
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

pub(super) unsafe extern "C" fn h_get_field(f: *mut JitExecFrame) -> u32 {
    unsafe { field_get(f, false) }
}
pub(super) unsafe extern "C" fn h_get_field2(f: *mut JitExecFrame) -> u32 {
    unsafe { field_get(f, true) }
}

/// `put_field`: cached direct slot write when the shape and flags allow it,
/// otherwise the full property set (which refreshes the cache).
pub(super) unsafe extern "C" fn h_put_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let target = *f.sp.offset(-2);
        let value = *f.sp.offset(-1);
        let atom = crate::atom::Atom::from_id(f.imm);

        #[cfg(feature = "inline_cache")]
        {
            let ic_idx = f.imm2 as usize;
            let cache = &*(*f.bytecode).inline_cache.get();
            if ic_idx < cache.len()
                && let Some(o) = target.as_object()
                && let Some(shape) = o.shape()
            {
                let shape_ptr = shape.gc_ref().as_ptr().as_ptr();
                if let Some((slot, cached_flags)) =
                    cache[ic_idx].find(shape_ptr, shape.uid, atom.id())
                {
                    let slot = slot as usize;
                    if !matches!(o.prop_slot(slot), JSProperty::Deleted) {
                        let flags = cached_flags as u32;
                        if flags & crate::object::JS_PROP_TMASK == crate::object::JS_PROP_NORMAL
                            && flags & crate::object::JS_PROP_WRITABLE != 0
                            && flags & crate::object::JS_PROP_LENGTH == 0
                        {
                            let mut o = target.as_object_unchecked();
                            o.set_prop_slot(slot, JSProperty::Normal(value));
                            f.sp = f.sp.offset(-2);
                            return HELPER_OK;
                        }
                    }
                }
            }
        }

        let result =
            target.set_property_internal(scope, atom, value, None, ThrowPolicy::StrictOnly, false);
        f.sp = f.sp.offset(-2);
        if result.is_err() {
            return HELPER_EXCEPTION;
        }

        #[cfg(feature = "inline_cache")]
        if let Some(o) = target.as_object()
            && let Some(shape) = o.shape()
            && let Some((idx, flags)) = o.find_shape_property(atom)
        {
            let ic_idx = f.imm2 as usize;
            let cache = &mut *(*f.bytecode).inline_cache.get();
            if ic_idx < cache.len() {
                cache[ic_idx].insert(
                    shape.gc_ref().as_ptr().as_ptr(),
                    shape.uid,
                    atom.id(),
                    idx as u32,
                    flags as u8,
                );
            }
        }

        HELPER_OK
    }
}
