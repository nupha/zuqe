// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The inline guards: array elements, object properties and global
//! variables. Each guard answers on its own or hands the access back
//! to the helper.

use super::*;

/// How a typed array's element becomes a `JSValue` — the interpreter's
/// `typed_array_el!` arms, one by one.
#[derive(Clone, Copy)]
enum TaElement {
    /// A signed 8-bit integer → an `Int`.
    I8,
    /// A signed 16-bit integer → an `Int`.
    I16,
    /// A signed 32-bit integer → an `Int`.
    I32,
    /// An unsigned 8-bit integer → an `Int`.
    U8,
    /// An unsigned 16-bit integer → an `Int`.
    U16,
    /// An unsigned 32-bit integer → an `Int`, or a `Float64` when the sign bit
    /// is set (the engine's `From<u32> for JSValue`).
    U32,
    /// A 32-bit float → `From<f64> for JSValue` (an `Int`, or a `Float64`).
    F32,
    /// A 64-bit float → `From<f64> for JSValue` (an `Int`, or a `Float64`).
    F64,
}

/// Emit the load of one typed array element: the address (`base` + the
/// index scaled by the element's width), the load, and the conversion to a
/// `JSValue` — bit for bit what the interpreter does.
fn lower_typed_array_element(
    b: &mut FunctionBuilder,
    base: Value,
    index: Value,
    element: TaElement,
) -> Value {
    // The element's address.
    let (width, kind) = match element {
        TaElement::I8 => (1, 0),
        TaElement::I16 => (2, 1),
        TaElement::I32 => (4, 2),
        TaElement::U8 => (1, 3),
        TaElement::U16 => (2, 4),
        TaElement::U32 => (4, 5),
        TaElement::F32 => (4, 6),
        TaElement::F64 => (8, 7),
    };
    let scaled = b.ins().imul_imm(index, width);
    let addr = b.ins().iadd(base, scaled);
    match kind {
        // A signed integer: widen it, then sign-extend by hand (Cranelift has
        // no `sextend`): shift the bits up and back down arithmetically.
        0 | 1 => {
            let raw = if width == 1 {
                b.ins().load(types::I8, MemFlags::new(), addr, 0)
            } else {
                b.ins().load(types::I16, MemFlags::new(), addr, 0)
            };
            let widened = b.ins().uextend(types::I32, raw);
            let shift = 32 - width * 8;
            let up = b.ins().ishl_imm(widened, shift);
            let signed = b.ins().sshr_imm(up, shift);
            b.ins().uextend(types::I64, signed)
        }
        // An unsigned integer: zero-extend to the `Int` payload.
        3 | 4 => {
            let raw = if width == 1 {
                b.ins().load(types::I8, MemFlags::new(), addr, 0)
            } else {
                b.ins().load(types::I16, MemFlags::new(), addr, 0)
            };
            let widened = b.ins().uextend(types::I32, raw);
            b.ins().uextend(types::I64, widened)
        }
        2 => {
            let raw = b.ins().load(types::I32, MemFlags::new(), addr, 0);
            b.ins().uextend(types::I64, raw)
        }
        5 => {
            let raw = b.ins().load(types::I32, MemFlags::new(), addr, 0);
            box_bit_result(b, raw)
        }
        6 => {
            let raw = b.ins().load(types::F32, MemFlags::new(), addr, 0);
            let wide = b.ins().fpromote(types::F64, raw);
            box_float64_from_f64(b, wide)
        }
        _ => {
            let raw = b.ins().load(types::F64, MemFlags::new(), addr, 0);
            box_float64_from_f64(b, raw)
        }
    }
}

/// The two shapes a **property** read has: the value takes the target's
/// place (`get_field`), or the target stays and the value is pushed next to it
/// (`get_field2` — the method-call shape).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldRead {
    /// `[target] -> [value]`: the value replaces the target.
    Replace,
    /// `[target] -> [target, value]`: the value is pushed.
    Keep,
}

impl FieldRead {
    /// The slot the result lands in, relative to the stack pointer.
    const fn result_offset(self) -> i32 {
        match self {
            // The target's slot.
            FieldRead::Replace => -8,
            // The slot above the target.
            FieldRead::Keep => 0,
        }
    }

    /// The stack pointer after the instruction.
    fn sp_next(self, b: &mut FunctionBuilder, sp: Value) -> Value {
        match self {
            // The value replaced the target: the same number of slots.
            FieldRead::Replace => sp,
            // The value was pushed: one slot more.
            FieldRead::Keep => b.ins().iadd_imm(sp, 8),
        }
    }
}

/// The two shapes an **array element** read has: the value takes the array's
/// place (`get_array_el`), or the array stays and the value takes the key's
/// place (`get_array_el2` — the method-call shape).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ArrayRead {
    /// `[array, prop] -> [value]`: the value replaces the array.
    Replace,
    /// `[array, prop] -> [array, value]`: the value replaces the key.
    Keep,
}

impl ArrayRead {
    /// The slot the result lands in, relative to the stack pointer.
    const fn result_offset(self) -> i32 {
        match self {
            ArrayRead::Replace => -16,
            ArrayRead::Keep => -8,
        }
    }

    /// The stack pointer after the instruction.
    fn sp_next(self, b: &mut FunctionBuilder, sp: Value) -> Value {
        match self {
            // The value replaced the array: one slot fewer.
            ArrayRead::Replace => b.ins().iadd_imm(sp, -8),
            // The value replaced the key: the same number of slots.
            ArrayRead::Keep => sp,
        }
    }
}

/// The inline-cache site of a `get_field`/`put_field` and its first way's
/// recorded shape, id and property slot. The sites live in the frame, so the
/// guard reads them per access and the load stays out of the entry block.
fn load_ic_way(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    insn: &Instruction,
) -> (Value, Value, Value, Value) {
    let ic_sites = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.frame, O_IC_SITES);
    let site = b
        .ins()
        .iadd_imm(ic_sites, i64::from(ic_index(insn)) * ic_layout::SITE_SIZE);
    let shape = b.ins().load(
        types::I64,
        MemFlags::new(),
        site,
        ic_layout::SHAPE_PTR_OFFSET,
    );
    let uid = b.ins().load(
        types::I32,
        MemFlags::new(),
        site,
        ic_layout::SHAPE_UID_OFFSET,
    );
    let slot = b
        .ins()
        .load(types::I32, MemFlags::new(), site, ic_layout::SLOT_OFFSET);
    (site, shape, uid, slot)
}

/// The property and its flags recorded in the same way — the write guard needs
/// them, the read guard does not.
fn load_ic_property(b: &mut FunctionBuilder, site: Value) -> (Value, Value) {
    let prop = b
        .ins()
        .load(types::I32, MemFlags::new(), site, ic_layout::PROP_OFFSET);
    let flags = b
        .ins()
        .load(types::I8, MemFlags::new(), site, ic_layout::FLAGS_OFFSET);
    (prop, flags)
}

/// A property slot's address (the property array plus the slot's stride) and
/// the "still a plain data property" test: `delete` leaves a tombstone behind
/// *without* changing the shape, and an accessor has its own tag, so every
/// guard re-checks the tag on each access.
fn data_property_slot(b: &mut FunctionBuilder, slots: Value, slot: Value) -> (Value, Value) {
    let offset = b.ins().imul_imm(slot, JSProperty::slot_size());
    let addr = b.ins().iadd(slots, offset);
    let tag = b
        .ins()
        .load(types::I8, MemFlags::new(), addr, JSProperty::tag_offset());
    let is_data = b
        .ins()
        .icmp_imm(IntCC::Equal, tag, i64::from(PROP_TAG_NORMAL));
    (addr, is_data)
}

