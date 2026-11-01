// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;
use smallvec::SmallVec;

use crate::{
    JS_CLASS_MODULE_NS,
    atom::Atom,
    config::JS_PROP_INITIAL_SIZE,
    context::ScopedContext,
    object::{LazyInit, Object, ObjectImpl},
    utils::unlikely,
    value::JSValue,
};

use super::{
    JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK,
    JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty, PropertyGetSet,
};

/// The storage behind an object's property slots.
///
/// Upper code (creation, slot access, tracing, compaction) talks to this type
/// only: the concrete container — `SmallVec` with `JS_PROP_INITIAL_SIZE` slots
/// inline today — is an implementation detail, so switching it to a `Vec` (or
/// anything else) is a change local to this type (VM-P1).
///
/// The JIT's property guard does **not** go through here: it reads the
/// `prop_view` mirror, which [`ObjectImpl::sync_prop_view`] keeps pointing at
/// whatever storage this type hands out.
pub(crate) struct PropValues(SmallVec<[JSProperty; JS_PROP_INITIAL_SIZE as usize]>);

impl PropValues {
    /// Empty: no slots, no allocation.
    pub(crate) fn new() -> Self {
        Self(SmallVec::new())
    }

    /// Append `count` default (undefined) slots, reserving at least
    /// `JS_PROP_INITIAL_SIZE` up front. `Err(-1)` on allocation failure.
    pub(crate) fn try_init(&mut self, count: usize) -> Result<(), i32> {
        self.0
            .try_reserve(std::cmp::max(JS_PROP_INITIAL_SIZE as usize, count))
            .map_err(|_| -1)?;
        self.try_extend(count)
    }

    /// Append `extra` default (undefined) slots. `Err(-1)` on allocation failure.
    pub(crate) fn try_extend(&mut self, extra: usize) -> Result<(), i32> {
        self.0.try_reserve(extra).map_err(|_| -1)?;
        self.0
            .extend(std::iter::from_fn(|| Some(JSProperty::default())).take(extra));
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    #[inline(always)]
    pub(crate) fn get(&self, idx: usize) -> &JSProperty {
        debug_assert!(idx < self.len());
        unsafe { self.0.get_unchecked(idx) }
    }

    #[inline(always)]
    pub(crate) fn get_mut(&mut self, idx: usize) -> &mut JSProperty {
        debug_assert!(idx < self.len());
        unsafe { self.0.get_unchecked_mut(idx) }
    }

    #[inline(always)]
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, JSProperty> {
        self.0.iter()
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&JSProperty) -> bool) {
        // Adapter: `Vec::retain` and `SmallVec::retain` disagree on the
        // closure's parameter (`&T` vs `&mut T`); callers only ever see `&T`.
        self.0.retain(|p| keep(p));
    }

    /// The slots' base address — what the JIT's slot mirror records (there is
    /// no reader without the `jit` feature).
    #[cfg(feature = "jit")]
    #[inline(always)]
    pub(crate) fn as_ptr(&self) -> *const JSProperty {
        self.0.as_ptr()
    }
}

impl ObjectImpl {
    /// Debug-only consistency check for the JIT's slot mirror: the engine
    /// reads through `prop_values` (storage-agnostic), the generated code
    /// reads through `prop_view`, so a mutation that forgot to refresh the
    /// mirror would be a wild read in the JIT. Compiled out without the
    /// `jit` feature — there is no mirror there to go stale.
    #[cfg(feature = "jit")]
    #[inline(always)]
    fn debug_assert_prop_view_synced(&self) {
        debug_assert_eq!(
            self.prop_view.data,
            self.prop_values.as_ptr() as *mut JSProperty,
            "stale property view"
        );
    }

    #[cfg(not(feature = "jit"))]
    #[inline(always)]
    fn debug_assert_prop_view_synced(&self) {}

