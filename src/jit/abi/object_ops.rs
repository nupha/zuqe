// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Object, array and class operations, iteration and literals.

use super::*;

/// `set_name`: name the function on top of the stack (`[obj, ^] -> [obj, ^]`,
/// a peek — so an exception leaves `sp` exactly as the interpreter would).
/// `imm` is the atom id.
pub(super) unsafe extern "C" fn h_set_name(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let atom = crate::atom::Atom::from_id(f.imm);
        let name = (*f.sp.offset(-1))
            .as_object()
            .ok_or(-1)
            .and_then(|mut o| o.define_name(scope, atom, crate::object::JS_PROP_CONFIGURABLE));
        if name.is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `for_of_start`: `[obj, ^] -> [iter_obj, next, catch_offset(0), ^]`. The zero
/// catch offset is the sentinel `find_catch_in_frame` later uses to close the
/// iterator on an abrupt exit, so it must be pushed exactly like the
/// interpreter pushes it.
pub(super) unsafe extern "C" fn h_for_of_start(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        let opened = (*sp.offset(-1))
            .get_iterator_must(scope, false)
            .and_then(|o| {
                o.get_property(scope, crate::atom::Atom::CONST_next)
                    .map(|next| (o, next))
            });
        match opened {
            Ok((iter, next)) => {
                *sp.offset(-1) = JSValue::from(iter);
                *sp = next;
                *sp.offset(1) = JSValue::make_catch_offset(0);
                f.sp = sp.offset(2);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `for_of_next`: calls `next()` on the iterator at `sp[imm]`, pushes
/// `[value, done]`. Mirrors `js_for_of_next` (which also clears the iterator
/// slot on failure so the unwinder does not close it twice).
pub(super) unsafe extern "C" fn h_for_of_next(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        if crate::bytecode::operators::js_for_of_next(scope, sp, -3 - f.imm as i32).is_err() {
            return HELPER_EXCEPTION;
        }
        f.sp = sp.add(2);
        HELPER_OK
    }
}

/// `for_in_start`: `[obj, ^] -> [enum_obj, ^]`.
pub(super) unsafe extern "C" fn h_for_in_start(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        if crate::bytecode::operators::js_for_in_start(scope_of(f), f.sp).is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `for_in_next`: `[enum_obj, ^] -> [enum_obj, value, done, ^]`.
pub(super) unsafe extern "C" fn h_for_in_next(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        if crate::bytecode::operators::js_for_in_next(scope_of(f), f.sp).is_err() {
            HELPER_EXCEPTION
        } else {
            f.sp = f.sp.add(2);
            HELPER_OK
        }
    }
}

/// `iterator_check_object`: the value on top must be an object.
pub(super) unsafe extern "C" fn h_iterator_check_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        if (*f.sp.offset(-1)).is_object() {
            HELPER_OK
        } else {
            let _ = scope_of(f).throw_type_error("iterator must return an object");
            HELPER_EXCEPTION
        }
    }
}

/// `iterator_get_value_done`: `[iter_result, ^] -> [value, done, ^]`.
pub(super) unsafe extern "C" fn h_iterator_get_value_done(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        if crate::bytecode::operators::js_iterator_get_value_done(scope_of(f), f.sp) != 0 {
            HELPER_EXCEPTION
        } else {
            f.sp = f.sp.add(1);
            HELPER_OK
        }
    }
}

/// `iterator_close`: the lowering already dropped `next` and the catch offset,
/// so `sp[-1]` is the iterator. Calls its `return` method like the interpreter.
pub(super) unsafe extern "C" fn h_iterator_close(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let target = *f.sp.offset(-1);
        if let Some(o) = target.as_object()
            && crate::builtins::iterator::ObjectIter::close_iter_internal(scope_of(f), o).is_err()
        {
            return HELPER_EXCEPTION;
        }
        f.sp = f.sp.offset(-1);
        HELPER_OK
    }
}