/// The object's shape against the one the way recorded: mask the tagged
/// target, load its shape node and compare it with the way's payload pointer,
/// then (after the match) load the shape's id. Returns the object pointer,
/// the shape node and the id comparison; branches to `slow` on a mismatch.
fn shape_guard(
    b: &mut FunctionBuilder,
    target: Value,
    way_shape: Value,
    way_uid: Value,
    slow: Block,
) -> (Value, Value, Value) {
    // The object's pointer: the low 48 bits of the tagged value.
    let obj = b.ins().band_imm(target, 0x0000_ffff_ffff_ffff);
    // The object's shape: the GC node pointer. An object without a shape has
    // no own properties, so it cannot match a recorded shape.
    let shape_node = b
        .ins()
        .load(types::I64, MemFlags::new(), obj, jit_layout::SHAPE_OFFSET);
    // The inline cache records the shape's *payload* pointer.
    let shape_payload = b
        .ins()
        .iadd_imm(shape_node, i64::from(jit_layout::SHAPE_PAYLOAD_OFFSET));
    // The way must have seen a shape (an empty way holds a null pointer), and
    // it must be the object's shape.
    let used = b.ins().icmp_imm(IntCC::NotEqual, way_shape, 0);
    let same_shape = b.ins().icmp(IntCC::Equal, way_shape, shape_payload);
    let shape_ok = b.ins().band(used, same_shape);
    let matched = b.create_block();
    b.ins().brif(shape_ok, matched, &[], slow, &[]);

    b.switch_to_block(matched);
    // The shape's id: the second half of the cache key. A shape's memory can
    // be reused by another shape, and only the id tells them apart. The shape
    // matched a recorded one, so it is a live shape and the id is safe to read.
    let shape_uid = b.ins().load(
        types::I32,
        MemFlags::new(),
        shape_node,
        jit_layout::SHAPE_UID_OFFSET,
    );
    let same_uid = b.ins().icmp(IntCC::Equal, way_uid, shape_uid);
    (obj, shape_node, same_uid)
}

/// The typed-array prologue shared by the element read and write guards: the
/// view's payload (buffer, element count, out-of-bounds flag), the index
/// bounds check, and the buffer's byte view. Returns the element region's
/// base address, the "directly readable" predicate and the zero-extended
/// index, leaving the builder in the block that passed the bounds check — the
/// caller branches on the predicate.
fn typed_array_view(
    b: &mut FunctionBuilder,
    obj: Value,
    prop: Value,
    slow: Block,
) -> (Value, Value, Value) {
    let ta_payload = b
        .ins()
        .load(types::I64, MemFlags::new(), obj, jit_layout::PAYLOAD_OFFSET);
    let buffer = b.ins().load(
        types::I64,
        MemFlags::new(),
        ta_payload,
        ta_layout::BUFFER_OFFSET,
    );
    let view_offset = b.ins().load(
        types::I32,
        MemFlags::new(),
        ta_payload,
        ta_layout::OFFSET_OFFSET,
    );
    let count = b.ins().load(
        types::I32,
        MemFlags::new(),
        ta_payload,
        ta_layout::COUNT_OFFSET,
    );
    let out_of_bounds = b.ins().load(
        types::I8,
        MemFlags::new(),
        ta_payload,
        ta_layout::OUT_OF_BOUNDS_OFFSET,
    );
    // The index must be inside the view's elements.
    let index = b.ins().band_imm(prop, 0xffff_ffff);
    let count64 = b.ins().uextend(types::I64, count);
    let in_count = b.ins().icmp(IntCC::UnsignedLessThan, index, count64);
    let ta_buffer = b.create_block();
    b.ins().brif(in_count, ta_buffer, &[], slow, &[]);

    b.switch_to_block(ta_buffer);
    // The buffer's payload and the view of its bytes.
    let buf_payload = b.ins().load(
        types::I64,
        MemFlags::new(),
        buffer,
        jit_layout::PAYLOAD_OFFSET,
    );
    let data = b.ins().load(
        types::I64,
        MemFlags::new(),
        buf_payload,
        buffer_layout::VIEW_OFFSET + buffer_layout::VIEW_DATA_OFFSET,
    );
    let direct = b.ins().load(
        types::I32,
        MemFlags::new(),
        buf_payload,
        buffer_layout::VIEW_OFFSET + buffer_layout::VIEW_DIRECT_OFFSET,
    );
    let detached = b.ins().load(
        types::I8,
        MemFlags::new(),
        buf_payload,
        buffer_layout::DETACHED_OFFSET,
    );
    // A detached buffer, a view out of bounds and a shared buffer (whose
    // bytes live behind a lock) all go back to the helper.
    let not_detached = b.ins().icmp_imm(IntCC::Equal, detached, 0);
    let not_oob = b.ins().icmp_imm(IntCC::Equal, out_of_bounds, 0);
    let is_direct = b.ins().icmp_imm(IntCC::NotEqual, direct, 0);
    let flags_ok = b.ins().band(not_oob, is_direct);
    let readable = b.ins().band(not_detached, flags_ok);
    // The element's address: the buffer's bytes plus the view's offset.
    let view_base = b.ins().uextend(types::I64, view_offset);
    let base = b.ins().iadd(data, view_base);
    (base, readable, index)
}

