// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpcodeFormat, opcode_info_final, opcode_info_intermediate, opcodes},
    function::{
        FunctionDef, Label, push_vec_atom, push_vec_short_code, push_vec_short_int, push_vec_u8,
        push_vec_u16,
    },
    utils::{M2, M3, read_from_ptr, write_to_ptr},
};

pub const OPTIMIZE: bool = true;

#[derive(Debug, Clone)]
pub(super) struct CodeMatchState {
    /// position past the matched code pattern
    pub pos: usize,
    /// last visited op_line_num parameter or -1
    pub line_num: i32,

    pub op: i32,
    pub idx: i32,
    pub label: i32,
    pub val: i32,
    pub atom: Atom,
}

impl Default for CodeMatchState {
    fn default() -> Self {
        Self {
            pos: 0,
            line_num: -1,
            op: 0,
            idx: 0,
            label: 0,
            val: 0,
            atom: Atom::NULL,
        }
    }
}

impl FunctionDef {
    /// match in bytecodes for specified `pattern`, starting since `pos`.
    pub(super) fn code_match(&self, mut pos: usize, pattern: &[i32]) -> Option<CodeMatchState> {
        let bc_ptr = self.byte_code.as_ptr();
        let bc_len = self.byte_code.len();

        let mut state = CodeMatchState::default();
        let mut line_num = -1;
        let mut pos_next: usize;
        let mut found = false;

        let mut it = pattern.iter();
        loop {
            let op_arg = it.next().copied().unwrap_or(-1);
            if op_arg == -1 {
                state.pos = pos;
                state.line_num = line_num;
                found = true;
                break;
            }

            let op = loop {
                if pos >= bc_len {
                    return None;
                }

                let op = unsafe { *bc_ptr.add(pos) };
                pos_next = pos + opcode_info_intermediate(op).size as usize;
                if pos_next > bc_len {
                    return None;
                }
                if op == opcodes::op_line_num {
                    line_num = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 1)) };
                    pos = pos_next;
                } else {
                    break op;
                }
            };

            if op_arg != op as i32 {
                if op_arg == (op_arg as u8) as i32 || op == 0 {
                    break; // stop iter
                }
                if op != (op_arg as u8)
                    && op != ((op_arg >> 8) as u8)
                    && op != ((op_arg >> 16) as u8)
                    && op != ((op_arg >> 24) as u8)
                {
                    break; // stop iter
                }
                state.op = op as i32;
            }

            pos += 1;

            unsafe {
                match opcode_info_intermediate(op).fmt {
                    OpcodeFormat::LOC8 | OpcodeFormat::U8 => {
                        let idx = *bc_ptr.add(pos);
                        let arg = it.next().copied().unwrap_or(-1);
                        if arg == -1 {
                            state.idx = idx as i32;
                        } else if arg != idx as i32 {
                            return None;
                        }
                    }
                    OpcodeFormat::U16
                    | OpcodeFormat::NPOP
                    | OpcodeFormat::LOC
                    | OpcodeFormat::ARG
                    | OpcodeFormat::VAR_REF => {
                        let idx = read_from_ptr::<u16>(bc_ptr.add(pos)) as i32;
                        let arg = it.next().copied().unwrap_or(-1);
                        if arg == -1 {
                            state.idx = idx;
                        } else if arg != idx {
                            return None;
                        }
                    }
                    OpcodeFormat::I32
                    | OpcodeFormat::U32
                    | OpcodeFormat::LABEL
                    | OpcodeFormat::CONST => {
                        state.label = read_from_ptr::<u32>(bc_ptr.add(pos)) as _;
                    }
                    OpcodeFormat::LABEL_U16 => {
                        state.label = read_from_ptr::<u32>(bc_ptr.add(pos)) as _;
                        state.val = read_from_ptr::<u16>(bc_ptr.add(pos + 4)) as _;
                    }
                    OpcodeFormat::ATOM => {
                        state.atom = Atom::from_id(read_from_ptr::<u32>(bc_ptr.add(pos)));
                    }
                    OpcodeFormat::ATOM_U8 => {
                        state.atom = Atom::from_id(read_from_ptr::<u32>(bc_ptr.add(pos)));
                        state.val = *bc_ptr.add(pos + 4) as _;
                    }
                    OpcodeFormat::ATOM_U16 => {
                        state.atom = Atom::from_id(read_from_ptr::<u32>(bc_ptr.add(pos)));
                        state.val = read_from_ptr::<u16>(bc_ptr.add(pos + 4)) as _;
                    }
                    OpcodeFormat::ATOM_LABEL_U8 => {
                        state.atom = Atom::from_id(read_from_ptr::<u32>(bc_ptr.add(pos)));
                        state.label = read_from_ptr::<u32>(bc_ptr.add(pos + 4)) as _;
                        state.val = read_from_ptr::<u8>(bc_ptr.add(pos + 8)) as _;
                    }
                    _ => {}
                }
            }

            pos = pos_next;
        }

        if found { Some(state) } else { None }
    }

    pub(super) fn optimize_get_loc(
        &mut self,
        pos: usize,
        pos_next: &mut usize,
        line_num: &mut i32,
        line_pos: &mut u32,
        bc_out: &mut Vec<u8>,
    ) -> bool {
        // transformation:
        // get_loc(n) post_dec put_loc(n) drop -> dec_loc(n)
        // get_loc(n) post_inc put_loc(n) drop -> inc_loc(n)
        // get_loc(n) dec dup put_loc(n) drop -> dec_loc(n)
        // get_loc(n) inc dup put_loc(n) drop -> inc_loc(n)

        let bc_buf = self.byte_code.as_ptr();

        let idx = unsafe { read_from_ptr::<u16>(bc_buf.add(pos + 1)) };
        if idx >= 256 {
            false
        } else {
            if let Some(cc) = self
                .code_match(
                    *pos_next,
                    &[
                        M2(opcodes::OP_post_dec as _, opcodes::OP_post_inc as _),
                        opcodes::OP_put_loc as _,
                        idx as _,
                        opcodes::OP_drop as _,
                    ],
                )
                .or_else(|| {
                    self.code_match(
                        *pos_next,
                        &[
                            M2(opcodes::OP_dec as _, opcodes::OP_inc as _),
                            opcodes::OP_dup as _,
                            opcodes::OP_put_loc as _,
                            idx as _,
                            opcodes::OP_drop as _,
                        ],
                    )
                })
            {
                if cc.line_num >= 0 {
                    *line_num = cc.line_num;
                }
                self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
                push_vec_u8(
                    bc_out,
                    if matches!(cc.op as u8, opcodes::OP_inc | opcodes::OP_post_inc) {
                        opcodes::OP_inc_loc
                    } else {
                        opcodes::OP_dec_loc
                    },
                );
                push_vec_u8(bc_out, idx as u8);
                *pos_next = cc.pos;
            } else if let Some(cc) = self.code_match(
                *pos_next,
                &[
                    opcodes::OP_push_atom_value as _,
                    opcodes::OP_add as _,
                    opcodes::OP_dup as _,
                    opcodes::OP_put_loc as _,
                    idx as _,
                    opcodes::OP_drop as _,
                ],
            ) {
                // transformation:
                // get_loc(n) push_atom_value(x) add dup put_loc(n) drop -> push_atom_value(x) add_loc(n)
                if cc.line_num >= 0 {
                    *line_num = cc.line_num as _;
                }
                self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);

                #[cfg(feature = "short_opcode")]
                {
                    if cc.atom == JS_ATOM_empty_string {
                        JS_FreeAtom(ctx, cc.atom);
                        dbuf_putc(&mut bc_out, OP_push_empty_string);
                    } else {
                        push_vec_u8(&mut bc_out, opcodes::OP_push_atom_value);
                        push_vec_atom(&mut bc_out, cc.atom);
                    }
                }
                #[cfg(not(feature = "short_opcode"))]
                {
                    push_vec_u8(bc_out, opcodes::OP_push_atom_value);
                    push_vec_atom(bc_out, cc.atom);
                }

                push_vec_u8(bc_out, opcodes::OP_add_loc);
                push_vec_u8(bc_out, idx as u8);
                *pos_next = cc.pos;
            } else if let Some(cc) = self.code_match(
                *pos_next,
                &[
                    opcodes::OP_push_i32 as _,
                    opcodes::OP_add as _,
                    opcodes::OP_dup as _,
                    opcodes::OP_put_loc as _,
                    idx as _,
                    opcodes::OP_drop as _,
                ],
            ) {
                // transformation:
                // get_loc(n) push_i32(x) add dup put_loc(n) drop -> push_i32(x) add_loc(n)
                if cc.line_num >= 0 {
                    *line_num = cc.line_num as _;
                }
                self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
                push_vec_short_int(bc_out, cc.label);
                push_vec_u8(bc_out, opcodes::OP_add_loc);
                push_vec_u8(bc_out, idx as u8);
                *pos_next = cc.pos;
            } else if let Some(cc) = self.code_match(
                *pos_next,
                &[
                    M3(
                        opcodes::OP_get_loc as _,
                        opcodes::OP_get_arg as _,
                        opcodes::OP_get_var_ref as _,
                    ),
                    -1,
                    opcodes::OP_add as _,
                    opcodes::OP_dup as _,
                    opcodes::OP_put_loc as _,
                    idx as _,
                    opcodes::OP_drop as _,
                    -1,
                ],
            ) {
                // transformation: XXX: also do these:
                // get_loc(n) get_loc(x) add dup put_loc(n) drop -> get_loc(x) add_loc(n)
                // get_loc(n) get_arg(x) add dup put_loc(n) drop -> get_arg(x) add_loc(n)
                // get_loc(n) get_var_ref(x) add dup put_loc(n) drop -> get_var_ref(x) add_loc(n)

                if cc.line_num >= 0 {
                    *line_num = cc.line_num as _;
                }
                self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
                push_vec_short_code(bc_out, cc.op as _, cc.idx as _);
                push_vec_u8(bc_out, opcodes::OP_add_loc);
                push_vec_u8(bc_out, idx as u8);
                *pos_next = cc.pos;
            } else {
                self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
                push_vec_short_code(bc_out, OpCode::GET_LOC as u8, idx);
            }

            true
        }
    }

    pub(super) fn optimize_put_loc(
        &mut self,
        op: u8,
        pos: usize,
        pos_next: &mut usize,
        line_num: &mut i32,
        line_pos: &mut u32,
        bc_out: &mut Vec<u8>,
    ) -> bool {
        debug_assert!(matches!(
            op,
            opcodes::OP_put_loc | opcodes::OP_put_arg | opcodes::OP_put_var_ref,
        ));

        let bc_buf = self.byte_code.as_ptr();

        // transformation: put_x(n) get_x(n) -> set_x(n)
        let idx = unsafe { read_from_ptr::<u16>(bc_buf.add(pos + 1)) };
        let get_op = match op {
            opcodes::OP_put_loc => opcodes::OP_get_loc as i32,
            opcodes::OP_put_arg => opcodes::OP_get_arg as i32,
            opcodes::OP_put_var_ref => opcodes::OP_get_var_ref as i32,
            _ => unreachable!(),
        };
        if let Some(cc) = self.code_match(*pos_next, &[get_op, idx as _]) {
            if cc.line_num >= 0 {
                *line_num = cc.line_num;
            }
            self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);

            let set_op = match op {
                opcodes::OP_put_loc => opcodes::OP_set_loc,
                opcodes::OP_put_arg => opcodes::OP_set_arg,
                opcodes::OP_put_var_ref => opcodes::OP_set_var_ref,
                _ => unreachable!(),
            };
            push_vec_short_code(bc_out, set_op, idx);
            *pos_next = cc.pos;
        } else {
            self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
            push_vec_short_code(bc_out, op, idx);
        }

        true
    }

    pub(super) fn optimize_post_dec_inc(
        &mut self,
        op: u8,
        _pos: usize,
        pos_next: &mut usize,
        line_num: &mut i32,
        line_pos: &mut u32,
        bc_out: &mut Vec<u8>,
    ) -> bool {
        debug_assert!(matches!(op, opcodes::OP_post_dec | opcodes::OP_post_inc,));

        // transformation:
        // post_inc put_x drop -> inc put_x
        // post_inc put_x(n) get_x(n) drop -> inc set_x(n)  (compound assignment)
        // post_inc perm3 put_field drop -> inc put_field
        // post_inc perm3 put_field drop -> inc put_field
        // post_inc perm4 put_array_el drop -> inc put_array_el
        if let Some(cc) = self.code_match(
            *pos_next,
            &[
                M3(
                    OpCode::PUT_LOC as _,
                    OpCode::PUT_ARG as _,
                    OpCode::PUT_VAR_REF as _,
                ),
                -1,
                OpCode::DROP as _,
            ],
        ) {
            if cc.line_num >= 0 {
                *line_num = cc.line_num as _;
            }
            let put_op = cc.op as u8;
            let idx = cc.idx;
            *pos_next = cc.pos;

            let get_op = match put_op {
                opcodes::OP_put_loc => OpCode::GET_LOC as i32,
                opcodes::OP_put_arg => OpCode::GET_ARG as i32,
                opcodes::OP_put_var_ref => OpCode::GET_VAR_REF as i32,
                _ => unreachable!(),
            };
            let set_op = match put_op {
                opcodes::OP_put_loc => OpCode::SET_LOC as u8,
                opcodes::OP_put_arg => OpCode::SET_ARG as u8,
                opcodes::OP_put_var_ref => OpCode::SET_VAR_REF as u8,
                _ => unreachable!(),
            };
            let mut final_op = put_op;
            if let Some(m) = self.code_match(cc.pos, &[get_op, idx]) {
                if m.line_num >= 0 {
                    *line_num = m.line_num as _;
                }
                final_op = set_op; // put_x(n) get_x(n) -> set_x(n)
                *pos_next = m.pos;
            }
            self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
            let dec_inc = match op {
                opcodes::OP_post_dec => OpCode::DEC as u8,
                opcodes::OP_post_inc => OpCode::INC as u8,
                _ => unreachable!(),
            };
            push_vec_u8(bc_out, dec_inc);
            push_vec_short_code(bc_out, final_op, idx as u16);
            return true;
        }

        if let Some(cc) = self.code_match(
            *pos_next,
            &[
                OpCode::PERM3 as _,
                OpCode::PUT_FIELD as _,
                OpCode::DROP as _,
            ],
        ) {
            if cc.line_num >= 0 {
                *line_num = cc.line_num as _;
            }
            self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
            let dec_inc = match op {
                opcodes::OP_post_dec => OpCode::DEC as u8,
                opcodes::OP_post_inc => OpCode::INC as u8,
                _ => unreachable!(),
            };
            push_vec_u8(bc_out, dec_inc);
            push_vec_u8(bc_out, cc.op as u8);
            push_vec_atom(bc_out, cc.atom);
            *pos_next = cc.pos;
            return true;
        }

        if let Some(cc) = self.code_match(
            *pos_next,
            &[
                OpCode::PERM4 as _,
                OpCode::PUT_ARRAY_EL as _,
                OpCode::DROP as _,
            ],
        ) {
            if cc.line_num >= 0 {
                *line_num = cc.line_num as _;
            }
            self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
            let dec_inc = match op {
                opcodes::OP_post_dec => OpCode::DEC as u8,
                opcodes::OP_post_inc => OpCode::INC as u8,
                _ => unreachable!(),
            };
            push_vec_u8(bc_out, dec_inc);
            push_vec_u8(bc_out, OpCode::PUT_ARRAY_EL as u8);
            *pos_next = cc.pos;
            return true;
        }

        false
    }

    pub(super) fn optimize_if_true_false(
        &mut self,
        mut op: u8,
        pos: usize,
        pos_next: &mut usize,
        line_num: &mut i32,
        line_pos: &mut u32,
        bc_out: &mut Vec<u8>,
    ) -> Option<(u8, u32)> {
        debug_assert!(matches!(op, opcodes::OP_if_true | opcodes::OP_if_false));

        let bc_buf = self.byte_code.as_ptr();

        let mut label = unsafe { read_from_ptr::<u32>(bc_buf.add(pos + 1)) };
        (label, _, _) = self.find_jump_target(label);

         // transform if_false/if_true(l1) label(l1) -> drop label(l1) 
        if self.code_has_label(*pos_next, label) {
            self.update_label_ref_count(label, -1);
            push_vec_u8(bc_out, OpCode::DROP as u8);
            return None;
        }

         // transform if_false(l1) goto(l2) label(l1) -> if_false(l2) label(l1) 
        if let Some(cc) = self.code_match(*pos_next, &[OpCode::GOTO as i32]) {
            let pos1 = cc.pos;
            let line1 = cc.line_num;
            if self.code_has_label(pos1, label) {
                if line1 >= 0 {
                    *line_num = line1 as _;
                }
                *pos_next = pos1 as _;
                self.update_label_ref_count(label, -1);

                label = cc.label as _;
                op = if op == opcodes::OP_if_true {
                    opcodes::OP_if_false
                } else {
                    opcodes::OP_if_true
                }; // true->false, false->true
            }
        }

        Some((op, label))
    }

    pub(super) fn optimize_dup(
        &mut self,
        _pos: usize,
        pos_next: &mut usize,
        line_num: &mut i32,
        line_pos: &mut u32,
        bc_out: &mut Vec<u8>,
    ) -> bool {
        // Transformation: dup put_x(n) drop -> put_x(n)
        // Transformation: dup put_x(n) -> set_x(n)
        let mut line2: i32 = -1;

        if let Some(cc) = self.code_match(
            *pos_next,
            &[M3(
                OpCode::PUT_LOC as _,
                OpCode::PUT_ARG as _,
                OpCode::PUT_VAR_REF as _,
            )],
        ) {
            if cc.line_num >= 0 {
                *line_num = cc.line_num;
            }
            let put_op = cc.op as u8;
            let idx = cc.idx;
            *pos_next = cc.pos;

            let set_op = match put_op {
                opcodes::OP_put_loc => OpCode::SET_LOC as u8,
                opcodes::OP_put_arg => OpCode::SET_ARG as u8,
                opcodes::OP_put_var_ref => OpCode::SET_VAR_REF as u8,
                _ => unreachable!(),
            };
            let get_op = match put_op {
                opcodes::OP_put_loc => OpCode::GET_LOC as i32,
                opcodes::OP_put_arg => OpCode::GET_ARG as i32,
                opcodes::OP_put_var_ref => OpCode::GET_VAR_REF as i32,
                _ => unreachable!(),
            };
            let mut final_op = set_op;

            if let Some(cc1) = self.code_match(cc.pos, &[OpCode::DROP as i32]) {
                if cc1.line_num >= 0 {
                    *line_num = cc1.line_num;
                }
                final_op = put_op; // set_x drop -> put_x
                *pos_next = cc1.pos;

                if let Some(cc2) = self.code_match(cc1.pos, &[get_op, idx]) {
                    line2 = cc2.line_num; // delay line number update
                    final_op = set_op; // put_x(n) get_x(n) -> set_x(n)
                    *pos_next = cc2.pos;
                }
            }

            self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
            push_vec_short_code(bc_out, final_op, idx as u16);
            if line2 >= 0 {
                *line_num = line2;
            }
            return true;
        }

        false
    }
}

