// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::{Atom, AtomKind},
    bytecode::{OpCode, OpCodeTemp, opcodes},
    function::{FunctionDef, FunctionKind},
    value::JSValue,
};

impl FunctionDef {
    #[inline(always)]
    pub fn emit_u8(&mut self, val: u8) {
        push_vec_u8(&mut self.byte_code, val);
    }

    #[inline(always)]
    pub fn emit_u16(&mut self, val: u16) {
        push_vec_u16(&mut self.byte_code, val);
    }

    #[inline(always)]
    pub fn emit_u32(&mut self, val: u32) {
        push_vec_u32(&mut self.byte_code, val);
    }

    /// emit an atom.
    /// Note: `atom` will be duplicated inside
    #[inline(always)]
    pub fn emit_atom(&mut self, atom: Atom) {
        self.emit_u32(atom.id());
    }

    pub(crate) fn emit_op_u8(&mut self, op: u8, last_line_num: i32, last_line_pos: u32) {
        debug_assert!(last_line_num >= 0);

        let bc = &mut self.byte_code;

        // Use the line number of the last token used, not the next token,
        // nor the current offset in the source file.
        // Emit LINE_NUM when either line or column changes.
        if self.last_opcode_line_num != last_line_num || self.last_opcode_line_pos != last_line_pos
        {
            push_vec_u8(bc, OpCodeTemp::LINE_NUM as u8);
            push_vec_u32(bc, last_line_num as _);
            push_vec_u32(bc, last_line_pos);
            self.last_opcode_line_num = last_line_num;
            self.last_opcode_line_pos = last_line_pos;
        }

        self.last_opcode_pos = bc.len() as i32;
        bc.push(op);
    }

    #[inline(always)]
    pub fn emit_op(&mut self, op: OpCode, last_line_num: i32, last_line_pos: u32) {
        self.emit_op_u8(op as u8, last_line_num, last_line_pos);
    }
    #[inline(always)]
    pub fn emit_op_temp(&mut self, op: OpCodeTemp, last_line_num: i32, last_line_pos: u32) {
        self.emit_op_u8(op as u8, last_line_num, last_line_pos);
    }

    pub fn emit_get_field(&mut self, atom: Atom, last_line_num: i32, last_line_pos: u32) {
        self.emit_op(OpCode::GET_FIELD, last_line_num, last_line_pos);
        self.emit_atom(atom);
        #[cfg(feature = "inline_cache")]
        {
            self.emit_u16(self.inline_cache_count);
            self.inline_cache_count += 1;
        }
    }

    /// Emit a named-property-get that preserves the object on the stack
    /// (GET_FIELD2_IC or plain GET_FIELD2).
    pub fn emit_get_field2(&mut self, atom: Atom, last_line_num: i32, last_line_pos: u32) {
        self.emit_op(OpCode::GET_FIELD2, last_line_num, last_line_pos);
        self.emit_atom(atom);
        #[cfg(feature = "inline_cache")]
        {
            self.emit_u16(self.inline_cache_count);
            self.inline_cache_count += 1;
        }
    }

    /// Emit a named-property-put — either PUT_FIELD_IC (with inline_cache
    /// feature) or plain PUT_FIELD.
    pub fn emit_put_field(&mut self, atom: Atom, last_line_num: i32, last_line_pos: u32) {
        self.emit_op(OpCode::PUT_FIELD, last_line_num, last_line_pos);
        self.emit_atom(atom);
        #[cfg(feature = "inline_cache")]
        {
            self.emit_u16(self.inline_cache_count);
            self.inline_cache_count += 1;
        }
    }