/// The property inline guard: the inline fast path for `get_array_el` — a read from a **packed**
/// array with an **integer** index.
///
/// The guard checks, in order: the key's tag (`Int`), the target's tag
/// (`Object`), the target's class (a plain `Array` or an `arguments` object),
/// its fast-array flag, and the index against the array's length. A typed
/// array (`Int32Array` / `Float64Array`) has its own path: its elements live
/// in an `ArrayBuffer`, so the guard checks the view's element count, the
/// out-of-bounds flag, the buffer's detached flag and whether its bytes are
/// directly readable. Anything else falls back to the helper, which owns the
/// full semantics (dictionary arrays, holes, float or string keys, the
/// prototype chain, …).
///
/// The reads go through the layout contracts in [`super::layout`]: the
/// generated code loads the object's flags, its payload pointer and the
/// elements directly. Everything is read fresh on every access, so a shape
/// change (the array stops being fast, it grows, it is replaced) is a
/// **guard miss**, not a stale read — no invalidation channel is needed.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_get_array_el_fast(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp_var: Variable,
    sp: Value,
    insn: &Instruction,
    shape: ArrayRead,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let top = state.stack.height;
    let array = state.stack.read(b, sp, top - 1);
    let prop = state.stack.read(b, sp, top);

    // Both checks come before any load, so a non-object value is never
    // dereferenced: the key must be an `Int` and the target an object.
    let prop_is_int = is_int(b, prop);
    let array_is_object = is_object(b, array);
    let guard = b.ins().band(prop_is_int, array_is_object);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // A later instruction may reuse this one's result, so it has to leave
    // this instruction in a register (`cont` carries it as a parameter).
    let keep_id = state.cse_keep(insn.pc());
    let kept = keep_id.is_some();
    if kept {
        b.append_block_param(cont, types::I64);
    }
    // The operands stay in registers on the fast path; the helper's own
    // preamble flushes them on the slow path.
    state.stack.flush_except(b, sp, &[top - 1, top]);
    b.ins().brif(guard, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    // `get_array_el2` keeps the array below the value that replaces the key,
    // but `cont` drops both register copies; memory must hold the array by
    // then, or the next flush (the call helper right after) would see a stale
    // slot. `Replace` overwrites the array's slot instead.
    if shape == ArrayRead::Keep {
        b.ins().store(MemFlags::new(), array, sp, -16);
    }
    // The object's pointer: the low 48 bits of the tagged value.
    let obj = b.ins().band_imm(array, 0x0000_ffff_ffff_ffff);
    // Its flags word: the class id and the fast-array bit.
    let flags = b
        .ins()
        .load(types::I32, MemFlags::new(), obj, jit_layout::FLAGS_OFFSET);
    let class_id = b.ins().band_imm(flags, jit_layout::CLASS_ID_MASK);
    // A plain `Array` or an `arguments` object: both carry their elements in
    // the array payload.
    let is_array = b
        .ins()
        .icmp_imm(IntCC::Equal, class_id, i64::from(JS_CLASS_ARRAY));
    let is_arguments = b
        .ins()
        .icmp_imm(IntCC::Equal, class_id, i64::from(JS_CLASS_ARGUMENTS));
    let fast_bit = b.ins().band_imm(flags, jit_layout::FAST_ARRAY_MASK);
    let is_fast = b.ins().icmp_imm(IntCC::NotEqual, fast_bit, 0);
    let is_array_or_arguments = b.ins().bor(is_array, is_arguments);
    let shape_ok = b.ins().band(is_array_or_arguments, is_fast);
    // The typed arrays we inline: every width the interpreter has an arm
    // for. Their elements live in an `ArrayBuffer`, not in the array payload.
    // The class ids are contiguous (`JS_CLASS_UINT8C_ARRAY` ..
    // `JS_CLASS_FLOAT64_ARRAY`), so the range check is two compares.
    let typed_low = b.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        class_id,
        i64::from(JS_CLASS_UINT8C_ARRAY),
    );
    let typed_high = b.ins().icmp_imm(
        IntCC::UnsignedLessThanOrEqual,
        class_id,
        i64::from(JS_CLASS_FLOAT64_ARRAY),
    );
    let is_typed = b.ins().band(typed_low, typed_high);
    let ta_check = b.create_block();
    let plain_check = b.create_block();
    b.ins().brif(is_typed, ta_check, &[], plain_check, &[]);

    b.switch_to_block(plain_check);
    // The payload is only read once the class says it is an array: a plain
    // object has no payload, and reading it would be a wild load.
    let load = b.create_block();
    b.ins().brif(shape_ok, load, &[], slow, &[]);

    b.switch_to_block(load);
    // The payload and the elements it holds.
    let payload = b
        .ins()
        .load(types::I64, MemFlags::new(), obj, jit_layout::PAYLOAD_OFFSET);
    let data = b.ins().load(
        types::I64,
        MemFlags::new(),
        payload,
        array_layout::DATA_OFFSET,
    );
    let len = b.ins().load(
        types::I32,
        MemFlags::new(),
        payload,
        array_layout::LEN_OFFSET,
    );
    // The index: an `Int` payload is the low 32 bits. A negative index
    // becomes a huge unsigned value and fails the bounds check.
    let index = b.ins().band_imm(prop, 0xffff_ffff);
    let len64 = b.ins().uextend(types::I64, len);
    let in_bounds = b.ins().icmp(IntCC::UnsignedLessThan, index, len64);
    let hit = b.create_block();
    b.ins().brif(in_bounds, hit, &[], slow, &[]);

    b.switch_to_block(hit);
    // The element itself: `data + index * 8`.
    let offset = b.ins().imul_imm(index, 8);
    let addr = b.ins().iadd(data, offset);
    let value = b.ins().load(types::I64, MemFlags::new(), addr, 0);
    // The result lands in the slot the array occupied (`get_array_el`) or in
    // the slot the key occupied (`get_array_el2`, which keeps the array).
    b.ins()
        .store(MemFlags::new(), value, sp, shape.result_offset());
    let sp_next = shape.sp_next(b, sp);
    b.def_var(sp_var, sp_next);
    let keep = [value];
    b.ins().jump(cont, if kept { &keep } else { &[] });

    // ── The typed arrays ──────────────────────────────────────────────
    //
    // Their elements live in an `ArrayBuffer`: the view (offset, byte length,
    // element count) is in the typed array's payload, the bytes are in the
    // buffer's payload. Everything is read fresh, so a resize, a detach or a
    // view that went out of bounds is a guard miss, not a stale read.
    b.switch_to_block(ta_check);
    let (base, readable, index) = typed_array_view(b, obj, prop, slow);
    let ta_kind = b.create_block();
    b.ins().brif(readable, ta_kind, &[], slow, &[]);

    b.switch_to_block(ta_kind);
    // The class decides the element's width and how it becomes a `JSValue`:
    // a jump table over the typed array classes. `BigInt64Array`,
    // `BigUint64Array` and `Float16Array` have no plain-load arm (the
    // interpreter has none either), so they go back to the helper.
    let kind = b.ins().iadd_imm(class_id, -(JS_CLASS_UINT8C_ARRAY as i64));
    let ta_u8 = b.create_block();
    let ta_i8 = b.create_block();
    let ta_i16 = b.create_block();
    let ta_u16 = b.create_block();
    let ta_i32 = b.create_block();
    let ta_u32 = b.create_block();
    let ta_f32 = b.create_block();
    let ta_f64 = b.create_block();
    // The jump table's entries, in class order.
    let mut entries = Vec::with_capacity(12);
    for block in [
        ta_u8,  // Uint8ClampedArray
        ta_i8,  // Int8Array
        ta_u8,  // Uint8Array
        ta_i16, // Int16Array
        ta_u16, // Uint16Array
        ta_i32, // Int32Array
        ta_u32, // Uint32Array
        slow,   // BigInt64Array
        slow,   // BigUint64Array
        slow,   // Float16Array
        ta_f32, // Float32Array
        ta_f64, // Float64Array
    ] {
        entries.push(b.func.dfg.block_call(block, &[]));
    }
    let default = b.func.dfg.block_call(slow, &[]);
    let table = b.create_jump_table(JumpTableData::new(default, &entries));
    b.ins().br_table(kind, table);

    // Every arm loads the element and leaves it in the slot the array
    // occupied, like the plain-array path.
    let arm = |b: &mut FunctionBuilder, element: TaElement| {
        let value = lower_typed_array_element(b, base, index, element);
        b.ins()
            .store(MemFlags::new(), value, sp, shape.result_offset());
        let sp_next = shape.sp_next(b, sp);
        b.def_var(sp_var, sp_next);
        let keep = [value];
        b.ins().jump(cont, if kept { &keep } else { &[] });
    };

    b.switch_to_block(ta_u8);
    arm(b, TaElement::U8);
    b.switch_to_block(ta_i8);
    arm(b, TaElement::I8);
    b.switch_to_block(ta_i16);
    arm(b, TaElement::I16);
    b.switch_to_block(ta_u16);
    arm(b, TaElement::U16);
    b.switch_to_block(ta_i32);
    arm(b, TaElement::I32);
    b.switch_to_block(ta_u32);
    arm(b, TaElement::U32);
    b.switch_to_block(ta_f32);
    arm(b, TaElement::F32);
    b.switch_to_block(ta_f64);
    arm(b, TaElement::F64);

    b.switch_to_block(slow);
    // The site index rides in `imm2` (array element keys are runtime values,
    // so the cached property atom lives in the cache entry).
    #[cfg(feature = "inline_cache")]
    {
        let ic = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
        b.ins().store(MemFlags::new(), ic, fv.frame, O_IMM2);
    }
    let helper = match shape {
        ArrayRead::Replace => HELPER_GET_ARRAY_EL,
        ArrayRead::Keep => HELPER_GET_ARRAY_EL2,
    };
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper, pc_after);
    b.def_var(sp_var, sp_slow);
    let mut slow_result = [sp_slow];
    if kept {
        slow_result[0] = b.ins().load(types::I64, MemFlags::new(), sp_slow, -8);
    }
    b.ins().jump(cont, if kept { &slow_result } else { &[] });

    b.switch_to_block(cont);
    // The operand entries are stale now (a flush would overwrite the result).
    match shape {
        ArrayRead::Replace => state.stack.consumed_binary(),
        ArrayRead::Keep => state.stack.replaced_pair(),
    }
    if let (Some(keep_id), Some(&value)) = (keep_id, b.block_params(cont).first()) {
        state.cse_regs.insert(keep_id, value);
    }
    Step::Continue(b.use_var(sp_var))
}

/// The write barrier's helper signature: `(frame, owner, value)`. The helper
/// takes its arguments directly, so it needs no operand-stack shape and no
/// status — the barrier cannot fail.
pub(super) fn barrier_sig(
    b: &mut FunctionBuilder,
    helper_sig: cranelift_codegen::ir::SigRef,
) -> cranelift_codegen::ir::SigRef {
    let conv = b.func.dfg.signatures[helper_sig].call_conv;
    let mut sig = Signature::new(conv);
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    b.import_signature(sig)
}

