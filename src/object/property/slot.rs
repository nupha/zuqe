// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use gc_lite::{GcHead, GcNode};

use crate::{
    JS_CLASS_MODULE_NS,
    atom::Atom,
    context::ContextImpl,
    object::{LazyInit, Object, ObjectImpl},
    utils::unlikely,
    value::JSValue,
};

use super::{
    JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK,
    JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty,
};

impl ObjectImpl {
    /// get property slot at `idx`.
    /// in case idx is out of range, behaviour is undefined
    #[inline(always)]
    pub(crate) fn prop_slot(&self, idx: usize) -> &JSProperty {
        debug_assert!(idx < self.prop_values.len());
        unsafe { self.prop_values.get_unchecked(idx) }
    }

    /// get property slot at `idx`.
    /// in case idx is out of range, behaviour is undefined
    #[inline(always)]
    pub(crate) fn prop_slot_mut(&mut self, idx: usize) -> &mut JSProperty {
        debug_assert!(idx < self.prop_values.len());
        unsafe { self.prop_values.get_unchecked_mut(idx) }
    }

    /// set property at slot `idx` to non-gc
    #[inline(always)]
    pub(crate) fn set_prop_slot_non_gc(&mut self, idx: usize, p: JSProperty) {
        debug_assert!(idx < self.prop_values.len());

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

        unsafe {
            *self.prop_values.get_unchecked_mut(idx) = p;
        }
    }

    /// set property at slot `idx`, and set gc cross reference
    pub(crate) fn set_prop_slot(&mut self, idx: usize, p: JSProperty) {
        debug_assert!(idx < self.prop_values.len());

        let gc_node: [Option<NonNull<GcHead>>; 2] = match &p {
            JSProperty::Normal(val) => [val.gc_node_ptr(), None],
            JSProperty::GetSet { getter, setter } => [
                getter.map(|o| o.gc_node_ptr()),
                setter.map(|o| o.gc_node_ptr()),
            ],
            JSProperty::VarRef(vr) => [Some(vr.gc_node_ptr()), None],
            JSProperty::LazyInit {
                init_type: LazyInit::ModuleNS(m),
                ..
            } => [Some(m.gc_node_ptr()), None],
            _ => [None, None],
        };

        unsafe {
            *self.prop_values.get_unchecked_mut(idx) = p;
        }

        if gc_node[0].is_some() || gc_node[1].is_some() {
            let self_node = self.gc_head_ptr();
            let mut rt = self.rt();

            for node in gc_node.into_iter().flatten() {
                unsafe {
                    #[cfg(debug_assertions)]
                    node.as_ref().debug_assert_node_valid(rt.gc_heap_mut());

                    rt.gc_heap_mut().bind(self_node, node);
                }
            }
        }
    }

    /// extend property slot
    #[inline]
    pub(crate) fn extend_prop_slot(&mut self, extra: usize) {
        self.prop_values
            .extend(std::iter::from_fn(|| Some(JSProperty::default())).take(extra));
    }

    /// extend property slot.
    /// Returns error if memory allocation fails.
    pub(crate) fn try_extend_prop_slot(&mut self, extra: usize) -> Result<(), i32> {
        self.prop_values.try_reserve_exact(extra).map_err(|_| -1)?;
        self.extend_prop_slot(extra);
        Ok(())
    }
}

impl Object {
    /// update value to property slot `idx`, handle 'writable' flag setting.
    pub(super) fn update_prop_slot_value(
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
        prs_flags: u32,
        idx: usize,
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
                    return if !val
                        .is_same(self.prop_slot(idx).value(ctx.into(), JSValue::UNDEFINED)?)
                    {
                        if ctx.can_throw(flags) {
                            ctx.throw_type_error("property is not configurable");
                            Err(-1)
                        } else {
                            Ok(false)
                        }
                    } else {
                        Ok(true)
                    };
                }
            }
            JS_PROP_GETSET => {
                // convert getset property to normal type
                self.modify_shape_property_flags(ctx, prop, |f| {
                    let f2 = f & !(JS_PROP_TMASK | JS_PROP_WRITABLE);
                    debug_assert_eq!(flags & JS_PROP_TMASK, JS_PROP_NORMAL);
                    f2
                })?;

                // self.shape_prepare_update(ctx)?;
                // let sh = self.shape().unwrap();
                // let prs = unsafe { sh.as_mut().prop_defs.get_mut(&prop.id()).unwrap() };
                // prs.set_flags(prs.flags() & !(JS_PROP_TMASK | JS_PROP_WRITABLE));
                // debug_assert_eq!(prs.flags() & JS_PROP_TMASK, JS_PROP_NORMAL);

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

            if let JSProperty::VarRef(mut vref) = self.prop_values[idx] {
                if flags & JS_PROP_HAS_VALUE != 0 {
                    if unlikely(class_id == JS_CLASS_MODULE_NS) {
                        /* JS_PROP_WRITABLE is always true for variable
                        references, but they are write protected in module name
                        spaces. */
                        if !val.is_same(vref.value()) {
                            return if ctx.can_throw(flags) {
                                ctx.throw_type_error("property is not configurable");
                                Err(-1)
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
                        return if ctx.can_throw(flags) {
                            ctx.throw_type_error(
                                "module namespace properties have writable = false",
                            );
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    } else {
                        self.modify_shape_property_flags(ctx, prop, |f| {
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
                let can_throw = ctx.can_throw(flags);
                unsafe {
                    self.as_Array_mut_unchecked()
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
        ctx: &mut ContextImpl,
        prop: Atom,
        prs_flags: u32,
        idx: usize,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let new_getter = if flags & JS_PROP_HAS_GET != 0 && getter.is_function() {
            getter.as_object().copied()
        } else {
            None
        };
        let new_setter = if flags & JS_PROP_HAS_SET != 0 && setter.is_function() {
            setter.as_object().copied()
        } else {
            None
        };

        let mut old_getter: Option<Object> = None;
        let mut old_setter: Option<Object> = None;

        if prs_flags & JS_PROP_TMASK != JS_PROP_GETSET {
            // property is not getset type:
            // change shape property type and clear old property value.
            self.modify_shape_property_flags(ctx, prop, |f| {
                (f & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET
            })?;

            // self.shape_prepare_update(ctx)?;
            // let sh = self.shape().unwrap();
            // let prs = unsafe { sh.as_mut().prop_defs.get_mut(&prop.id()).unwrap() };
            // prs.set_flags(
            //     (prs.flags() & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET,
            // );
        } else {
            // alreay is getset type
            if let JSProperty::GetSet { getter, setter } = self.prop_slot(idx) {
                if unlikely(
                    prs_flags & JS_PROP_CONFIGURABLE == 0
                        && ((flags & JS_PROP_HAS_GET != 0 && new_getter != *getter)
                            || (flags & JS_PROP_HAS_SET != 0 && new_setter != *setter)),
                ) {
                    return if ctx.can_throw(flags) {
                        ctx.throw_type_error("property is not configurable");
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                } else {
                    if flags & JS_PROP_HAS_GET == 0 {
                        old_getter = *getter;
                    }
                    if flags & JS_PROP_HAS_SET == 0 {
                        old_setter = *setter;
                    }
                }
            } else {
                unreachable!()
            }
        }

        self.set_prop_slot(
            idx,
            JSProperty::GetSet {
                getter: new_getter.or(old_getter),
                setter: new_setter.or(old_setter),
            },
        );

        Ok(true)
    }
}
