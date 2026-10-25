// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpcodeFormat, opcode_info_final, opcode_info_intermediate, opcodes},
    function::{
        FunctionDef, Label, push_vec_atom, push_vec_i32, push_vec_op_u16, push_vec_u8, push_vec_u16,
    },
    utils::{M2, M3, read_from_ptr, write_to_ptr},
};

use Pat::{Any, Op, Val};

pub const OPTIMIZE: bool = true;

/// One element of a [`FunctionDef::code_match`] pattern.
///
/// A pattern element is either an opcode or one operand slot. How many operand
/// slots an opcode owns is a property of its *format*
/// (`opcode_info_intermediate(...).fmt`), not of the pattern, so the pattern
/// has to spell them out to stay in sync with the opcode table.
///
/// This used to be a plain `&[i32]` where `-1` meant both "end of pattern" and
/// "any operand", so an operand slot and a small opcode value shared one
/// namespace: a pattern that forgot a slot silently compared the operand
/// against the opcode meant to follow it, and matched *wrongly* when the two
/// happened to be equal. `inline_cache` gave `put_array_el` a `u16` operand
/// that way, which corrupted `insert3 put_array_el drop` (memo/bytecode-bugs.md
/// §2.6). With the typed pattern a stale pattern is a structural error instead:
/// `code_match` reports no match (and debug-asserts), so it loses an
/// optimization rather than emitting wrong bytecode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pat {
    /// An opcode, or a set of alternatives packed with `M2`/`M3`/`M4`.
    Op(i32),
    /// One operand slot, any value.
    Any,
    /// One operand slot that must hold exactly this value.
    Val(i32),
}

/// Operand slots carried by `op`'s format.
fn slots_of(op: u8) -> usize {
    match opcode_info_intermediate(op).fmt {
        OpcodeFormat::LOC8
        | OpcodeFormat::U8
        | OpcodeFormat::U16
        | OpcodeFormat::NPOP
        | OpcodeFormat::LOC
        | OpcodeFormat::ARG
        | OpcodeFormat::VAR_REF => 1,
        _ => 0,
    }
}

/// Operand slots carried by a pattern opcode, unpacking `M2`/`M3`/`M4`
/// alternatives. `None` when the alternatives disagree on the count (then the
/// pattern cannot be written unambiguously) or when `op` is not an opcode.
fn pat_slot_count(packed: i32) -> Option<usize> {
    let packed = packed as u32;
    let mut slots = None;
    for shift in [0, 8, 16, 24] {
        let alt = (packed >> shift) as u8;
        // Opcodes are never 0, so a zero byte ends a packed set.
        if shift != 0 && alt == 0 {
            break;
        }
        let n = slots_of(alt);
        match slots {
            None => slots = Some(n),
            Some(prev) if prev != n => return None,
            _ => {}
        }
        if packed >> 8 == 0 {
            break;
        }
    }
    slots
}

/// Whether every operand slot `pattern` spells out holds an operand.
///
/// A pattern may end in the middle of the last opcode's operand slots (the rest
/// are wildcards), but it must never put an opcode where a slot belongs — that
/// is the shape that silently compared an operand against the opcode meant to
/// follow it. See `Pat`.
///
/// The check is not `const` because patterns carry runtime operand values
/// (`Pat::Val(idx)`), so `code_match` debug-asserts it; CI's debug test runs
/// turn a stale pattern into a loud failure, and in release a malformed pattern
/// simply never matches.
fn pattern_is_well_formed(pattern: &[Pat]) -> bool {
    let mut i = 0;
    while i < pattern.len() {
        let Pat::Op(packed) = pattern[i] else {
            return false;
        };
        i += 1;
        let Some(slots) = pat_slot_count(packed) else {
            return false;
        };
        for _ in 0..slots {
            if i == pattern.len() {
                break;
            }
            if !matches!(pattern[i], Pat::Any | Pat::Val(_)) {
                return false;
            }
            i += 1;
        }
    }
    true
}