/// The property inline guard (the write half): the inline fast path for `put_array_el` — a store
/// into a **packed** array with an **integer** index.
///
/// The guard mirrors the engine's own fast write
/// (`ObjectImpl::set_property_u32_internal`): the key's tag (`Int`), the
/// target's tag (`Object`), the target's class (a plain `Array` or an
/// `arguments` object), the fast-array flag, the extensibility of a plain
/// array, and the index against the array's length. Everything else — a
/// dictionary array, a float or string key, a negative index, a store that
/// grows the array, a frozen array — falls back to the helper, which owns the
/// full semantics. A typed array has its own path: the element's width comes
/// from its class, and a numeric value (`Int`/`Float64`) is converted exactly
/// like the interpreter (`ToInt32`, the clamped rounding, `ToNumber`); a
/// BigInt, f16 or non-numeric value still goes back to the helper.
///
/// A stored value that references a GC node needs the collector's write
/// barrier (a black array pointing at a white value would let the collector
/// sweep the value away). The generated code checks the value's tag and calls
/// [`h_gc_bind`](super::abi::h_gc_bind) for the rest — the barrier is the
/// collector's business, not the generated code's.
pub(super) fn lower_put_array_el_fast(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp_var: Variable,
    sp: Value,
    insn: &Instruction,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let top = state.stack.height;
    let array = state.stack.read(b, sp, top - 2);
    let prop = state.stack.read(b, sp, top - 1);
    let value = state.stack.read(b, sp, top);

    // Both checks come before any load, so a non-object value is never
    // dereferenced: the key must be an `Int` and the target an object.
    let prop_is_int = is_int(b, prop);
    let array_is_object = is_object(b, array);
    let guard = b.ins().band(prop_is_int, array_is_object);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // The paths leave different stack pointers behind (the fast path pops the
    // three operands itself, the helper pops them in the frame), so the
    // continuation carries it.
    b.append_block_param(cont, types::I64);
    // The plain array's arm, the typed array's arm, and the blocks they use.
    let plain_array_arm = b.create_block();
    let typed_blk = b.create_block();
    let ta_view = b.create_block();
    let ta_kind = b.create_block();
    let ta_done = b.create_block();
    // The operands stay in registers on the fast path; the helper's own
    // preamble flushes them on the slow path.
    state.stack.flush_except(b, sp, &[top - 2, top - 1, top]);
    b.ins().brif(guard, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    // The object's pointer: the low 48 bits of the tagged value.
    let obj = b.ins().band_imm(array, 0x0000_ffff_ffff_ffff);
    // Its flags word: the class id, the fast-array bit and the extensible bit.
    let flags = b
        .ins()
        .load(types::I32, MemFlags::new(), obj, jit_layout::FLAGS_OFFSET);
    // The class id and the fast-array bit, read together: a fast plain `Array`
    // is `fast | Array`, a fast `arguments` object is `fast | Arguments`. One
    // masked word and two compares cover both shapes — and a target that is
    // neither (a typed array, a plain object) leaves after them, without
    // touching the rest of the flags.
    let class_and_fast = b.ins().band_imm(flags, WRITE_CLASS_MASK);
    let is_array = b
        .ins()
        .icmp_imm(IntCC::Equal, class_and_fast, WRITE_ARRAY_FLAGS);
    let is_arguments = b
        .ins()
        .icmp_imm(IntCC::Equal, class_and_fast, WRITE_ARGUMENTS_FLAGS);
    let array_or_arguments = b.ins().bor(is_array, is_arguments);
    // A target that is neither is a typed array (its own arm below) or
    // something the helper owns.
    b.ins()
        .brif(array_or_arguments, plain_array_arm, &[], typed_blk, &[]);

    b.switch_to_block(typed_blk);
    // The class id decides: a typed array has its own arm (its elements live
    // in an `ArrayBuffer`, not in a payload), everything else is the
    // helper's.
    let class_id = b.ins().band_imm(flags, jit_layout::CLASS_ID_MASK);
    let typed_low = b.ins().icmp_imm(
        IntCC::SignedGreaterThanOrEqual,
        class_id,
        i64::from(JS_CLASS_UINT8C_ARRAY),
    );
    let typed_high = b.ins().icmp_imm(
        IntCC::SignedLessThanOrEqual,
        class_id,
        i64::from(JS_CLASS_FLOAT64_ARRAY),
    );
    let is_typed = b.ins().band(typed_low, typed_high);
    b.ins().brif(is_typed, ta_view, &[], slow, &[]);

    // ── The typed array ───────────────────────────────────────────────
    //
    // Its elements live in an `ArrayBuffer`: the view (offset, element count)
    // is in the typed array's payload, the bytes are in the buffer's payload.
    // Everything is read fresh, so a resize, a detach or a view that went out
    // of bounds is a guard miss, not a stale write.
    b.switch_to_block(ta_view);
    let (base, readable, index) = typed_array_view(b, obj, prop, slow);
    b.ins().brif(readable, ta_kind, &[], slow, &[]);

    b.switch_to_block(ta_kind);
    // The value's tags, computed before the jump table (which ends this
    // block): only an `Int` or a `Float64` skips `ToNumber`, which may run
    // user code. Everything else goes back to the helper. Each arm checks its
    // usual tag first, so the pre-existing hot paths keep one branch.
    let int_value = is_int(b, value);
    let float_value = is_float64(b, value);
    // An `Int`'s `ToInt32` is its low 32 bits; a `Float64`'s is the engine's
    // `f64_to_i32`, emitted per arm so the integer stores do not pay for it.
    let int32 = b.ins().ireduce(types::I32, value);
    // The class decides the element's width and how the value becomes it: a
    // jump table over the typed array classes. `BigInt64Array`,
    // `BigUint64Array` and `Float16Array` have no arm here (the interpreter's
    // conversion is more than a number conversion), so they go back to the
    // helper.
    let kind = b.ins().iadd_imm(class_id, -(JS_CLASS_UINT8C_ARRAY as i64));
    let ta_clamped = b.create_block();
    let ta_int8 = b.create_block();
    let ta_int16 = b.create_block();
    let ta_int32 = b.create_block();
    let ta_f32 = b.create_block();
    let ta_f64 = b.create_block();
    // The jump table's entries, in class order.
    let mut entries = Vec::with_capacity(12);
    for block in [
        ta_clamped, // Uint8ClampedArray
        ta_int8,    // Int8Array
        ta_int8,    // Uint8Array
        ta_int16,   // Int16Array
        ta_int16,   // Uint16Array
        ta_int32,   // Int32Array
        ta_int32,   // Uint32Array
        slow,       // BigInt64Array
        slow,       // BigUint64Array
        slow,       // Float16Array
        ta_f32,     // Float32Array
        ta_f64,     // Float64Array
    ] {
        entries.push(b.func.dfg.block_call(block, &[]));
    }
    let default = b.func.dfg.block_call(slow, &[]);
    let table = b.create_jump_table(JumpTableData::new(default, &entries));
    b.ins().br_table(kind, table);

    // The integer widths: store the value's low bits, truncated to the width
    // (the engine's `val as u8` / `as i16` / … ✓). The stored type decides
    // the store's width.
    let int_store = |b: &mut FunctionBuilder, width: i64, bits: Value| {
        let scaled = b.ins().imul_imm(index, width);
        let addr = b.ins().iadd(base, scaled);
        let value = match width {
            1 => b.ins().ireduce(types::I8, bits),
            2 => b.ins().ireduce(types::I16, bits),
            _ => bits,
        };
        b.ins().store(MemFlags::new(), value, addr, 0);
    };
    // An integer width takes any number: an `Int` stores its low bits as they
    // are, a `Float64` is first converted with `ToInt32`.
    let int_arm = |b: &mut FunctionBuilder, width: i64| {
        let other = b.create_block();
        let store = b.create_block();
        b.ins().brif(int_value, store, &[], other, &[]);
        b.switch_to_block(store);
        int_store(b, width, int32);
        b.ins().jump(ta_done, &[]);
        b.switch_to_block(other);
        let ok = b.create_block();
        b.ins().brif(float_value, ok, &[], slow, &[]);
        b.switch_to_block(ok);
        let float = unbox_float64(b, value);
        let converted = to_int32_from_float64(b, float);
        int_store(b, width, converted);
        b.ins().jump(ta_done, &[]);
    };
    b.switch_to_block(ta_int8);
    int_arm(b, 1);
    b.switch_to_block(ta_int16);
    int_arm(b, 2);
    b.switch_to_block(ta_int32);
    int_arm(b, 4);

    // `Uint8ClampedArray`: the engine's `to_u8_clamped` — round to nearest,
    // ties to even, then clamp to [0, 255]. For a `Float64` the rounding is
    // the 2^52 add/sub trick (every value below 2^52 rounds to an integer
    // there; larger ones are integral anyway and clamp to an end).
    b.switch_to_block(ta_clamped);
    let convert = b.create_block();
    let store = b.create_block();
    b.ins().brif(int_value, store, &[], convert, &[]);
    b.switch_to_block(convert);
    let ok_clamped = b.create_block();
    b.ins().brif(float_value, ok_clamped, &[], slow, &[]);
    b.switch_to_block(ok_clamped);
    let float = unbox_float64(b, value);
    let magic = b.ins().f64const(4503599627370496.0);
    let shifted = b.ins().fadd(float, magic);
    let rounded = b.ins().fsub(shifted, magic);
    let converted = b.ins().fcvt_to_sint_sat(types::I32, rounded);
    let clamp = b.create_block();
    b.append_block_param(clamp, types::I32);
    b.ins().jump(clamp, &[converted]);
    b.switch_to_block(store);
    b.ins().jump(clamp, &[int32]);
    b.switch_to_block(clamp);
    let bits = b.block_params(clamp)[0];
    let zero = b.ins().iconst(types::I32, 0);
    let low = b.ins().smax(bits, zero);
    let max = b.ins().iconst(types::I32, 255);
    let high = b.ins().smin(low, max);
    let byte = b.ins().ireduce(types::I8, high);
    let scaled = b.ins().imul_imm(index, 1);
    let addr = b.ins().iadd(base, scaled);
    b.ins().store(MemFlags::new(), byte, addr, 0);
    b.ins().jump(ta_done, &[]);

    // The float widths: an `Int` is converted (`ToNumber`), a `Float64` is
    // unboxed.
    let float_arm = |b: &mut FunctionBuilder, width: i64, narrow: bool| {
        let store = |b: &mut FunctionBuilder, wide: Value| {
            let stored = if narrow {
                b.ins().fdemote(types::F32, wide)
            } else {
                wide
            };
            let scaled = b.ins().imul_imm(index, width);
            let addr = b.ins().iadd(base, scaled);
            b.ins().store(MemFlags::new(), stored, addr, 0);
        };
        let other = b.create_block();
        let float = b.create_block();
        b.ins().brif(float_value, float, &[], other, &[]);
        b.switch_to_block(float);
        let wide = unbox_float64(b, value);
        store(b, wide);
        b.ins().jump(ta_done, &[]);
        b.switch_to_block(other);
        let ok = b.create_block();
        b.ins().brif(int_value, ok, &[], slow, &[]);
        b.switch_to_block(ok);
        let wide = int_to_float64(b, value);
        store(b, wide);
        b.ins().jump(ta_done, &[]);
    };
    b.switch_to_block(ta_f32);
    // A `Float32Array` keeps the double's rounded value (the engine's
    // `val as f32` ✓).
    float_arm(b, 4, true);
    b.switch_to_block(ta_f64);
    float_arm(b, 8, false);

    b.switch_to_block(ta_done);
    // The three operands are gone; the stack pointer drops with them.
    let sp_next = b.ins().iadd_imm(sp, -24);
    b.ins().jump(cont, &[sp_next]);

    b.switch_to_block(plain_array_arm);
    // The engine's fast write into a plain array only runs on an extensible
    // one (a frozen or sealed array goes through the slow path, which throws
    // in strict mode). The guard asks for an extensible `arguments` object
    // too: a stricter guard only means the helper answers it, with the same
    // result.
    let ext_bit = b.ins().ushr_imm(flags, i64::from(WRITE_EXTENSIBLE_SHIFT));
    let is_extensible = b.ins().icmp_imm(IntCC::NotEqual, ext_bit, 0);
    let check = b.create_block();
    b.ins().brif(is_extensible, check, &[], slow, &[]);

    b.switch_to_block(check);
    // The payload is only read once the class says it is an array: a plain
    // object has no payload, and reading it would be a wild load.
    let payload = b
        .ins()
        .load(types::I64, MemFlags::new(), obj, jit_layout::PAYLOAD_OFFSET);
    let data = b.ins().load(
        types::I64,
        MemFlags::new(),
        payload,
        array_layout::DATA_OFFSET,
    );
    let len = b.ins().load(
        types::I32,
        MemFlags::new(),
        payload,
        array_layout::LEN_OFFSET,
    );
    // The index: an `Int` payload is the low 32 bits. A negative index
    // becomes a huge unsigned value and fails the bounds check.
    let index = b.ins().band_imm(prop, 0xffff_ffff);
    let len64 = b.ins().uextend(types::I64, len);
    let in_bounds = b.ins().icmp(IntCC::UnsignedLessThan, index, len64);
    // A fast array always has a payload, but the engine checks it before the
    // write, so the guard does too: a null payload would be a wild load.
    let has_payload = b.ins().icmp_imm(IntCC::NotEqual, payload, 0);
    let store_ok = b.ins().band(has_payload, in_bounds);
    let store = b.create_block();
    b.ins().brif(store_ok, store, &[], slow, &[]);

    b.switch_to_block(store);
    // The element's address: `data + index * 8`.
    let offset = b.ins().imul_imm(index, 8);
    let addr = b.ins().iadd(data, offset);
    b.ins().store(MemFlags::new(), value, addr, 0);
    // The three operands are gone; the stack pointer drops with them.
    let sp_next = b.ins().iadd_imm(sp, -24);
    let bind = b.create_block();
    emit_write_barrier(b, state, fv, obj, value, bind, cont, &[sp_next]);

    b.switch_to_block(slow);
    // The site index rides in `imm2` (array element keys are runtime values,
    // so the cached property atom lives in the cache entry).
    #[cfg(feature = "inline_cache")]
    {
        let ic = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
        b.ins().store(MemFlags::new(), ic, fv.frame, O_IMM2);
    }
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_PUT_ARRAY_EL, pc_after);
    b.ins().jump(cont, &[sp_slow]);

    b.switch_to_block(cont);
    // The three operands are gone, and nothing replaces them.
    state.stack.consumed_ternary();
    let sp_next = b.block_params(cont)[0];
    b.def_var(sp_var, sp_next);
    Step::Continue(b.use_var(sp_var))
}