    /// get property slot at `idx`.
    /// in case idx is out of range, behaviour is undefined
    #[inline(always)]
    pub(crate) fn prop_slot(&self, idx: usize) -> &JSProperty {
        debug_assert!(idx < self.prop_values.len());
        self.debug_assert_prop_view_synced();
        self.prop_values.get(idx)
    }

    /// get property slot at `idx`.
    /// in case idx is out of range, behaviour is undefined
    #[inline(always)]
    pub(crate) fn prop_slot_mut(&mut self, idx: usize) -> &mut JSProperty {
        debug_assert!(idx < self.prop_values.len());
        self.debug_assert_prop_view_synced();
        self.prop_values.get_mut(idx)
    }

    /// set property at slot `idx` to non-gc
    #[inline(always)]
    pub(crate) fn set_prop_slot_non_gc(&mut self, idx: usize, p: JSProperty) {
        debug_assert!(idx < self.prop_values.len());
        self.debug_assert_prop_view_synced();

        #[cfg(debug_assertions)]
        match &p {
            JSProperty::Normal(v) => {
                debug_assert!(!v.is_gc());
            }
            JSProperty::Deleted => {}
            _ => {
                unreachable!("expect non-gc property");
            }
        }

        *self.prop_values.get_mut(idx) = p;
    }

    /// Write barrier for the property just stored at slot `idx`: bind every
    /// gc node it references to `self`.
    fn bind_prop_slot(&self, idx: usize) {
        let self_node = self.gc_head_ptr();
        let mut rt = self.rt_ptr();

        match self.prop_slot(idx) {
            JSProperty::Normal(val) => rt.bind_node_value(self_node, *val),
            JSProperty::GetSet(gs) => {
                for o in [gs.getter, gs.setter].into_iter().flatten() {
                    rt.bind_node(self_node, o.gc_node_ptr());
                }
            }
            JSProperty::VarRef(vr) => rt.bind_node(self_node, vr.gc_node_ptr()),
            JSProperty::LazyInit(lz) => {
                if let LazyInit::ModuleNS(m) = &lz.init {
                    rt.bind_node(self_node, m.gc_node_ptr());
                }
            }
            _ => {}
        }
    }

    /// set property at slot `idx`, returns the old one
    pub(crate) fn replace_prop_slot(&mut self, idx: usize, p: JSProperty) -> JSProperty {
        debug_assert!(idx < self.prop_values.len());
        self.debug_assert_prop_view_synced();

        // SAFETY: same contract as `prop_slot` — `idx` is in bounds
        // (debug-checked above); the slot is replaced wholesale.
        let old = unsafe { std::ptr::replace(self.prop_values.get_mut(idx), p) };

        self.bind_prop_slot(idx);

        old
    }

    /// set property at slot `idx`
    pub(crate) fn set_prop_slot(&mut self, idx: usize, p: JSProperty) {
        debug_assert!(idx < self.prop_values.len());
        self.debug_assert_prop_view_synced();

        *self.prop_values.get_mut(idx) = p;

        self.bind_prop_slot(idx);
    }

    /// extend property slot.
    /// Returns error if memory allocation fails.
    pub(crate) fn try_extend_prop_slot(&mut self, extra: usize) -> Result<(), i32> {
        self.prop_values.try_extend(extra)?;
        // The property inline guard: the mirror the JIT reads follows the slots.
        self.sync_prop_view();
        Ok(())
    }