/// Match one operand slot: `Any` accepts anything (`Some(true)` tells the caller
/// to record the value), `Val(n)` must equal `val`. A pattern that ends here
/// accepts anything too, as an implicit wildcard.
///
/// Anything else — a different value, or an opcode where an operand belongs —
/// mismatches, so a stale pattern loses its optimization instead of matching
/// wrongly.
fn take_operand(it: &mut std::slice::Iter<'_, Pat>, val: i32) -> Option<bool> {
    match it.next() {
        Some(Pat::Any) | None => Some(true),
        Some(Pat::Val(n)) if *n == val => Some(false),
        _ => None,
    }
}

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
    pub(super) fn code_match(&self, mut pos: usize, pattern: &[Pat]) -> Option<CodeMatchState> {
        debug_assert!(
            pattern_is_well_formed(pattern),
            "code_match pattern does not line up with the opcode formats: {pattern:?}"
        );

        let bc_ptr = self.byte_code.as_ptr();
        let bc_len = self.byte_code.len();

        let mut state = CodeMatchState::default();
        let mut line_num = -1;
        let mut pos_next: usize;
        let mut found = false;

        let mut it = pattern.iter();
        loop {
            let Some(&pat) = it.next() else {
                state.pos = pos;
                state.line_num = line_num;
                found = true;
                break;
            };

            // An operand slot where an opcode belongs means the pattern does not
            // line up with the opcode formats. Refuse it rather than compare the
            // slot against the opcode and match by accident.
            let Pat::Op(op_arg) = pat else {
                return None;
            };

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
                        let idx = *bc_ptr.add(pos) as i32;
                        if take_operand(&mut it, idx)? {
                            state.idx = idx;
                        }
                    }
                    OpcodeFormat::U16
                    | OpcodeFormat::NPOP
                    | OpcodeFormat::LOC
                    | OpcodeFormat::ARG
                    | OpcodeFormat::VAR_REF => {
                        let idx = read_from_ptr::<u16>(bc_ptr.add(pos)) as i32;
                        if take_operand(&mut it, idx)? {
                            state.idx = idx;
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
                        Op(M2(opcodes::OP_post_dec as _, opcodes::OP_post_inc as _)),
                        Op(opcodes::OP_put_loc as _),
                        Val(idx as _),
                        Op(opcodes::OP_drop as _),
                    ],
                )
                .or_else(|| {
                    self.code_match(
                        *pos_next,
                        &[
                            Op(M2(opcodes::OP_dec as _, opcodes::OP_inc as _)),
                            Op(opcodes::OP_dup as _),
                            Op(opcodes::OP_put_loc as _),
                            Val(idx as _),
                            Op(opcodes::OP_drop as _),
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
                    Op(opcodes::OP_push_atom_value as _),
                    Op(opcodes::OP_add as _),
                    Op(opcodes::OP_dup as _),
                    Op(opcodes::OP_put_loc as _),
                    Val(idx as _),
                    Op(opcodes::OP_drop as _),
                ],
            ) {
                // transformation:
                // get_loc(n) push_atom_value(x) add dup put_loc(n) drop -> push_atom_value(x) add_loc(n)
                if cc.line_num >= 0 {
                    *line_num = cc.line_num as _;
                }
                self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);

                push_vec_u8(bc_out, opcodes::OP_push_atom_value);
                push_vec_atom(bc_out, cc.atom);

                push_vec_u8(bc_out, opcodes::OP_add_loc);
                push_vec_u8(bc_out, idx as u8);
                *pos_next = cc.pos;
            } else if let Some(cc) = self.code_match(
                *pos_next,
                &[
                    Op(opcodes::OP_push_i32 as _),
                    Op(opcodes::OP_add as _),
                    Op(opcodes::OP_dup as _),
                    Op(opcodes::OP_put_loc as _),
                    Val(idx as _),
                    Op(opcodes::OP_drop as _),
                ],
            ) {
                // transformation:
                // get_loc(n) push_i32(x) add dup put_loc(n) drop -> push_i32(x) add_loc(n)
                if cc.line_num >= 0 {
                    *line_num = cc.line_num as _;
                }
                self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
                push_vec_i32(bc_out, cc.label);
                push_vec_u8(bc_out, opcodes::OP_add_loc);
                push_vec_u8(bc_out, idx as u8);
                *pos_next = cc.pos;
            } else if let Some(cc) = self.code_match(
                *pos_next,
                &[
                    Op(M3(
                        opcodes::OP_get_loc as _,
                        opcodes::OP_get_arg as _,
                        opcodes::OP_get_var_ref as _,
                    )),
                    Any,
                    Op(opcodes::OP_add as _),
                    Op(opcodes::OP_dup as _),
                    Op(opcodes::OP_put_loc as _),
                    Val(idx as _),
                    Op(opcodes::OP_drop as _),
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
                push_vec_op_u16(bc_out, cc.op as _, cc.idx as _);
                push_vec_u8(bc_out, opcodes::OP_add_loc);
                push_vec_u8(bc_out, idx as u8);
                *pos_next = cc.pos;
            } else {
                self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
                push_vec_op_u16(bc_out, OpCode::GET_LOC as u8, idx);
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
        if let Some(cc) = self.code_match(*pos_next, &[Op(get_op), Val(idx as _)]) {
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
            push_vec_op_u16(bc_out, set_op, idx);
            *pos_next = cc.pos;
        } else {
            self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
            push_vec_op_u16(bc_out, op, idx);
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
                Op(M3(
                    OpCode::PUT_LOC as _,
                    OpCode::PUT_ARG as _,
                    OpCode::PUT_VAR_REF as _,
                )),
                Any,
                Op(OpCode::DROP as _),
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
            if let Some(m) = self.code_match(cc.pos, &[Op(get_op), Val(idx)]) {
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
            push_vec_op_u16(bc_out, final_op, idx as u16);
            return true;
        }

        if let Some(cc) = self.code_match(
            *pos_next,
            &[
                Op(OpCode::PERM3 as _),
                Op(OpCode::PUT_FIELD as _),
                Op(OpCode::DROP as _),
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
            // NB: `PUT_FIELD` is matched exactly here, so `code_match` never
            // populates `cc.op` (it only records the op for multi-alternative
            // M2/M3 matches) — it stays 0. Emit the opcode explicitly instead of
            // `cc.op`, which would write an invalid `op=0` byte. Also preserve
            // the trailing inline-cache index that `PUT_FIELD` (ATOM_U16) carries
            // when the `inline_cache` feature is on; `code_match` reads it into
            // `cc.val`. Dropping it truncated the instruction and shifted every
            // following opcode.
            push_vec_u8(bc_out, OpCode::PUT_FIELD as u8);
            push_vec_atom(bc_out, cc.atom);
            #[cfg(feature = "inline_cache")]
            push_vec_u16(bc_out, cc.val as u16);
            *pos_next = cc.pos;
            return true;
        }

        // `put_array_el` carries a u16 inline-cache operand when the feature is
        // on, so it owns a pattern slot. `code_match` refuses a pattern that
        // leaves the slot out (see `Pat`).
        #[cfg(feature = "inline_cache")]
        let pat: &[Pat] = &[
            Op(OpCode::PERM4 as _),
            Op(OpCode::PUT_ARRAY_EL as _),
            Any,
            Op(OpCode::DROP as _),
        ];
        #[cfg(not(feature = "inline_cache"))]
        let pat: &[Pat] = &[
            Op(OpCode::PERM4 as _),
            Op(OpCode::PUT_ARRAY_EL as _),
            Op(OpCode::DROP as _),
        ];

        if let Some(cc) = self.code_match(*pos_next, pat) {
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
            // Preserve the inline-cache index the matched `put_array_el`
            // carried (`code_match` reads a U16 operand into `cc.idx`).
            #[cfg(feature = "inline_cache")]
            push_vec_u16(bc_out, cc.idx as u16);
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
        if let Some(cc) = self.code_match(*pos_next, &[Op(OpCode::GOTO as _)]) {
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
            &[
                Op(M3(
                    OpCode::PUT_LOC as _,
                    OpCode::PUT_ARG as _,
                    OpCode::PUT_VAR_REF as _,
                )),
                Any,
            ],
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

            if let Some(cc1) = self.code_match(cc.pos, &[Op(OpCode::DROP as _)]) {
                if cc1.line_num >= 0 {
                    *line_num = cc1.line_num;
                }
                final_op = put_op; // set_x drop -> put_x
                *pos_next = cc1.pos;

                if let Some(cc2) = self.code_match(cc1.pos, &[Op(get_op), Val(idx)]) {
                    line2 = cc2.line_num; // delay line number update
                    final_op = set_op; // put_x(n) get_x(n) -> set_x(n)
                    *pos_next = cc2.pos;
                }
            }

            self.add_pc2line_info(bc_out.len() as _, *line_num, *line_pos);
            push_vec_op_u16(bc_out, final_op, idx as u16);
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

#[cfg(test)]
mod tests {
    //! Guards for the peephole patterns in this module and in `resolve_vars`.

    use crate::{EvalFlags, EvalName, EvalType, Runtime, ScopedContext};

    /// Run a global script and return its completion value as an int.
    fn eval_int(src: &str) -> i32 {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        ctx.eval(
            src.as_bytes(),
            &EvalName::Name("peephole-probe.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap()
        .as_int()
        .expect("the probe scripts all evaluate to an int")
    }

    /// `n` statement-position element reads, to advance the function's
    /// inline-cache index by `n` before the site under test.
    fn filler(n: usize) -> String {
        (0..n).map(|i| format!("a[{i}];")).collect()
    }

    const SETUP: &str = "var a = []; for (var i = 0; i < 8; i++) a[i] = i; var k = 3; \
                         function id(x) { return x; }\n";

    /// A computed-element store **used as a value** must keep the result on the
    /// stack, whatever the site's inline-cache index is.
    ///
    /// `resolve_variables` folds `insert3 put_array_el drop` into `put_array_el`.
    /// Its pattern did not account for the `u16` inline-cache operand that
    /// `inline_cache` added to `put_array_el`, so the operand slot was compared
    /// against the `drop` element instead. When the index happened to equal
    /// `OP_drop`'s opcode the fold fired on the *value* form too, deleting the
    /// `insert3` and underflowing the stack at the next call (memo/bytecode-bugs.md
    /// §2.6). The index is per function and counts every element access, so
    /// sweep the filler length across the range where small operand values and
    /// opcodes overlap.
    #[test]
    fn element_store_used_as_a_value_survives_every_inline_cache_index() {
        for n in 0..64 {
            let src = format!(
                "{SETUP}{}\nvar r = id(a[k + 0] = 7);\nr === 7 && a[3] === 7 ? 1 : 0;",
                filler(n)
            );
            assert_eq!(eval_int(&src), 1, "value-form store, filler={n}");
        }
    }

    /// The same sweep for `optimize_post_dec_inc`: `a[i]++` folds
    /// `perm4 put_array_el drop` into `inc put_array_el` and carries the same
    /// operand slot, and it must keep the index intact.
    #[test]
    fn element_postfix_increment_survives_every_inline_cache_index() {
        for n in 0..64 {
            let src = format!("{SETUP}{}\na[k + 0]++;\na[3] === 4 ? 1 : 0;", filler(n));
            assert_eq!(eval_int(&src), 1, "postfix increment, filler={n}");
        }
    }
}