/// The property inline guard (the property half): the inline fast path for `get_field` — the read
/// of an own **data** property.
///
/// The guard checks, in order: the target's tag (`Object`), the object's shape
/// against the shape the inline-cache site recorded (the site's first way
/// holds that shape, its id and the slot the property was in — the id is the
/// second half of the cache key and tells a shape from another one that took
/// its place), the slot's range, and the slot's kind (a plain data property —
/// not an accessor, a variable reference, a lazy initializer or a tombstone).
/// Anything else falls back to the helper, which owns the full semantics (the
/// prototype chain, getters, string and number targets, …).
///
/// Only the site's **first** way is read: it is the monomorphic case, and a
/// miss must stay cheap (the helper call it guards against is only a few
/// dozen cycles). A site that sees several shapes is served by the helper.
///
/// Everything is read fresh on every access, so a shape change (the object
/// grows a property, the property becomes an accessor, the object is
/// replaced) is a **guard miss**, not a stale read — no invalidation channel
/// is needed.
///
/// The reads go through the layout contracts in [`super::layout`]: the
/// object's shape pointer and its property slots, the shape's id, and the
/// inline-cache site's first way.
#[cfg(feature = "inline_cache")]
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_get_field_fast(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp_var: Variable,
    sp: Value,
    insn: &Instruction,
    shape: FieldRead,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let top = state.stack.height;
    let target = state.stack.read(b, sp, top);

    // The check comes before any load, so a non-object value is never
    // dereferenced — and it is the *only* check a miss pays for (everything
    // else lives in the fast path, so a site that never hits costs one compare
    // and a branch).
    let target_is_object = is_object(b, target);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // The target stays in a register on the fast path; the helper's own
    // preamble flushes it on the slow path.
    state.stack.flush_except(b, sp, &[top]);
    b.ins().brif(target_is_object, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    // `get_field2` keeps the target below the pushed value, but `cont` drops
    // the target's register copy; memory must hold it by then, or the next
    // flush (the call helper after `get_field2` in a method call) would write
    // every other slot around a stale one. `Replace` writes the value into the
    // target's own slot instead, so it needs no store.
    if shape == FieldRead::Keep {
        b.ins().store(MemFlags::new(), target, sp, -8);
    }
    let (site, way_shape, way_uid, way_slot) = load_ic_way(b, fv, insn);
    let (obj, shape_node, same_uid) = shape_guard(b, target, way_shape, way_uid, slow);

    // ── The typed array's `length` ────────────────────────────────────
    //
    // The property lives on `%TypedArray%.prototype`, so the ordinary slot
    // cache cannot serve it. The entry holds the instance's shape (an own
    // `length` changes it) and the id of the shape that owns `length` in the
    // chain (a patched prototype changes that one) — the guard re-checks both
    // on every access, so a change is a miss, never a stale read.
    //
    // Only a `length` read can hit it, and the atom is known at compile time,
    // so the extra loads and the kind check exist only there.
    let ordinary = if insn.operand_u32(1) == crate::atom::Atom::CONST_length.id() {
        // The entry's kind: an ordinary one (an own property, cached by its
        // slot) or a typed array's `length` (the property lives on the
        // prototype, so the entry caches the shape that owns it instead).
        let ordinary = b.create_block();
        let way_flags = b
            .ins()
            .load(types::I8, MemFlags::new(), site, ic_layout::FLAGS_OFFSET);
        let is_ta_length = {
            let kind_bit = b.ins().band_imm(
                way_flags,
                i64::from(crate::function::IC_FLAG_TYPED_ARRAY_LENGTH),
            );
            b.ins().icmp_imm(IntCC::NotEqual, kind_bit, 0)
        };
        // The target must really be a typed array: a plain object can be given
        // the typed array's prototype, and its payload is not a view.
        let flags = b
            .ins()
            .load(types::I32, MemFlags::new(), obj, jit_layout::FLAGS_OFFSET);
        let class_id = b.ins().band_imm(flags, jit_layout::CLASS_ID_MASK);
        let is_ta_class = {
            let lower = b.ins().icmp_imm(
                IntCC::SignedGreaterThanOrEqual,
                class_id,
                i64::from(JS_CLASS_UINT8C_ARRAY),
            );
            let upper = b.ins().icmp_imm(
                IntCC::SignedLessThanOrEqual,
                class_id,
                i64::from(JS_CLASS_FLOAT64_ARRAY),
            );
            b.ins().band(lower, upper)
        };
        // The shape's id has to match too: the recorded pointer alone could be
        // a shape that was freed and whose memory another shape took.
        let ta_ok = {
            let both = b.ins().band(is_ta_length, is_ta_class);
            b.ins().band(both, same_uid)
        };
        let kind_block = b.create_block();
        b.ins().brif(ta_ok, kind_block, &[], ordinary, &[]);

        b.switch_to_block(kind_block);
        // The id of the shape that owns `length` in the chain.
        let way_owner = b
            .ins()
            .load(types::I32, MemFlags::new(), site, ic_layout::PROP_OFFSET);
        // The chain: the instance's shape's prototype (the typed array's own
        // prototype) and its prototype's prototype (`%TypedArray%.prototype`),
        // which owns the `length` getter.
        let proto1 = b.ins().load(
            types::I64,
            MemFlags::new(),
            shape_node,
            jit_layout::SHAPE_PROTO_OFFSET,
        );
        let proto1_shape = b.ins().load(
            types::I64,
            MemFlags::new(),
            proto1,
            jit_layout::SHAPE_OFFSET,
        );
        let proto2 = b.ins().load(
            types::I64,
            MemFlags::new(),
            proto1_shape,
            jit_layout::SHAPE_PROTO_OFFSET,
        );
        let proto2_shape = b.ins().load(
            types::I64,
            MemFlags::new(),
            proto2,
            jit_layout::SHAPE_OFFSET,
        );
        let owner_uid = b.ins().load(
            types::I32,
            MemFlags::new(),
            proto2_shape,
            jit_layout::SHAPE_UID_OFFSET,
        );
        let same_owner = b.ins().icmp(IntCC::Equal, way_owner, owner_uid);
        let chain_ok = b.create_block();
        b.ins().brif(same_owner, chain_ok, &[], slow, &[]);

        b.switch_to_block(chain_ok);
        // The view's element count, and the "out of bounds" flag: a view whose
        // resizable buffer was shrunk past its end reports a length of 0 (the
        // same rule the engine's getter uses).
        let payload = b
            .ins()
            .load(types::I64, MemFlags::new(), obj, jit_layout::PAYLOAD_OFFSET);
        let count = b.ins().load(
            types::I32,
            MemFlags::new(),
            payload,
            ta_layout::COUNT_OFFSET,
        );
        let out_of_bounds = b.ins().load(
            types::I8,
            MemFlags::new(),
            payload,
            ta_layout::OUT_OF_BOUNDS_OFFSET,
        );
        let is_oob = b.ins().icmp_imm(IntCC::NotEqual, out_of_bounds, 0);
        let zero = b.ins().iconst(types::I32, 0);
        let len = b.ins().select(is_oob, zero, count);
        // The value is an `Int`: its payload is the count in the low 32 bits.
        let value = b.ins().uextend(types::I64, len);
        b.ins()
            .store(MemFlags::new(), value, sp, shape.result_offset());
        // Property-guard instrumentation: the guard answered this read on its own.
        bump_counter(b, fv, O_PROPERTY_HITS);
        let sp_next = shape.sp_next(b, sp);
        b.def_var(sp_var, sp_next);
        b.ins().jump(cont, &[]);
        Some(ordinary)
    } else {
        None
    };

    // ── The ordinary own data property ─────────────────────────────────
    if let Some(ordinary) = ordinary {
        b.switch_to_block(ordinary);
    }
    // The slot the way recorded must be inside the shape's slots: slots are
    // index-aligned with the shape's props, and the shape matched above.
    let slot = b.ins().uextend(types::I64, way_slot);
    let count = b.ins().load(
        types::I32,
        MemFlags::new(),
        shape_node,
        jit_layout::SHAPE_PROP_COUNT_OFFSET,
    );
    let count64 = b.ins().uextend(types::I64, count);
    let in_range = b.ins().icmp(IntCC::UnsignedLessThan, slot, count64);
    // Both must hold before the slot is read: the shape says *which* slot, the
    // range says the slot is there at all.
    let ok = b.ins().band(same_uid, in_range);
    let value_ok = b.create_block();
    b.ins().brif(ok, value_ok, &[], slow, &[]);

    b.switch_to_block(value_ok);
    // The property slots themselves.
    let slots = b.ins().load(
        types::I64,
        MemFlags::new(),
        obj,
        jit_layout::PROP_DATA_OFFSET,
    );
    let (addr, is_data) = data_property_slot(b, slots, slot);
    let read_ok = b.create_block();
    b.ins().brif(is_data, read_ok, &[], slow, &[]);

    b.switch_to_block(read_ok);
    let value = b.ins().load(
        types::I64,
        MemFlags::new(),
        addr,
        JSProperty::normal_value_offset(),
    );
    // The result takes the target's place (`get_field`) or is pushed next to
    // it (`get_field2`, the method-call shape).
    b.ins()
        .store(MemFlags::new(), value, sp, shape.result_offset());
    // Property-guard instrumentation: the guard answered this read on its own.
    bump_counter(b, fv, O_PROPERTY_HITS);
    // The stack height does not change, but the variable must be defined on
    // *both* paths: the merge block takes it from there, and a path that left
    // it undefined would hand the merge a stale pointer.
    let sp_next = shape.sp_next(b, sp);
    b.def_var(sp_var, sp_next);
    b.ins().jump(cont, &[]);

    b.switch_to_block(slow);
    // Property-guard instrumentation: the guard handed this read back to the helper.
    bump_counter(b, fv, O_PROPERTY_MISSES);
    // The helper does the full lookup: the atom rides in `imm`, the site
    // index in `imm2`.
    let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
    b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
    let ic = b.ins().iconst(types::I32, i64::from(ic_index(insn)));
    b.ins().store(MemFlags::new(), ic, fv.frame, O_IMM2);
    let helper = match shape {
        FieldRead::Replace => HELPER_GET_FIELD,
        FieldRead::Keep => HELPER_GET_FIELD2,
    };
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper, pc_after);
    b.def_var(sp_var, sp_slow);
    b.ins().jump(cont, &[]);

    b.switch_to_block(cont);
    // The slot now holds the property's value, not the target: the register
    // copy of the target is stale.
    state.stack.replace_top();
    match shape {
        FieldRead::Replace => {}
        // The value was pushed next to the target: one slot more.
        FieldRead::Keep => state.stack.adjust(1),
    }
    Step::Continue(b.use_var(sp_var))
}