/// `iterator_next`: `[iter_obj, next, catch_offset, value, ^] -> [.., result, ^]`.
pub(super) unsafe extern "C" fn h_iterator_next(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let result = (*sp.offset(-3)).call(scope_of(f), *sp.offset(-4), &[*sp.offset(-1)]);
        match result {
            Ok(value) => {
                *sp.offset(-1) = value;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `iterator_call`: close (`return`) or forward the throw of the iterator at
/// `sp[-4]`. `imm & 1` selects `throw` instead of `return`, `imm & 2` means no
/// argument. Pushes the returned flag.
pub(super) unsafe extern "C" fn h_iterator_call(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let flags = f.imm as u8;
        let scope = scope_of(f);
        let atom = if flags & 1 != 0 {
            crate::atom::Atom::CONST_throw
        } else {
            crate::atom::Atom::CONST_return
        };
        let Ok(method) = (*sp.offset(-4)).get_property(scope, atom) else {
            return HELPER_EXCEPTION;
        };
        let ret_flag = if method.is_undefined_or_null() {
            true
        } else {
            let args: &[JSValue] = if flags & 2 != 0 {
                &[]
            } else {
                &[*sp.offset(-1)]
            };
            match method.call(scope, *sp.offset(-4), args) {
                Ok(value) => {
                    *sp.offset(-1) = value;
                    false
                }
                Err(_) => return HELPER_EXCEPTION,
            }
        };
        *sp = JSValue::from(ret_flag);
        f.sp = sp.add(1);
        HELPER_OK
    }
}

/// Result of a `class_ops` call: the helper only needs OK/EXCEPTION.
#[inline]
pub(super) const fn status(result: Result<(), i32>) -> u32 {
    if result.is_ok() {
        HELPER_OK
    } else {
        HELPER_EXCEPTION
    }
}

/// `define_field`: `[obj, value, ^] -> [obj, ^]` (`imm` = atom id).
pub(super) unsafe extern "C" fn h_define_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        let r = crate::bytecode::class_ops::define_field(
            scope_of(f),
            &mut *sp.offset(-2),
            atom,
            *sp.offset(-1),
        );
        f.sp = sp.offset(-1);
        status(r)
    }
}

/// `set_name_computed`: `[name, value, ^] -> [name, value, ^]` (peek).
pub(super) unsafe extern "C" fn h_set_name_computed(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        status(crate::bytecode::class_ops::set_name_computed(
            scope_of(f),
            *sp.offset(-2),
            &mut *sp.offset(-1),
        ))
    }
}

/// `set_proto`: `[obj, proto, ^] -> [obj, ^]`.
pub(super) unsafe extern "C" fn h_set_proto(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r =
            crate::bytecode::class_ops::set_proto(scope_of(f), &mut *sp.offset(-2), *sp.offset(-1));
        if r.is_err() {
            return HELPER_EXCEPTION;
        }
        f.sp = sp.offset(-1);
        HELPER_OK
    }
}

/// `set_home_object`: `[home_obj, func, ^] -> [home_obj, func, ^]` (peek).
pub(super) unsafe extern "C" fn h_set_home_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        crate::bytecode::class_ops::set_home_object(
            scope_of(f),
            &mut *sp.offset(-1),
            *sp.offset(-2),
        );
        HELPER_OK
    }
}

/// `define_array_el`: `[obj, prop, value, ^] -> [obj, prop, ^]`.
pub(super) unsafe extern "C" fn h_define_array_el(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::define_array_el(
            scope_of(f),
            &mut *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-1);
        status(r)
    }
}

/// `copy_data_properties`: three operands in place (`imm` = mask).
pub(super) unsafe extern "C" fn h_copy_data_properties(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let mask = f.imm as i32;
        let target = -1 - (mask & 3);
        let source = -1 - ((mask >> 2) & 7);
        let exclude = -1 - ((mask >> 5) & 7);
        let r = crate::bytecode::class_ops::copy_data_properties(
            scope_of(f),
            &mut *sp.offset(target as isize),
            *sp.offset(source as isize),
            *sp.offset(exclude as isize),
        );
        status(r)
    }
}

/// `define_method`: `[obj, func, ^] -> [obj, ^]` (`imm` = atom, `imm2` = flags).
pub(super) unsafe extern "C" fn h_define_method(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::define_method(
            scope_of(f),
            &mut *sp.offset(-2),
            *sp.offset(-1),
            crate::atom::Atom::from_id(f.imm),
            JSValue::UNDEFINED,
            f.imm2 as u8,
        );
        f.sp = sp.offset(-1);
        status(r)
    }
}

/// `define_method_computed`: `[obj, name, func, ^] -> [obj, ^]`.
pub(super) unsafe extern "C" fn h_define_method_computed(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::define_method(
            scope_of(f),
            &mut *sp.offset(-3),
            *sp.offset(-1),
            crate::atom::Atom::NULL,
            *sp.offset(-2),
            f.imm as u8,
        );
        f.sp = sp.offset(-2);
        status(r)
    }
}