    /// update value to property slot `idx`, handle 'writable' flag setting.
    pub(super) fn update_prop_slot_value(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        prs_flags: u32,
        mut idx: usize,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        //
        // 1. check configurable
        //
        match prs_flags & JS_PROP_TMASK {
            JS_PROP_NORMAL => {
                if unlikely(
                    prs_flags & (JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE) == 0
                        && flags & JS_PROP_HAS_VALUE != 0,
                ) {
                    return if !val.is_same(self.prop_slot(idx).value(ctx, JSValue::UNDEFINED)?) {
                        if ctx.rt().can_throw(flags) {
                            Err(ctx.throw_type_error("property is not configurable"))
                        } else {
                            Ok(false)
                        }
                    } else {
                        Ok(true)
                    };
                }
            }
            JS_PROP_GETSET => {
                // convert getset property to normal type.
                idx = self.modify_shape_property_flags(ctx, prop, |f| {
                    let f2 = f & !(JS_PROP_TMASK | JS_PROP_WRITABLE);
                    debug_assert_eq!(flags & JS_PROP_TMASK, JS_PROP_NORMAL);
                    f2
                })?;
                self.set_prop_slot_non_gc(idx, JSProperty::default());
            }
            JS_PROP_VARREF => {
                // JS_PROP_VARREF is always writable.
            }
            _ => {
                unreachable!()
            }
        }

        //
        // 2. update value, process 'writable' flag
        //

        if prs_flags & JS_PROP_TMASK == JS_PROP_VARREF {
            // varref property
            let class_id = self.class_id();

            if let JSProperty::VarRef(mut vref) = *self.prop_values.get(idx) {
                if flags & JS_PROP_HAS_VALUE != 0 {
                    if unlikely(class_id == JS_CLASS_MODULE_NS) {
                        // JS_PROP_WRITABLE is always true for variable references,
                        // but they are write protected in module name spaces.
                        if !val.is_same(vref.value()) {
                            return if ctx.rt().can_throw(flags) {
                                Err(ctx.throw_type_error("property is not configurable"))
                            } else {
                                Ok(false)
                            };
                        }
                    } else {
                        vref.set_value(&mut ctx.rt_ptr(), val);
                    }
                }

                if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                    // has {writable:false}
                    // 'writable' is set to false, no longer a reference (for mapped arguments)
                    if unlikely(class_id == JS_CLASS_MODULE_NS) {
                        return if ctx.rt().can_throw(flags) {
                            Err(ctx.throw_type_error(
                                "module namespace properties have writable = false",
                            ))
                        } else {
                            Ok(false)
                        };
                    } else {
                        idx = self.modify_shape_property_flags(ctx, prop, |f| {
                            let f2 = f & !(JS_PROP_TMASK | JS_PROP_WRITABLE);
                            debug_assert_eq!(f2 & JS_PROP_TMASK, JS_PROP_NORMAL);
                            f2
                        })?;
                        self.set_prop_slot(idx, JSProperty::Normal(vref.value()));
                    }
                }

                Ok(true)
            } else {
                unreachable!();
            }
        } else if prs_flags & JS_PROP_LENGTH != 0 {
            // the length property
            let res = if flags & JS_PROP_HAS_VALUE != 0 {
                let arr_len = val.to_array_length(ctx, false)?;
                let can_throw = ctx.rt().can_throw(flags);
                unsafe {
                    Object::from_ref(self)
                        .as_Array_mut_unchecked()
                        .set_length(ctx, arr_len, can_throw)
                }
            } else {
                Ok(true)
            };

            if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                // has {writable:false}
                self.modify_shape_property_flags(ctx, prop, |f| f & !JS_PROP_WRITABLE)?;
            }