/// The property inline guard (the write half): the inline fast path for `put_field` — the write
/// of an own **data** property.
///
/// The guard is the property read guard plus the two things a write needs: the
/// recorded property must be the one this site writes (the site's way holds
/// the atom it saw), and it must be **writable** — the engine's own
/// inline-cache write path checks exactly that (`JS_PROP_NORMAL`,
/// `JS_PROP_WRITABLE`, and not `JS_PROP_LENGTH`, because writing `length`
/// resizes the array and stays with the helper).
///
/// A stored value that references a GC node needs the collector's write
/// barrier (a black object pointing at a white value would let the collector
/// sweep the value away): the same leaf helper the array write uses.
#[cfg(feature = "inline_cache")]
pub(super) fn lower_put_field_fast(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp_var: Variable,
    sp: Value,
    insn: &Instruction,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let top = state.stack.height;
    // `put_field`: [target, value, ^] -> [^].
    let target = state.stack.read(b, sp, top - 1);
    let value = state.stack.read(b, sp, top);

    // The check comes before any load, so a non-object value is never
    // dereferenced.
    let target_is_object = is_object(b, target);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // The operands stay in registers on the fast path; the helper's own
    // preamble flushes them on the slow path.
    state.stack.flush_except(b, sp, &[top - 1, top]);
    b.ins().brif(target_is_object, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    let (site, way_shape, way_uid, way_slot) = load_ic_way(b, fv, insn);
    let (way_prop, way_flags) = load_ic_property(b, site);
    let (obj, shape_node, same_uid) = shape_guard(b, target, way_shape, way_uid, slow);
    // The recorded property must be the one this site writes.
    let atom_id = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
    let same_prop = b.ins().icmp(IntCC::Equal, way_prop, atom_id);
    // The recorded flags: a plain data property (`JS_PROP_NORMAL`), writable,
    // and not `length` (writing `length` resizes the array).
    let kind_bits = b.ins().band_imm(way_flags, i64::from(JS_PROP_TMASK));
    let is_data = b
        .ins()
        .icmp_imm(IntCC::Equal, kind_bits, i64::from(JS_PROP_NORMAL));
    let writable_bit = b.ins().band_imm(way_flags, i64::from(JS_PROP_WRITABLE));
    let is_writable = b.ins().icmp_imm(IntCC::NotEqual, writable_bit, 0);
    let length_bit = b.ins().band_imm(way_flags, i64::from(JS_PROP_LENGTH));
    let not_length = b.ins().icmp_imm(IntCC::Equal, length_bit, 0);
    let writable_ok = b.ins().band(is_writable, not_length);
    let flags_ok = b.ins().band(is_data, writable_ok);

    // The slot the way recorded must be inside the shape's slots (slots are
    // index-aligned with the shape's props, and the shape matched above).
    let slot = b.ins().uextend(types::I64, way_slot);
    let count = b.ins().load(
        types::I32,
        MemFlags::new(),
        shape_node,
        jit_layout::SHAPE_PROP_COUNT_OFFSET,
    );
    let count64 = b.ins().uextend(types::I64, count);
    let in_range = b.ins().icmp(IntCC::UnsignedLessThan, slot, count64);
    // Every part must hold before the slot is written.
    let ok = b.ins().band(same_uid, same_prop);
    let ok = b.ins().band(ok, flags_ok);
    let ok = b.ins().band(ok, in_range);
    let write_ok = b.create_block();
    b.ins().brif(ok, write_ok, &[], slow, &[]);

    b.switch_to_block(write_ok);
    // The property slots themselves.
    let slots = b.ins().load(
        types::I64,
        MemFlags::new(),
        obj,
        jit_layout::PROP_DATA_OFFSET,
    );
    let (addr, is_normal) = data_property_slot(b, slots, slot);
    let store_ok = b.create_block();
    b.ins().brif(is_normal, store_ok, &[], slow, &[]);

    b.switch_to_block(store_ok);
    // The value goes into the slot's `Normal` payload; the tag stays.
    b.ins().store(
        MemFlags::new(),
        value,
        addr,
        JSProperty::normal_value_offset(),
    );
    // Property-guard instrumentation: the guard answered this write on its own.
    bump_counter(b, fv, O_PROPERTY_HITS);
    let bind = b.create_block();
    emit_write_barrier(b, state, fv, obj, value, bind, cont, &[]);

    b.switch_to_block(slow);
    // Property-guard instrumentation: the guard handed this write back to the helper.
    bump_counter(b, fv, O_PROPERTY_MISSES);
    // The helper does the full lookup: the atom rides in `imm`, the site
    // index in `imm2`.
    let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
    b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
    let ic = b.ins().iconst(types::I32, i64::from(ic_index(insn)));
    b.ins().store(MemFlags::new(), ic, fv.frame, O_IMM2);
    // The helper popped the two operands, exactly like the fast path.
    let _ = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_PUT_FIELD, pc_after);
    b.ins().jump(cont, &[]);

    b.switch_to_block(cont);
    // The two operands are gone, and nothing replaces them.
    state.stack.consumed_pair();
    let sp_next = b.ins().iadd_imm(sp, -16);
    b.def_var(sp_var, sp_next);
    Step::Continue(b.use_var(sp_var))
}