/// `define_class` / `define_class_computed` (`imm` = atom, `imm2` = flags).
unsafe fn define_class(f: *mut JitExecFrame, is_computed: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let arena = f.arena;
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let sp = f.sp;
        let captured = captured_vars_of(f);
        let r = crate::bytecode::operators::js_op_define_class_for_arena(
            scope_of(f),
            sp,
            crate::atom::Atom::from_id(f.imm),
            f.imm2 as i32,
            captured,
            &mut *arena,
            frame_idx,
            is_computed,
        );
        status(r)
    }
}

/// `define_class`.
pub(super) unsafe extern "C" fn h_define_class(f: *mut JitExecFrame) -> u32 {
    unsafe { define_class(f, false) }
}

/// `define_class_computed`.
pub(super) unsafe extern "C" fn h_define_class_computed(f: *mut JitExecFrame) -> u32 {
    unsafe { define_class(f, true) }
}

/// `check_ctor_return`: `[ret_val, ^] -> [ret_val, bool, ^]`.
pub(super) unsafe extern "C" fn h_check_ctor_return(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::class_ops::check_ctor_return(
            scope_of(f),
            *sp.offset(-1),
            f.arena,
            FrameIndex::from_u32(f.cur_frame_idx),
        ) {
            Ok(flag) => {
                *sp = JSValue::from(flag);
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `check_ctor`.
pub(super) unsafe extern "C" fn h_check_ctor(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        status(crate::bytecode::class_ops::check_ctor(
            scope_of(f),
            f.new_target,
        ))
    }
}

/// `init_ctor`: `[^] -> [this, ^]`.
pub(super) unsafe extern "C" fn h_init_ctor(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let func_obj = (*f.arena).frame(frame_idx).func_obj;
        let args = std::slice::from_raw_parts(f.arg_base, f.arg_real_count as usize);
        let sp = f.sp;
        match crate::bytecode::class_ops::init_ctor(scope_of(f), func_obj, f.new_target, args) {
            Ok(val) => {
                *sp = val;
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `check_brand`: `[obj, brand, ^] -> [obj, brand, ^]` (peek + throw).
pub(super) unsafe extern "C" fn h_check_brand(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        status(crate::bytecode::class_ops::check_brand(
            scope_of(f),
            *sp.offset(-2),
            *sp.offset(-1),
        ))
    }
}

/// `add_brand`: `[func, home_obj, ^] -> [^]`.
pub(super) unsafe extern "C" fn h_add_brand(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::add_brand(scope_of(f), *sp.offset(-2), *sp.offset(-1));
        f.sp = sp.offset(-2);
        status(r)
    }
}

/// `private_symbol`: `[^] -> [symbol, ^]` (`imm` = atom id).
pub(super) unsafe extern "C" fn h_private_symbol(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        match crate::bytecode::class_ops::private_symbol(scope_of(f), atom) {
            Ok(val) => {
                *sp = val;
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `get_private_field`: `[obj, name, ^] -> [value, ^]`.
pub(super) unsafe extern "C" fn h_get_private_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let val = crate::bytecode::class_ops::get_private_field(
            scope_of(f),
            *sp.offset(-2),
            *sp.offset(-1),
        )
        .unwrap_or(JSValue::EXCEPTION);
        *sp.offset(-2) = val;
        f.sp = sp.offset(-1);
        if val.is_exception() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `put_private_field`: `[obj, name, value, ^] -> [^]`.
pub(super) unsafe extern "C" fn h_put_private_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::put_private_field(
            scope_of(f),
            &mut *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-3);
        status(r)
    }
}

/// `define_private_field`: `[obj, name, value, ^] -> [obj, ^]`.
pub(super) unsafe extern "C" fn h_define_private_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::define_private_field(
            scope_of(f),
            &mut *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-2);
        status(r)
    }
}

/// `append`: `[dest, start, src, ^] -> [dest, end, ^]`.
pub(super) unsafe extern "C" fn h_append(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        if crate::bytecode::operators::js_append_enumerate(scope_of(f), sp).is_err() {
            return HELPER_EXCEPTION;
        }
        f.sp = sp.offset(-1);
        HELPER_OK
    }
}

/// `private_in`: `[obj, field, ^] -> [bool, ^]`.
pub(super) unsafe extern "C" fn h_private_in(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        if crate::bytecode::operators::js_operator_private_in(scope_of(f), sp).is_err() {
            return HELPER_EXCEPTION;
        }
        f.sp = sp.offset(-1);
        HELPER_OK
    }
}