    pub fn emit_return(&mut self, mut hasval: bool, last_line_num: i32, last_line_pos: u32) {
        // execute the finally blocks before return
        if self.attrs.func_kind != FunctionKind::Normal {
            if !hasval {
                // no value: direct return in case of async generator
                self.emit_op(OpCode::UNDEFINED, last_line_num, last_line_pos);
                hasval = true;
            } else if self.attrs.func_kind == FunctionKind::AsyncGenerator {
                // the await must be done before handling the "finally" in
                // case it raises an exception
                self.emit_op(OpCode::AWAIT, last_line_num, last_line_pos);
            }
        }

        let mut top = self.top_break;
        while !top.is_null() {
            if unsafe { (*top).has_iterator || (*top).label_finally != -1 } {
                if !hasval {
                    self.emit_op(OpCode::UNDEFINED, last_line_num, last_line_pos);
                    hasval = true;
                }
                // Remove the stack elements up to and including the catch
                // offset. When 'yield' is used in an expression we have
                // no easy way to count them, so we use this specific
                // instruction instead.
                self.emit_op(OpCode::NIP_CATCH, last_line_num, last_line_pos);

                // stack: iter_obj next ret_val
                if unsafe { (&*top).has_iterator } {
                    if self.attrs.func_kind == FunctionKind::AsyncGenerator {
                        self.emit_op(OpCode::NIP, last_line_num, last_line_pos); // next
                        self.emit_op(OpCode::SWAP, last_line_num, last_line_pos);
                        self.emit_get_field2(Atom::CONST_return, last_line_num, last_line_pos);

                        // stack: iter_obj return_func
                        self.emit_op(OpCode::DUP, last_line_num, last_line_pos);
                        self.emit_op(OpCode::IS_UNDEFINED_OR_NULL, last_line_num, last_line_pos);

                        let label_next =
                            self.emit_goto(OpCode::IF_TRUE, -1, last_line_num, last_line_pos);
                        self.emit_op(OpCode::CALL_METHOD, last_line_num, last_line_pos);
                        self.emit_u16(0);
                        self.emit_op(OpCode::ITERATOR_CHECK_OBJECT, last_line_num, last_line_pos);
                        self.emit_op(OpCode::AWAIT, last_line_num, last_line_pos);

                        let label_next2 =
                            self.emit_goto(OpCode::GOTO, -1, last_line_num, last_line_pos);
                        self.emit_label(label_next, last_line_num, last_line_pos);
                        self.emit_op(OpCode::DROP, last_line_num, last_line_pos);
                        self.emit_label(label_next2, last_line_num, last_line_pos);
                        self.emit_op(OpCode::DROP, last_line_num, last_line_pos);
                    } else {
                        self.emit_op(OpCode::ROT3R, last_line_num, last_line_pos);
                        self.emit_op(OpCode::UNDEFINED, last_line_num, last_line_pos); // dummy catch offset
                        self.emit_op(OpCode::ITERATOR_CLOSE, last_line_num, last_line_pos);
                    }
                } else {
                    // execute the "finally" block
                    self.emit_goto(
                        OpCode::GOSUB,
                        unsafe { (&*top).label_finally },
                        last_line_num,
                        last_line_pos,
                    );
                }
            }

            top = unsafe { (*top).prev };
        }

        if self.attrs.is_derived_class_constructor {
            // 'this' can be uninitialized, so it may be accessed only if
            // the derived class constructor does not return an object
            let label_return = if hasval {
                self.emit_op(OpCode::CHECK_CTOR_RETURN, last_line_num, last_line_pos);
                let l = self.emit_goto(OpCode::IF_FALSE, -1, last_line_num, last_line_pos);
                self.emit_op(OpCode::DROP, last_line_num, last_line_pos);
                l
            } else {
                -1
            };

            // The error should be raised in the caller context, so we use
            // a specific opcode
            self.emit_op_temp(
                OpCodeTemp::SCOPE_GET_VAR_CHECKTHIS,
                last_line_num,
                last_line_pos,
            );
            self.emit_atom(Atom::CONST_this);
            self.emit_u16(0);

            self.emit_label(label_return, last_line_num, last_line_pos);
            self.emit_op(OpCode::RETURN, last_line_num, last_line_pos);
        } else if self.attrs.func_kind != FunctionKind::Normal {
            self.emit_op(OpCode::RETURN_ASYNC, last_line_num, last_line_pos);
        } else {
            self.emit_op(
                if hasval {
                    OpCode::RETURN
                } else {
                    OpCode::RETURN_UNDEF
                },
                last_line_num,
                last_line_pos,
            );
        }
    }