/// The global-variable guard's shared prologue.
///
/// Emits the site's cache loads and the checks that must pass before the slot
/// may be touched: the entry is filled, the owning object's shape and id
/// match, `global_var_obj`'s shape and id match (a lexical binding shadows a
/// global-object property), the cached slot is in range, and the slot still
/// holds a plain data property. On success it returns with the builder in the
/// slot block; every miss branches to `slow`.
///
/// Everything is re-read on every access — a shape write, a `delete`, an
/// `eval` that adds a binding or a flag change is a miss, never a stale read.
struct GlobalGuard {
    /// The owning object (a node pointer): `global_var_obj` for a lexical
    /// binding, `global_obj` otherwise.
    owner: Value,
    /// The address of the property slot.
    slot_addr: Value,
    /// The cached property flags.
    flags: Value,
    /// The cache entry's kind ([`GLOBAL_CACHE_LEXICAL`]/[`GLOBAL_CACHE_GLOBAL`]).
    kind: Value,
    slow: Block,
    cont: Block,
}

fn emit_global_guard(b: &mut FunctionBuilder, fv: &FrameVals, insn: &Instruction) -> GlobalGuard {
    let idx = i64::from(insn.operand_u16(1));
    let site = b
        .ins()
        .iadd_imm(fv.global_cache, idx * gv_layout::ENTRY_SIZE);
    let kind = b
        .ins()
        .load(types::I8, MemFlags::new(), site, gv_layout::KIND_OFFSET);
    let owner_shape = b.ins().load(
        types::I64,
        MemFlags::new(),
        site,
        gv_layout::OWNER_SHAPE_OFFSET,
    );
    let lexical_shape = b.ins().load(
        types::I64,
        MemFlags::new(),
        site,
        gv_layout::LEXICAL_SHAPE_OFFSET,
    );
    let owner_uid = b.ins().load(
        types::I32,
        MemFlags::new(),
        site,
        gv_layout::OWNER_UID_OFFSET,
    );
    let lexical_uid = b.ins().load(
        types::I32,
        MemFlags::new(),
        site,
        gv_layout::LEXICAL_UID_OFFSET,
    );
    let slot = b
        .ins()
        .load(types::I32, MemFlags::new(), site, gv_layout::SLOT_OFFSET);
    let flags = b
        .ins()
        .load(types::I8, MemFlags::new(), site, gv_layout::FLAGS_OFFSET);

    // The two global objects, as tagged values; the low 48 bits are the node
    // pointer the loads below use.
    let gobj_value = fv.global_obj;
    let gv_value = fv.global_var_obj;
    let node_mask = 0x0000_ffff_ffff_ffff_i64;
    let gv = b.ins().band_imm(gv_value, node_mask);
    let is_lexical = b
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(GLOBAL_CACHE_LEXICAL));
    let owner_value = b.ins().select(is_lexical, gv_value, gobj_value);
    let owner = b.ins().band_imm(owner_value, node_mask);

    // The owning object's shape: the object holds the shape's *node* pointer,
    // the cache records the *payload* pointer.
    let owner_shape_node =
        b.ins()
            .load(types::I64, MemFlags::new(), owner, jit_layout::SHAPE_OFFSET);
    let owner_shape_payload = b.ins().iadd_imm(
        owner_shape_node,
        i64::from(jit_layout::SHAPE_PAYLOAD_OFFSET),
    );
    let same_owner = b.ins().icmp(IntCC::Equal, owner_shape, owner_shape_payload);

    // `global_var_obj` shadows the global object: a global-object entry also
    // proves the shadowing object still looks the way it did.
    let gv_shape_node = b
        .ins()
        .load(types::I64, MemFlags::new(), gv, jit_layout::SHAPE_OFFSET);
    let gv_shape_payload = b
        .ins()
        .iadd_imm(gv_shape_node, i64::from(jit_layout::SHAPE_PAYLOAD_OFFSET));
    let same_lexical = b.ins().icmp(IntCC::Equal, lexical_shape, gv_shape_payload);

    let used = b.ins().icmp_imm(IntCC::NotEqual, kind, 0);
    let pointers_ok = b.ins().band(used, same_owner);
    let pointers_ok = b.ins().band(pointers_ok, same_lexical);

    let slow = b.create_block();
    let shapes = b.create_block();
    // The ids are loaded only after the pointers match: an object without a
    // shape has a null shape pointer, and reading its "id" would be a wild
    // load. A matched pointer guarantees the node is a live shape (the object
    // holds it), so the load below is safe.
    b.ins().brif(pointers_ok, shapes, &[], slow, &[]);

    b.switch_to_block(shapes);
    let live_owner_uid = b.ins().load(
        types::I32,
        MemFlags::new(),
        owner_shape_node,
        jit_layout::SHAPE_UID_OFFSET,
    );
    let live_lexical_uid = b.ins().load(
        types::I32,
        MemFlags::new(),
        gv_shape_node,
        jit_layout::SHAPE_UID_OFFSET,
    );
    let same_owner_uid = b.ins().icmp(IntCC::Equal, owner_uid, live_owner_uid);
    let same_lexical_uid = b.ins().icmp(IntCC::Equal, lexical_uid, live_lexical_uid);
    let ids_ok = b.ins().band(same_owner_uid, same_lexical_uid);

    let matched = b.create_block();
    b.ins().brif(ids_ok, matched, &[], slow, &[]);

    b.switch_to_block(matched);
    // The slot the cache recorded must still be there (slots are index-
    // aligned with the owner shape's props, which matched above), and must
    // still be a plain data property (`delete` leaves a tombstone; an
    // accessor, a variable reference and a lazy initializer have their own
    // tags).
    let slot64 = b.ins().uextend(types::I64, slot);
    let count = b.ins().load(
        types::I32,
        MemFlags::new(),
        owner_shape_node,
        jit_layout::SHAPE_PROP_COUNT_OFFSET,
    );
    let count64 = b.ins().uextend(types::I64, count);
    let in_range = b.ins().icmp(IntCC::UnsignedLessThan, slot64, count64);
    let slots = b.ins().load(
        types::I64,
        MemFlags::new(),
        owner,
        jit_layout::PROP_DATA_OFFSET,
    );
    let (addr, is_data) = data_property_slot(b, slots, slot64);
    let slot_ok = b.ins().band(in_range, is_data);
    let value_ok = b.create_block();
    b.ins().brif(slot_ok, value_ok, &[], slow, &[]);

    b.switch_to_block(value_ok);
    GlobalGuard {
        owner,
        slot_addr: addr,
        flags,
        kind,
        slow,
        cont: b.create_block(),
    }
}