/// `get_super`: `[obj, ^] -> [obj.prototype, ^]`.
pub(super) unsafe extern "C" fn h_get_super(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::class_ops::get_super(scope_of(f), *sp.offset(-1)) {
            Ok(proto) => {
                *sp.offset(-1) = proto;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `get_super_value`: `[this, obj, prop, ^] -> [value, ^]`.
pub(super) unsafe extern "C" fn h_get_super_value(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::class_ops::get_super_value(
            scope_of(f),
            *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        ) {
            Ok(val) => {
                *sp.offset(-3) = val;
                f.sp = sp.offset(-2);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `put_super_value`: `[this, obj, prop, val, ^] -> [^]`.
pub(super) unsafe extern "C" fn h_put_super_value(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::put_super_value(
            scope_of(f),
            *sp.offset(-4),
            *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-4);
        status(r)
    }
}

/// `object`: `[^] -> [obj, ^]`, an empty base object (the seed for an object
/// literal, then filled by `define_field`/`define_array_el`/...).
pub(super) unsafe extern "C" fn h_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = scope_of(f)
            .create_base_object()
            .map_or(JSValue::EXCEPTION, JSValue::from);
        push_status(f, value)
    }
}

/// The merged object literal: `[v1, ..., vN, ^] -> [obj, ^]`, where `imm` is
/// `N` and `lit_atoms[..N]` holds the field atoms (compile-time checked to be
/// pairwise distinct, so the batch transition's "no duplicates" contract holds).
///
/// One trip into Rust builds the whole literal: a fresh base object plus one
/// batched shape transition ([`ObjectImpl::add_own_properties`]) instead of
/// `N` `define_field` round trips. Every value is read straight out of the
/// operand stack before the batch runs, so a collection inside it sees them
/// (the frame is the GC window); the fresh object is unobservable until the
/// helper returns, which is what lets the values be defined after all of them
/// were evaluated.
pub(super) unsafe extern "C" fn h_object_literal(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let n = f.imm as usize;
        debug_assert!((1..=LIT_ATOM_MAX).contains(&n), "literal window is bounded");

        let sp = f.sp;
        let base = sp.sub(n);
        let mut props = [(crate::atom::Atom::NULL, 0u32, JSValue::UNDEFINED); LIT_ATOM_MAX];
        for (i, slot) in props.iter_mut().enumerate().take(n) {
            *slot = (
                crate::atom::Atom::from_id(f.lit_atoms[i]),
                crate::object::JS_PROP_C_W_E,
                *base.add(i),
            );
        }
        let mut obj = match scope_of(f).create_base_object() {
            Ok(obj) => obj,
            Err(_) => return HELPER_EXCEPTION,
        };
        if obj.add_own_properties(scope_of(f), &props[..n]).is_err() {
            return HELPER_EXCEPTION;
        }
        *base = JSValue::from(obj);
        f.sp = base.add(1);
        HELPER_OK
    }
}

/// `object` inside an inlined body: allocate an empty object and leave
/// it in `scratch` instead of the caller's operand stack. The helper cannot
/// collect (an allocation only sets `gc_req`), so the unrooted scratch slot is
/// safe until the generated code reads it in the same basic block.
pub(super) unsafe extern "C" fn h_alloc_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = scope_of(f)
            .create_base_object()
            .map_or(JSValue::EXCEPTION, JSValue::from);
        f.scratch = value;
        if value.is_exception() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `array_from 0` inside an inlined body: an empty array in `scratch`.
pub(super) unsafe extern "C" fn h_alloc_empty_array(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let Ok(list) = crate::array::alloc_fast_array_slots(scope, 0) else {
            return HELPER_EXCEPTION;
        };
        match crate::array::Array::from_values(scope, list) {
            Ok(array) => {
                f.scratch = JSValue::from(array);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `array_from`: `[v0, …, vN-1, ^] -> [array, ^]`; `imm` is `N`. Mirrors the
/// interpreter's `ARRAY_FROM` (the same fast-array slot allocation).
pub(super) unsafe extern "C" fn h_array_from(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let count = f.imm as i32;
        let base = f.sp.offset(-(count as isize));
        let Ok(mut list) = crate::array::alloc_fast_array_slots(scope, count as usize) else {
            return HELPER_EXCEPTION;
        };
        for i in 0..count as usize {
            list.push(*base.add(i));
        }
        let Ok(array) = crate::array::Array::from_values(scope, list) else {
            return HELPER_EXCEPTION;
        };
        f.sp = base;
        *f.sp = JSValue::from(array);
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// `close_loc`: detaches the frame's `var_ref` for local `imm`, so a later
/// write goes through the box instead of the (now dead) frame slot.
pub(super) unsafe extern "C" fn h_close_loc(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let idx = f.imm as u16;
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let mut rt = scope_of(f).rt_ptr();
        (*f.arena).close_loc_ref(frame_idx, idx, rt.gc_heap_mut());
        HELPER_OK
    }
}
