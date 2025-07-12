use std::marker::PhantomData;

use crate::{
    atom::{JS_ATOM_NULL, JSAtom},
    bytecode::{OpCode, opcode_info_intermediate, opcodes},
    context::{JS_MODE_STRICT, JSContext},
    function::{JSFunctionDef, LabelSlot, find_jump_target},
    utils::{
        M2, M3, push_vec_short_code, push_vec_short_int, push_vec_u8, push_vec_u16, push_vec_u32,
        read_from_ptr, write_to_ptr,
    },
};

pub const OPTIMIZE: bool = true;

#[derive(Clone, Copy)]
pub(super) struct CodeMatchState {
    /// position past the matched code pattern
    pub pos: usize,
    /// last visited op_line_num parameter or -1
    pub line_num: i32,

    pub op: i32,
    pub idx: i32,
    pub label: i32,
    pub val: i32,
    pub atom: JSAtom,
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
            atom: JS_ATOM_NULL,
        }
    }
}

impl JSFunctionDef {
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

                let op = unsafe { *bc_ptr.add(pos as usize) };
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
                    "loc8" | "u8" => {
                        let idx = *bc_ptr.add(pos);
                        let arg = it.next().copied().unwrap_or(-1);
                        if arg == -1 {
                            state.idx = idx as i32;
                        } else if arg != idx as i32 {
                            return None;
                        }
                    }
                    "u16" | "npop" | "loc" | "arg" | "var_ref" => {
                        let idx = read_from_ptr::<u16>(bc_ptr.add(pos)) as i32;
                        let arg = it.next().copied().unwrap_or(-1);
                        if arg == -1 {
                            state.idx = idx;
                        } else if arg != idx {
                            return None;
                        }
                    }
                    "i32" | "u32" | "label" | "const" => {
                        state.label = read_from_ptr::<u32>(bc_ptr.add(pos)) as _;
                    }
                    "label_u16" => {
                        state.label = read_from_ptr::<u32>(bc_ptr.add(pos)) as _;
                        state.val = read_from_ptr::<u16>(bc_ptr.add(pos + 4)) as _;
                    }
                    "atom" => {
                        state.atom = JSAtom(read_from_ptr::<u32>(bc_ptr.add(pos)));
                    }
                    "atom_u8" => {
                        state.atom = JSAtom(read_from_ptr::<u32>(bc_ptr.add(pos)));
                        state.val = *bc_ptr.add(pos + 4) as _;
                    }
                    "atom_u16" => {
                        state.atom = JSAtom(read_from_ptr::<u32>(bc_ptr.add(pos)));
                        state.val = read_from_ptr::<u16>(bc_ptr.add(pos + 4)) as _;
                    }
                    "atom_label_u8" => {
                        state.atom = JSAtom(read_from_ptr::<u32>(bc_ptr.add(pos)));
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
        self: &mut JSFunctionDef,
        pos: usize,
        pos_next: &mut usize,
        line_num: &mut i32,
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
                self.add_pc2line_info(bc_out.len() as _, *line_num);
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
                self.add_pc2line_info(bc_out.len() as _, *line_num);

                #[cfg(feature = "short_opcode")]
                {
                    if cc.atom == JS_ATOM_empty_string {
                        JS_FreeAtom(ctx, cc.atom);
                        dbuf_putc(&mut bc_out, OP_push_empty_string);
                    } else {
                        push_vec_u8(&mut bc_out, opcodes::OP_push_atom_value);
                        push_vec_u32(&mut bc_out, cc.atom);
                    }
                }
                #[cfg(not(feature = "short_opcode"))]
                {
                    push_vec_u8(bc_out, opcodes::OP_push_atom_value);
                    push_vec_u32(bc_out, cc.atom.0);
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
                self.add_pc2line_info(bc_out.len() as _, *line_num);
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
                self.add_pc2line_info(bc_out.len() as _, *line_num);
                push_vec_short_code(bc_out, cc.op as _, cc.idx as _);
                push_vec_u8(bc_out, opcodes::OP_add_loc);
                push_vec_u8(bc_out, idx as u8);
                *pos_next = cc.pos;
            } else {
                self.add_pc2line_info(bc_out.len() as _, *line_num);
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
        bc_out: &mut Vec<u8>,
    ) -> bool {
        debug_assert!(matches!(
            op,
            opcodes::OP_put_loc | opcodes::OP_put_arg | opcodes::OP_put_var_ref,
        ));

        let bc_buf = self.byte_code.as_ptr();

        // transformation: put_x(n) get_x(n) -> set_x(n)
        let idx = unsafe { read_from_ptr::<u16>(bc_buf.add(pos + 1)) };
        if let Some(cc) = self.code_match(*pos_next, &[op as i32 - 1, idx as _]) {
            if cc.line_num >= 0 {
                *line_num = cc.line_num;
            }
            self.add_pc2line_info(bc_out.len() as _, *line_num);
            push_vec_short_code(bc_out, op + 1, idx);
            *pos_next = cc.pos;
        } else {
            self.add_pc2line_info(bc_out.len() as _, *line_num);
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
        bc_out: &mut Vec<u8>,
    ) -> bool {
        debug_assert!(matches!(op, opcodes::OP_post_dec | opcodes::OP_post_inc,));

        // transformation:
        // post_inc put_x drop -> inc put_x
        // post_inc perm3 put_field drop -> inc put_field
        // post_inc perm3 put_var_strict drop -> inc put_var_strict
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
            let mut op1 = cc.op;
            let idx = cc.idx;
            *pos_next = cc.pos;

            if let Some(m) = self.code_match(cc.pos, &[op1 - 1, idx]) {
                if m.line_num >= 0 {
                    *line_num = m.line_num as _;
                }
                op1 += 1; // put_x(n) get_x(n) -> set_x(n)
                *pos_next = m.pos;
            }
            self.add_pc2line_info(bc_out.len() as _, *line_num);
            push_vec_u8(bc_out, OpCode::DEC as u8 + (op - OpCode::POST_DEC as u8));
            push_vec_short_code(bc_out, op1 as u8, idx as u16);
            return true;
        }

        if let Some(cc) = self.code_match(
            *pos_next,
            &[
                OpCode::PERM3 as _,
                M2(OpCode::PUT_FIELD as _, OpCode::PUT_VAR_STRICT as _),
                OpCode::DROP as _,
            ],
        ) {
            if cc.line_num >= 0 {
                *line_num = cc.line_num as _;
            }
            self.add_pc2line_info(bc_out.len() as _, *line_num);
            push_vec_u8(bc_out, OpCode::DEC as u8 + (op - OpCode::POST_DEC as u8));
            push_vec_u8(bc_out, cc.op as u8);
            push_vec_u32(bc_out, cc.atom.0);
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
            self.add_pc2line_info(bc_out.len() as _, *line_num);
            push_vec_u8(bc_out, OpCode::DEC as u8 + (op - OpCode::POST_DEC as u8));
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
        bc_out: &mut Vec<u8>,
    ) -> Option<(u8, u32)> {
        debug_assert!(matches!(op, opcodes::OP_if_true | opcodes::OP_if_false));

        let bc_buf = self.byte_code.as_ptr();
        let mut label = unsafe { read_from_ptr::<u32>(bc_buf.add(pos + 1)) };

        let mut _op1: u8 = 0;
        label = unsafe { find_jump_target(self, label, &mut _op1, std::ptr::null_mut()) };

        /* transform if_false/if_true(l1) label(l1) -> drop label(l1) */
        if self.code_has_label(*pos_next, label) {
            self.update_label_ref_count(label, -1);
            push_vec_u8(bc_out, OpCode::DROP as u8);
            return None;
        }

        /* transform if_false(l1) goto(l2) label(l1) -> if_false(l2) label(l1) */
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
                op ^= opcodes::OP_if_true ^ opcodes::OP_if_false; // true->false, false->true
            }
        }

        Some((op, label))
    }