            res
        } else {
            // normal property
            if flags & JS_PROP_HAS_VALUE != 0 {
                self.set_prop_slot(idx, JSProperty::Normal(val));
            }

            // update `writable` flag
            if flags & JS_PROP_HAS_WRITABLE != 0 {
                self.modify_shape_property_flags(ctx, prop, |f| {
                    f & !JS_PROP_WRITABLE | (flags & JS_PROP_WRITABLE)
                })?;
            }

            Ok(true)
        }
    }

    /// update property as getset type at specified slot `idx`
    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_prop_slot_getset(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        prs_flags: u32,
        mut idx: usize,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let new_getter = if flags & JS_PROP_HAS_GET != 0 && getter.is_callable() {
            getter.as_object()
        } else {
            None
        };
        let new_setter = if flags & JS_PROP_HAS_SET != 0 && setter.is_callable() {
            setter.as_object()
        } else {
            None
        };

        let mut old_getter: Option<Object> = None;
        let mut old_setter: Option<Object> = None;

        if prs_flags & JS_PROP_TMASK != JS_PROP_GETSET {
            // property is not getset type:
            // change shape property type and clear old property value.
            idx = self.modify_shape_property_flags(ctx, prop, |f| {
                (f & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET
            })?;
        } else {
            // alreay is getset type
            if let JSProperty::GetSet(gs) = self.prop_slot(idx) {
                if unlikely(
                    prs_flags & JS_PROP_CONFIGURABLE == 0
                        && ((flags & JS_PROP_HAS_GET != 0 && new_getter != gs.getter)
                            || (flags & JS_PROP_HAS_SET != 0 && new_setter != gs.setter)),
                ) {
                    return if ctx.rt().can_throw(flags) {
                        Err(ctx.throw_type_error("property is not configurable"))
                    } else {
                        Ok(false)
                    };
                } else {
                    if flags & JS_PROP_HAS_GET == 0 {
                        old_getter = gs.getter;
                    }
                    if flags & JS_PROP_HAS_SET == 0 {
                        old_setter = gs.setter;
                    }
                }
            } else {
                unreachable!()
            }
        }

        self.set_prop_slot(
            idx,
            JSProperty::GetSet(Box::new(PropertyGetSet {
                getter: new_getter.or(old_getter),
                setter: new_setter.or(old_setter),
            })),
        );

        Ok(true)
    }
}

#[cfg(all(test, feature = "jit"))]
mod tests {
    use std::mem::size_of;

    use super::*;
    use crate::{JSValue, Runtime, ScopedContext};

    /// VM-P1: with inline slot storage, `prop_view` (the mirror the JIT's
    /// property guard reads) must point at the slots *inside the heap object*.
    /// It is filled only after the object has been moved into the GC heap —
    /// a mirror left at the stack-local address would be a wild read.
    #[test]
    fn slot_mirror_tracks_inline_and_spilled_slots() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        ctx.with_root_scope(|mut cx| {
            let mut obj = cx.create_base_object()?;

            // Two props: within the inline capacity, so the slots live in the
            // object itself and no slot buffer is heap-allocated.
            obj.set_property_str(&mut cx, "a", JSValue::from(1))?;
            obj.set_property_str(&mut cx, "b", JSValue::from(2))?;
            assert_eq!(obj.prop_values.len(), 2);
            assert_eq!(obj.prop_view.data, obj.prop_values.as_ptr() as *mut _);

            let base = obj.id();
            let slots = obj.prop_view.data as usize;
            assert!(
                slots >= base
                    && slots + 2 * size_of::<JSProperty>() <= base + size_of::<ObjectImpl>(),
                "inline slots must live inside the object: {slots:#x} not in {base:#x}"
            );
            assert!(matches!(obj.prop_slot(0), JSProperty::Normal(v) if *v == JSValue::from(1)));
            assert!(matches!(obj.prop_slot(1), JSProperty::Normal(v) if *v == JSValue::from(2)));
            // The guard's range check reads the count from the shape.
            let sh = obj.shape().unwrap();
            assert_eq!(sh.prop_count as usize, sh.len(), "shape count mirror");

            // Grow past the inline capacity: the slots spill to a heap buffer
            // and the mirror must follow them there.
            for name in ["c", "d", "e"] {
                obj.set_property_str(&mut cx, name, JSValue::from(3))?;
            }
            assert_eq!(obj.prop_values.len(), 5);
            assert_eq!(obj.prop_view.data, obj.prop_values.as_ptr() as *mut _);
            let spilled = obj.prop_view.data as usize;
            let outside = spilled < base || spilled >= base + size_of::<ObjectImpl>();
            assert!(
                outside,
                "spilled slots must not be the object's inline area"
            );
            assert!(matches!(obj.prop_slot(4), JSProperty::Normal(v) if *v == JSValue::from(3)));
            let sh = obj.shape().unwrap();
            assert_eq!(sh.prop_count as usize, sh.len(), "shape count mirror");
            Ok(())
        })
    }
}