/// `get_var` / `get_var_undef`: read an own data property of one of the
/// two global objects inline, behind [`emit_global_guard`]. A TDZ slot, an
/// unbound name and everything the guard declines take the helper, which owns
/// the full semantics (errors included).
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_get_var_fast(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp_var: Variable,
    sp: Value,
    insn: &Instruction,
    throw: bool,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    // Every register-resident stack slot must be in the frame before the
    // branch: the helper path flushes, and the merge must not read a stale
    // slot the fast path never wrote.
    state.stack.flush(b, sp);
    let guard = emit_global_guard(b, fv, insn);

    let value = b.ins().load(
        types::I64,
        MemFlags::new(),
        guard.slot_addr,
        JSProperty::normal_value_offset(),
    );
    // A TDZ slot (`let`/`const` before its initializer) is not a value: the
    // helper raises the ReferenceError.
    let uninitialized = b.ins().icmp_imm(IntCC::Equal, value, UNINITIALIZED_BITS);
    let push = b.create_block();
    b.ins().brif(uninitialized, guard.slow, &[], push, &[]);

    b.switch_to_block(push);
    b.ins().store(MemFlags::new(), value, sp, 0);
    let sp_next = b.ins().iadd_imm(sp, 8);
    b.def_var(sp_var, sp_next);
    bump_counter(b, fv, O_GLOBAL_GUARD_HITS);
    b.ins().jump(guard.cont, &[]);

    b.switch_to_block(guard.slow);
    bump_counter(b, fv, O_GLOBAL_GUARD_MISSES);
    let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
    b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM);
    let helper = if throw {
        HELPER_GET_VAR
    } else {
        HELPER_GET_VAR_UNDEF
    };
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper, pc_after);
    b.def_var(sp_var, sp_slow);
    b.ins().jump(guard.cont, &[]);

    b.switch_to_block(guard.cont);
    state.stack.adjust(1);
    Step::Continue(b.use_var(sp_var))
}

/// `put_var` / `put_var_init`: write an own **writable** data property of
/// one of the two global objects inline, behind [`emit_global_guard`].
///
/// A lexical binding's `init` skips the TDZ and writability checks exactly like
/// `set_global_var`'s `Init` mode; every other shape (an unbound name, a
/// read-only property, an accessor, a strict-mode add) takes the helper.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_put_var_fast(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp_var: Variable,
    sp: Value,
    insn: &Instruction,
    init: bool,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let top = state.stack.height;
    state.stack.flush(b, sp);
    let value = state.stack.read(b, sp, top);
    let guard = emit_global_guard(b, fv, insn);

    let writable_bit = b.ins().band_imm(guard.flags, i64::from(JS_PROP_WRITABLE));
    let writable = b.ins().icmp_imm(IntCC::NotEqual, writable_bit, 0);
    // `put_var_init` initializes a lexical binding: the slot may still be
    // uninitialized and the binding may be a `const`.
    let rights = if init {
        let lexical = b
            .ins()
            .icmp_imm(IntCC::Equal, guard.kind, i64::from(GLOBAL_CACHE_LEXICAL));
        b.ins().bor(lexical, writable)
    } else {
        writable
    };
    // Writing `length` resizes an array; the global object is not one, but the
    // cached flags are checked anyway so the guard never implies a fast path
    // the engine's own write would not take.
    let length_bit = b.ins().band_imm(guard.flags, i64::from(JS_PROP_LENGTH));
    let not_length = b.ins().icmp_imm(IntCC::Equal, length_bit, 0);
    let current = b.ins().load(
        types::I64,
        MemFlags::new(),
        guard.slot_addr,
        JSProperty::normal_value_offset(),
    );
    let uninitialized = b.ins().icmp_imm(IntCC::Equal, current, UNINITIALIZED_BITS);
    let tdz_ok = if init {
        b.ins().iconst(types::I8, 1)
    } else {
        b.ins().icmp_imm(IntCC::Equal, uninitialized, 0)
    };
    let ok = b.ins().band(rights, not_length);
    let ok = b.ins().band(ok, tdz_ok);
    let store = b.create_block();
    b.ins().brif(ok, store, &[], guard.slow, &[]);

    b.switch_to_block(store);
    b.ins().store(
        MemFlags::new(),
        value,
        guard.slot_addr,
        JSProperty::normal_value_offset(),
    );
    bump_counter(b, fv, O_GLOBAL_GUARD_HITS);
    let sp_next = b.ins().iadd_imm(sp, -8);
    b.def_var(sp_var, sp_next);
    let bind = b.create_block();
    emit_write_barrier(b, state, fv, guard.owner, value, bind, guard.cont, &[]);

    b.switch_to_block(guard.slow);
    bump_counter(b, fv, O_GLOBAL_GUARD_MISSES);
    let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
    b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM);
    let helper = if init {
        HELPER_PUT_VAR_INIT
    } else {
        HELPER_PUT_VAR
    };
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper, pc_after);
    b.def_var(sp_var, sp_slow);
    b.ins().jump(guard.cont, &[]);

    b.switch_to_block(guard.cont);
    state.stack.drop_top();
    Step::Continue(b.use_var(sp_var))
}