    pub(super) fn optimize_dup(
        &mut self,
        _pos: usize,
        pos_next: &mut usize,
        line_num: &mut i32,
        bc_out: &mut Vec<u8>,
    ) -> bool {
        // Transformation: dup put_x(n) drop -> put_x(n)
        // Transformation: dup put_x(n) -> set_x(n)
        let mut op1: i32;
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
            op1 = cc.op + 1; // put_x -> set_x
            *pos_next = cc.pos;

            if let Some(cc1) = self.code_match(cc.pos, &[OpCode::DROP as i32]) {
                if cc1.line_num >= 0 {
                    *line_num = cc1.line_num;
                }
                op1 -= 1; // set_x drop -> put_x
                *pos_next = cc1.pos;

                if let Some(cc2) = self.code_match(cc1.pos, &[op1 - 1, cc1.idx as i32]) {
                    line2 = cc2.line_num; // delay line number update
                    op1 += 1; // put_x(n) get_x(n) -> set_x(n)
                    *pos_next = cc2.pos;
                }
            }

            self.add_pc2line_info(bc_out.len() as _, *line_num);
            push_vec_short_code(bc_out, op1 as u8, cc.idx as _);
            if line2 >= 0 {
                *line_num = line2;
            }
            return true;
        }

        false
    }
}