    // don't update the last opcode and don't emit line number info
    pub(crate) fn emit_label_raw(&mut self, label: i32) {
        debug_assert!(label >= 0);
        self.emit_u8(OpCodeTemp::LABEL as u8);
        self.emit_u32(label as _);
        self.label_slots[label as usize].pos = self.byte_code.len() as _;
    }

    /// returns the offset to `label`
    pub fn emit_label(&mut self, label: i32, last_line_num: i32, last_line_pos: u32) -> i32 {
        if label >= 0 {
            self.emit_op_temp(OpCodeTemp::LABEL, last_line_num, last_line_pos);
            self.emit_u32(label as _);
            self.label_slots[label as usize].pos = self.byte_code.len() as _;
            self.byte_code.len() as i32 - 4
        } else {
            -1
        }
    }

    /// return label or -1 if dead code
    pub fn emit_goto(
        &mut self,
        opcode: OpCode,
        mut label: i32,
        last_line_num: i32,
        last_line_pos: u32,
    ) -> i32 {
        if self.is_live_code() {
            if label < 0 {
                label = self.add_label().unwrap() as i32;
            }

            self.emit_op(opcode, last_line_num, last_line_pos);
            self.emit_u32(label as _);
            self.label_slots[label as usize].ref_count += 1;
            label
        } else {
            -1
        }
    }

    pub fn emit_push_const(
        &mut self,
        val: JSValue,
        as_atom: bool,
        last_line_num: i32,
        last_line_pos: u32,
    ) -> Result<(), i32> {
        if let Some(ss) = val.as_string()
            && as_atom
            && let Ok(atom) = {
                let mut cx = self.ctx.current_scope();
                Atom::from_string(&mut cx, ss, AtomKind::String)
            }
            && !atom.is_small_int()
        {
            self.emit_op(OpCode::PUSH_ATOM_VALUE, last_line_num, last_line_pos);
            self.emit_atom(atom);
            return Ok(());
        }

        let cidx = self.cpool_add(val)?;
        self.emit_op(OpCode::PUSH_CONST, last_line_num, last_line_pos);
        self.emit_u32(cidx as u32);
        Ok(())
    }

    // initialize the class fields, called by the constructor. Note:
    // super() can be called in an arrow function, so <this> and
    // <class_fields_init> can be variable references
    pub fn emit_class_field_init(&mut self, last_line_num: i32, last_line_pos: u32) {
        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR, last_line_num, last_line_pos);
        self.emit_atom(Atom::CONST__class_fields_init_);
        self.emit_u16(self.scope_level as _);