pub(super) const fn can_optimize_put_ref_value(bc_buf: *const u8, pos: i32) -> bool {
    unsafe {
        *bc_buf.offset((pos + 1) as isize) == opcodes::OP_put_ref_value
            && matches!(
                *bc_buf.offset(pos as isize),
                opcodes::OP_insert3 | opcodes::OP_perm4 | opcodes::OP_nop | opcodes::OP_rot3l
            )
    }
}

pub(super) const fn can_optimize_put_global_ref_value(bc_buf: *const u8, pos: i32) -> bool {
    unsafe {
        *bc_buf.offset((pos + 1) as isize) == opcodes::OP_put_ref_value
            && matches!(
                *bc_buf.offset(pos as isize),
                opcodes::OP_insert3 | opcodes::OP_perm4 | opcodes::OP_nop | opcodes::OP_rot3l
            )
    }
}

pub(super) fn optimize_scope_make_ref(
    bc: &mut Vec<u8>,
    bc_orig: *mut u8,
    label: &Label,
    mut pos_next: usize,
    get_op: OpCode,
    var_idx: i32,
) -> usize {
    debug_assert!(label.pos >= 0);

    // ── Derived opcode sizes ──
    const label_op_size: usize = opcode_info_intermediate(opcodes::op_label).size as usize; // op_label(1) + u32(4)
    const stack_op_size: usize = opcode_info_final(opcodes::OP_insert3).size as usize; // 1B
    const put_ref_size: usize = opcode_info_final(opcodes::OP_put_ref_value).size as usize; // 1B
    const get_ref_size: usize = opcode_info_final(opcodes::OP_get_ref_value).size as usize; // 1B

     // XXX: should optimize `loc(a) += expr` as `expr add_loc(a)`
      // but only if expr does not modify `a`.
      // should scan the code between pos_next and label_pos
      // for operations that can potentially change `a`:
      // OP_scope_make_ref(a), function calls, jumps and gosub.
     // replace the reference get/put with normal variable accesses 
    if unsafe { *bc_orig.add(pos_next) } == opcodes::OP_get_ref_value {
        push_vec_u8(bc, get_op as u8);
        push_vec_u16(bc, var_idx as u16);
        pos_next += get_ref_size;
    }

    let label_pos = label.pos as usize;
    // The region being replaced spans from the op_label temp opcode
    // (at `label_pos - label_op_size`) through the stack-op pair
    // (stack_op at `label_pos`, put_ref_value at `label_pos + 1`).
    let mut pos = label_pos - label_op_size;
    debug_assert_eq!(unsafe { *bc_orig.add(pos) }, opcodes::op_label);

     // label points to an instruction pair:
      // - insert3 + put_ref_value => dup + put_var_ref
      // - perm4 / put_ref_value
      // - rot3l / put_ref_value
      // - nop / put_ref_value
    let end_pos = label_pos + stack_op_size + put_ref_size;

    let op_size: usize;
    unsafe {
        if *bc_orig.add(label_pos) == opcodes::OP_insert3 {
            // KEEP_TOP: keep value on stack as assignment expression result.
            // Use explicit opcodes (no get_op arithmetic) since CHECK variants
            // have a different encoding layout (GET/PUT/PUT_INIT/SET vs GET/PUT/SET).
            match get_op {
                OpCode::GET_ARG => {
                    *bc_orig.add(pos) = OpCode::SET_ARG as u8;
                    op_size = opcode_info_final(OpCode::SET_ARG as u8).size as usize;
                }
                OpCode::GET_LOC => {
                    *bc_orig.add(pos) = OpCode::SET_LOC as u8;
                    op_size = opcode_info_final(OpCode::SET_LOC as u8).size as usize;
                }
                OpCode::GET_VAR_REF => {
                    *bc_orig.add(pos) = OpCode::SET_VAR_REF as u8;
                    op_size = opcode_info_final(OpCode::SET_VAR_REF as u8).size as usize;
                }
                OpCode::GET_LOC_CHECK => {
                    *bc_orig.add(pos) = OpCode::SET_LOC_CHECK as u8;
                    op_size = opcode_info_final(OpCode::SET_LOC_CHECK as u8).size as usize;
                }
                OpCode::GET_VAR_REF_CHECK => {
                    // No SET_VAR_REF_CHECK opcode; fall back to dup + put_var_ref_check.
                    *bc_orig.add(pos) = opcodes::OP_dup;
                    pos += stack_op_size;
                    *bc_orig.add(pos) = OpCode::PUT_VAR_REF_CHECK as u8;
                    op_size = opcode_info_final(OpCode::PUT_VAR_REF_CHECK as u8).size as usize;
                }
                OpCode::GET_VAR => {
                    // dup + put_var for global var assignment
                    *bc_orig.add(pos) = opcodes::OP_dup;
                    pos += stack_op_size;
                    *bc_orig.add(pos) = OpCode::PUT_VAR as u8;
                    op_size = opcode_info_final(OpCode::PUT_VAR as u8).size as usize;
                }
                _ => unreachable!(),
            }
            write_to_ptr::<u16>(bc_orig.add(pos + 1), var_idx as u16);
        } else {
            // NOKEEP: value consumed from stack (n_pop=1, n_push=0).
            match get_op {
                OpCode::GET_ARG => {
                    *bc_orig.add(pos) = OpCode::PUT_ARG as u8;
                    op_size = opcode_info_final(OpCode::PUT_ARG as u8).size as usize;
                }
                OpCode::GET_LOC => {
                    *bc_orig.add(pos) = OpCode::PUT_LOC as u8;
                    op_size = opcode_info_final(OpCode::PUT_LOC as u8).size as usize;
                }
                OpCode::GET_VAR_REF => {
                    *bc_orig.add(pos) = OpCode::PUT_VAR_REF as u8;
                    op_size = opcode_info_final(OpCode::PUT_VAR_REF as u8).size as usize;
                }
                OpCode::GET_LOC_CHECK => {
                    *bc_orig.add(pos) = OpCode::PUT_LOC_CHECK as u8;
                    op_size = opcode_info_final(OpCode::PUT_LOC_CHECK as u8).size as usize;
                }
                OpCode::GET_VAR_REF_CHECK => {
                    *bc_orig.add(pos) = OpCode::PUT_VAR_REF_CHECK as u8;
                    op_size = opcode_info_final(OpCode::PUT_VAR_REF_CHECK as u8).size as usize;
                }
                OpCode::GET_VAR => {
                    // Like QuickJS: put_var for global var assignment (nokeep)
                    *bc_orig.add(pos) = OpCode::PUT_VAR as u8;
                    op_size = opcode_info_final(OpCode::PUT_VAR as u8).size as usize;
                }
                _ => unreachable!(),
            }
            write_to_ptr::<u16>(bc_orig.add(pos + 1), var_idx as u16);
        }
    }

    pos += op_size;

    // pad with OP_nop
    while pos < end_pos {
        unsafe {
            *bc_orig.add(pos) = opcodes::OP_nop;
        }
        pos += 1;
    }

    pos_next
}