pub fn can_optimize_put_ref_value(bc_buf: *const u8, pos: i32) -> bool {
    unsafe {
        *bc_buf.offset((pos + 1) as isize) == opcodes::OP_put_ref_value
            && matches!(
                *bc_buf.offset(pos as isize),
                opcodes::OP_insert3 | opcodes::OP_perm4 | opcodes::OP_nop | opcodes::OP_rot3l
            )
    }
}

pub fn can_optimize_put_global_ref_value(bc_buf: *const u8, pos: i32) -> bool {
    unsafe {
        *bc_buf.offset((pos + 1) as isize) == opcodes::OP_put_ref_value
            && matches!(
                *bc_buf.offset(pos as isize),
                opcodes::OP_insert3 | opcodes::OP_perm4 | opcodes::OP_nop | opcodes::OP_rot3l
            )
    }
}

pub fn optimize_scope_make_ref(
    bc: &mut Vec<u8>,
    bc_buf: *mut u8,
    label: &LabelSlot,
    mut pos_next: usize,
    get_op: OpCode,
    var_idx: i32,
) -> usize {
    debug_assert!(label.pos >= 0);

    /* XXX: should optimize `loc(a) += expr` as `expr add_loc(a)`
      but only if expr does not modify `a`.
      should scan the code between pos_next and label_pos
      for operations that can potentially change `a`:
      OP_scope_make_ref(a), function calls, jumps and gosub.
    */
    /* replace the reference get/put with normal variable accesses */
    if unsafe { *bc_buf.add(pos_next) } == opcodes::OP_get_ref_value {
        push_vec_u8(bc, get_op as u8);
        push_vec_u16(bc, var_idx as u16);
        pos_next += 1;
    }

    let label_pos = label.pos as usize;
    let mut pos = label_pos - 5;
    debug_assert_eq!(unsafe { *bc_buf.add(pos) }, opcodes::op_label);

    /* label points to an instruction pair:
      - insert3 / put_ref_value
      - perm4 / put_ref_value
      - rot3l / put_ref_value
      - nop / put_ref_value
    */
    let end_pos = label_pos + 2;

    unsafe {
        if *bc_buf.add(label_pos) == opcodes::OP_insert3 {
            *bc_buf.add(pos) = opcodes::OP_dup;
            pos += 1;
        }

        *bc_buf.add(pos) = get_op as u8 + 1;
        write_to_ptr::<u16>(bc_buf.add(pos + 1), var_idx as u16);
    }

    pos += 3;
    // pad with OP_nop
    while pos < end_pos {
        unsafe {
            *bc_buf.add(pos) = opcodes::OP_nop;
        }
        pos += 1;
    }

    pos_next
}

pub fn optimize_scope_make_global_ref(
    ctx: &JSContext,
    js_mode: u8,
    bc: &mut Vec<u8>,
    bc_buf: *mut u8,
    label: &LabelSlot,
    mut pos_next: u32,
    var_name: JSAtom,
) -> u32 {
    let is_strict = js_mode & JS_MODE_STRICT as u8 != 0;
    if is_strict {
        push_vec_u8(bc, opcodes::OP_check_var);
        push_vec_u32(bc, var_name.dup(ctx.rt()).0);
    } else {
        // XXX: need 2 extra OP_true if destructuring an array
    }

    if unsafe { *bc_buf.add(pos_next as usize) } == opcodes::OP_get_ref_value {
        push_vec_u8(bc, opcodes::OP_get_var);
        push_vec_u32(bc, var_name.dup(ctx.rt()).0);
        pos_next += 1;
    }

    let label_pos = label.pos as usize;
    let end_pos = label_pos + 2;
    let mut pos = label_pos - 5;
    debug_assert_eq!(unsafe { *bc_buf.add(pos) }, opcodes::op_label);

    let op = unsafe { *bc_buf.add(label_pos) };
    if is_strict {
        if op != opcodes::OP_nop {
            unsafe {
                *bc_buf.add(pos) = match op {
                    opcodes::OP_insert3 => opcodes::OP_insert2,
                    opcodes::OP_perm4 => opcodes::OP_perm3,
                    opcodes::OP_rot3l => opcodes::OP_swap,
                    _ => unreachable!(),
                };
            }
            pos += 1;
        }
    } else {
        if op == opcodes::OP_insert3 {
            unsafe {
                *bc_buf.add(pos) = opcodes::OP_dup;
            }
            pos += 1;
        }
    }

    unsafe {
        *bc_buf.add(pos) = if is_strict {
            opcodes::OP_put_var_strict
        } else {
            opcodes::OP_put_var
        };
        write_to_ptr::<u32>(bc_buf.add(pos + 1), var_name.dup(ctx.rt()).0);
    }
    pos += 5;

    while pos < end_pos {
        // fill with nop as placeholder
        unsafe {
            *bc_buf.add(pos) = opcodes::OP_nop;
        }
        pos += 1;
    }

    pos_next
}