        // no need to call the class field initializer if not defined
        self.emit_op(OpCode::DUP, last_line_num, last_line_pos);
        let label_next = self.emit_goto(OpCode::IF_FALSE, -1, last_line_num, last_line_pos);

        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR, last_line_num, last_line_pos);
        self.emit_atom(Atom::CONST_this);
        self.emit_u16(0);

        self.emit_op(OpCode::SWAP, last_line_num, last_line_pos);

        self.emit_op(OpCode::CALL_METHOD, last_line_num, last_line_pos);
        self.emit_u16(0);

        self.emit_label(label_next, last_line_num, last_line_pos);
        self.emit_op(OpCode::DROP, last_line_num, last_line_pos);
    }

    pub fn emit_spread_code(&mut self, depth: i32, last_line_num: i32, last_line_pos: u32) {
        // XXX: could check if enum object is an actual array and optimize
        //       slice extraction. enumeration record and target array are in a
        //       different order from OP_append case.
        // enum_rec xxx -- enum_rec xxx array 0
        self.emit_op(OpCode::ARRAY_FROM, last_line_num, last_line_pos);
        self.emit_u16(0);
        self.emit_op(OpCode::PUSH_I32, last_line_num, last_line_pos);
        self.emit_u32(0);

        let label_rest_next = self.add_label().map_or(-1, |x| x as i32);
        self.emit_label(label_rest_next, last_line_num, last_line_pos);
        self.emit_op(OpCode::FOR_OF_NEXT, last_line_num, last_line_pos);
        self.emit_u8((2 + depth) as u8);

        let label_rest_done = self.emit_goto(OpCode::IF_TRUE, -1, last_line_num, last_line_pos);
        // array idx val -- array idx
        self.emit_op(OpCode::DEFINE_ARRAY_EL, last_line_num, last_line_pos);
        self.emit_op(OpCode::INC, last_line_num, last_line_pos);
        self.emit_goto(
            OpCode::GOTO,
            label_rest_next as _,
            last_line_num,
            last_line_pos,
        );
        self.emit_label(label_rest_done, last_line_num, last_line_pos);
        // enum_rec xxx array idx undef -- enum_rec xxx array
        self.emit_op(OpCode::DROP, last_line_num, last_line_pos);
        self.emit_op(OpCode::DROP, last_line_num, last_line_pos);
    }

    /// `label` is the label name, JS_ATOM_NULL for none.
    pub fn emit_break(
        &mut self,
        label: Atom,
        is_continue: bool,
        last_line_num: i32,
        last_line_pos: u32,
    ) -> Result<(), i32> {
        let mut scope_level = self.scope_level;
        let mut top_break = self.top_break;

        while !top_break.is_null() {
            let top = unsafe { top_break.as_ref().unwrap() };

            self.close_scopes(
                scope_level as u16,
                top.scope_level as u16,
                last_line_num,
                last_line_pos,
            );
            scope_level = top.scope_level;

            if is_continue && top.label_cont != -1 && (label.is_null() || top.label_name == label) {
                // continue stays inside the same block
                self.emit_goto(OpCode::GOTO, top.label_cont, last_line_num, last_line_pos);
                return Ok(());
            }
            if !is_continue && top.label_break != -1 && (label.is_null() || top.label_name == label)
            {
                self.emit_goto(OpCode::GOTO, top.label_break, last_line_num, last_line_pos);
                return Ok(());
            }

            let i = if top.has_iterator {
                self.emit_op(OpCode::ITERATOR_CLOSE, last_line_num, last_line_pos);
                3
            } else {
                0
            };
            for _ in i..top.drop_count {
                self.emit_op(OpCode::DROP, last_line_num, last_line_pos);
            }

            if top.label_finally != -1 {
                // must push dummy value to keep same stack depth
                self.emit_op(OpCode::UNDEFINED, last_line_num, last_line_pos);
                self.emit_goto(
                    OpCode::GOSUB,
                    top.label_finally,
                    last_line_num,
                    last_line_pos,
                );
                self.emit_op(OpCode::DROP, last_line_num, last_line_pos);
            }

            top_break = top.prev;
        }

        Err(-1)
    }

    #[inline]
    pub fn get_prev_opcode(&self) -> u8 {
        if self.last_opcode_pos < 0 {
            OpCode::INVALID as _
        } else {
            self.byte_code[self.last_opcode_pos as usize] as _
        }
    }

    pub fn is_live_code(&self) -> bool {
        match self.get_prev_opcode() {
            opcodes::OP_tail_call
            | opcodes::OP_tail_call_method
            | opcodes::OP_return
            | opcodes::OP_return_undef
            | opcodes::OP_return_async
            | opcodes::OP_throw
            | opcodes::OP_throw_error
            | opcodes::OP_goto
            | opcodes::OP_ret => false,

            #[cfg(feature = "short_opcode")]
            opcodes::OpCode::GOTO8 | opcodes::OpCode::GOTO16 => false,

            _ => true,
        }
    }
}

#[inline(always)]
pub(crate) fn push_vec_u8(buf: &mut Vec<u8>, val: u8) {
    buf.push(val);
}

#[inline(always)]
pub(crate) fn push_vec_u16(buf: &mut Vec<u8>, val: u16) {
    buf.extend_from_slice(&val.to_ne_bytes());
}

#[inline(always)]
pub(crate) fn push_vec_u32(buf: &mut Vec<u8>, val: u32) {
    buf.extend_from_slice(&val.to_ne_bytes());
}

#[inline(always)]
pub(crate) fn push_vec_atom(buf: &mut Vec<u8>, a: Atom) {
    push_vec_u32(buf, a.id());
}

pub(crate) fn push_vec_short_code(buf: &mut Vec<u8>, op: u8, idx: u16) {
    #[cfg(feature = "short_opcode")]
    {
        if idx < 4 {
            match op {
                opcodes::OP_get_loc => {
                    push_vec_u8(buf, OP_get_loc0 + idx);
                    return;
                }
                opcodes::OP_put_loc => {
                    push_vec_u8(buf, OP_put_loc0 + idx);
                    return;
                }
                opcodes::OP_set_loc => {
                    push_vec_u8(buf, OP_set_loc0 + idx);
                    return;
                }
                opcodes::OP_get_arg => {
                    push_vec_u8(buf, OP_get_arg0 + idx);
                    return;
                }
                opcodes::OP_put_arg => {
                    push_vec_u8(buf, OP_put_arg0 + idx);
                    return;
                }
                opcodes::OP_set_arg => {
                    push_vec_u8(buf, OP_set_arg0 + idx);
                    return;
                }
                opcodes::OP_get_var_ref => {
                    push_vec_u8(buf, OP_get_var_ref0 + idx);
                    return;
                }
                opcodes::OP_put_var_ref => {
                    push_vec_u8(buf, OP_put_var_ref0 + idx);
                    return;
                }
                opcodes::OP_set_var_ref => {
                    push_vec_u8(buf, OP_set_var_ref0 + idx);
                    return;
                }
                opcodes::OP_call => {
                    push_vec_u8(buf, OP_call0 + idx);
                    return;
                }
                _ => {}
            }
        }

        if idx < 256 {
            match op {
                opcodes::OP_get_loc => {
                    push_vec_u8(buf, OP_get_loc8);
                    push_vec_u8(buf, idx);
                    return;
                }
                opcodes::OP_put_loc => {
                    push_vec_u8(buf, OP_put_loc8);
                    push_vec_u8(buf, idx);
                    return;
                }
                opcodes::OP_set_loc => {
                    push_vec_u8(buf, OP_set_loc8);
                    push_vec_u8(buf, idx);
                    return;
                }
                _ => {}
            }
        }
    }

    push_vec_u8(buf, op);
    push_vec_u16(buf, idx);
}

pub(crate) fn push_vec_short_int(buf: &mut Vec<u8>, val: i32) {
    #[cfg(feature = "short_opcode")]
    {
        if val >= -1 && val <= 7 {
            put_vec_u8(buf, OP_push_0 + val);
            return;
        }
        if val == (val as i8) as i32 {
            put_vec_u8(buf, OP_push_i8);
            put_vec_u8(buf, val as _);
            return;
        }
        if val == (val as i16) as i32 {
            put_vec_u8(buf, OP_push_i16);
            dbuf_put_u16(buf, val as _);
            return;
        }
    }

    push_vec_u8(buf, opcodes::OP_push_i32);
    push_vec_u32(buf, val as _);
}

pub(crate) fn push_vec_slice(buf: &mut Vec<u8>, data: *const u8, len: usize) -> Result<(), i32> {
    if buf.len() + len < buf.capacity() {
        buf.try_reserve(len).map_err(|_| -1)?;
    }
    buf.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    Ok(())
}

pub(crate) fn push_vec_leb128(buf: &mut Vec<u8>, val: u32) {
    let mut v = val;
    loop {
        let a = (v & 0x7f) as u8;
        v >>= 7;

        if v != 0 {
            buf.push(a | 0x80);
        } else {
            buf.push(a);
            break;
        }
    }
}

pub(crate) fn push_vec_sleb128(s: &mut Vec<u8>, val: i32) {
    // Standard ZigZag encoding: compatible with get_sleb128 decoder.
    // For positive values this is identical to 2*val; for negative
    // values it uses (val << 1) ^ (val >> 31) to interleave sign bit.
    let v = ((val << 1) ^ (val >> 31)) as u32;
    push_vec_leb128(s, v);
}
