use crate::{
    atom::{JS_FreeAtom, JSAtom},
    bytecode::{OpCode, SpecialObject, opcode_info_intermediate, opcodes},
    context::{JS_MODE_STRICT, JS_MODE_STRIP, JSContext},
    function::{CodeMatchState, JSFunctionDef, skip_dead_code},
    utils::{
        M2, M3, M4, push_vec_short_code, push_vec_short_int, push_vec_slice, push_vec_u8,
        push_vec_u16, push_vec_u32, read_from_ptr, write_to_ptr,
    },
};

use super::optimize::OPTIMIZE;

#[derive(Debug, Copy, Clone)]
enum ResolveLabelsState {
    NoChange,
    HasLabel {
        label: u32,
        op: u8,
    },
    HasGoto {
        label: u32,
        op: u8,
    },
    HasConstantTest {
        code_context: CodeMatchState,
        val: bool,
    },
    Fail,
}

impl JSFunctionDef {
    /// pass 3: peephole optimizations and resolve goto/labels.
    pub fn resolve_labels(&mut self, ctx: &JSContext) -> Result<(), i32> {
        let bc_ptr = self.byte_code.as_ptr();
        let bc_len = self.byte_code.len();

        // new buf to generate bytecode
        let mut bc_out = Vec::<u8>::new();
        bc_out.try_reserve_exact(bc_len).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        #[cfg(feature = "short_opcode")]
        if self.jump_slots_cap != 0 {
            if self
                .jump_slots
                .try_reserve_exact(self.jump_slots_cap)
                .is_err()
            {
                self.ctx.JS_ThrowOutOfMemory();
                return -1;
            }
        }

        // XXX: Should skip this phase if not generating SHORT_OPCODES
        if self.line_number_slots_cap != 0 && self.attrs.js_mode & JS_MODE_STRIP as u8 == 0 {
            self.line_number_slots
                .try_reserve(self.line_number_slots_cap)
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

            self.line_number_last = self.line_num;
            self.line_number_last_pc = 0;
        }

        // initialize the 'home_object' variable if needed
        if let Some(vidx) = self.home_object_var_idx {
            push_vec_u8(&mut bc_out, OpCode::SPECIAL_OBJECT as u8);
            push_vec_u8(&mut bc_out, SpecialObject::HomeObject as _);
            push_vec_short_code(&mut bc_out, OpCode::PUT_LOC as u8, vidx);
        }

        // initialize the 'this.active_func' variable if needed
        if let Some(vidx) = self.this_active_func_var_idx {
            push_vec_u8(&mut bc_out, OpCode::SPECIAL_OBJECT as u8);
            push_vec_u8(&mut bc_out, SpecialObject::ThisFunc as _);
            push_vec_short_code(&mut bc_out, OpCode::PUT_LOC as u8, vidx);
        }

        // initialize the 'new.target' variable if needed
        if let Some(vidx) = self.new_target_var_idx {
            push_vec_u8(&mut bc_out, OpCode::SPECIAL_OBJECT as u8);
            push_vec_u8(&mut bc_out, SpecialObject::NewTarget as _);
            push_vec_short_code(&mut bc_out, OpCode::PUT_LOC as u8, vidx);
        }

        // initialize the 'this' variable if needed. In a derived class
        // constructor, this is initially uninitialized.
        if let Some(vidx) = self.this_var_idx {
            if self.attrs.is_derived_class_constructor {
                push_vec_u8(&mut bc_out, OpCode::SET_LOC_UNINITIALIZED as u8);
                push_vec_u16(&mut bc_out, vidx);
            } else {
                push_vec_u8(&mut bc_out, OpCode::PUSH_THIS as u8);
                push_vec_short_code(&mut bc_out, OpCode::PUT_LOC as u8, vidx);
            }
        }

        // initialize the 'arguments' variable if needed
        if let Some(vidx) = self.arguments_var_idx {
            if self.attrs.js_mode & JS_MODE_STRICT as u8 != 0
                || !self.attrs.has_simple_parameter_list
            {
                push_vec_u8(&mut bc_out, OpCode::SPECIAL_OBJECT as u8);
                push_vec_u8(&mut bc_out, SpecialObject::Arguments as _);
            } else {
                push_vec_u8(&mut bc_out, OpCode::SPECIAL_OBJECT as u8);
                push_vec_u8(&mut bc_out, SpecialObject::MappedArguments as _);
            }
            if let Some(aidx) = self.arguments_arg_idx {
                push_vec_short_code(&mut bc_out, OpCode::SET_LOC as u8, aidx);
            }
            push_vec_short_code(&mut bc_out, OpCode::PUT_LOC as u8, vidx);
        }

        // initialize a reference to the current function if needed
        if let Some(vidx) = self.func_var_idx {
            push_vec_u8(&mut bc_out, OpCode::SPECIAL_OBJECT as u8);
            push_vec_u8(&mut bc_out, SpecialObject::ThisFunc as _);
            push_vec_short_code(&mut bc_out, OpCode::PUT_LOC as u8, vidx);
        }

        // initialize the variable environment object if needed
        if let Some(vidx) = self.var_object_idx {
            push_vec_u8(&mut bc_out, OpCode::SPECIAL_OBJECT as u8);
            push_vec_u8(&mut bc_out, SpecialObject::VarObject as _);
            push_vec_short_code(&mut bc_out, OpCode::PUT_LOC as u8, vidx);
        }
        if let Some(vidx) = self.arg_var_object_idx {
            push_vec_u8(&mut bc_out, OpCode::SPECIAL_OBJECT as u8);
            push_vec_u8(&mut bc_out, SpecialObject::VarObject as _);
            push_vec_short_code(&mut bc_out, OpCode::PUT_LOC as u8, vidx);
        }

        let mut label: u32;
        let mut line_num = self.line_num;
        let mut pos = 0_usize;
        let mut pos_next: usize;
        let mut op1: u8 = 0;

        while pos < bc_len {
            let op = unsafe { *bc_ptr.add(pos) };
            let oi = opcode_info_intermediate(op);
            let len = oi.size as usize;
            pos_next = pos + len;

            let mut state: Option<ResolveLabelsState> = match op {
                opcodes::op_line_num => {
                    /* line number info (for debug). We put it in a separate
                    compressed table to reduce memory usage and get better
                    performance */
                    line_num = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 1)) };
                    None
                }
                opcodes::op_label => {
                    label = unsafe { read_from_ptr::<u32>(bc_ptr.add(pos + 1)) };
                    debug_assert!(label < self.label_slots.len() as u32);

                    let ls = self.label_slots.get_mut(label as usize).unwrap();
                    debug_assert_eq!(ls.addr, -1);
                    ls.addr = bc_out.len() as _;

                    // resolve pending relocation entries
                    while let Some(re) = ls.pending_relocs.pop() {
                        let delta = ls.addr as usize - re.addr;
                        match re.size {
                            4 => unsafe {
                                write_to_ptr::<u32>(bc_out.as_mut_ptr().add(re.addr), delta as u32);
                            },
                            2 => unsafe {
                                debug_assert_eq!(delta, (delta as i16) as usize);
                                write_to_ptr::<u16>(bc_out.as_mut_ptr().add(re.addr), delta as u16);
                            },
                            1 => unsafe {
                                assert_eq!(delta, (delta as i8) as usize);
                                *bc_out.as_mut_ptr().add(re.addr) = delta as u8;
                            },
                            _ => {
                                unreachable!()
                            }
                        }
                    }

                    None
                }
                opcodes::OP_call | opcodes::OP_call_method => {
                    // detect and transform tail calls
                    let argc = unsafe { read_from_ptr::<u16>(bc_ptr.add(pos + 1)) };

                    if let Some(m) = self.code_match(pos_next, &[OpCode::RETURN as i32]) {
                        if m.line_num >= 0 {
                            line_num = m.line_num as _;
                        }
                        self.add_pc2line_info(bc_out.len() as _, line_num);
                        push_vec_short_code(&mut bc_out, op + 1, argc);
                        pos_next =
                            skip_dead_code(self, bc_ptr, bc_len, m.pos, &mut (line_num as i32));
                    } else {
                        self.add_pc2line_info(bc_out.len() as _, line_num);
                        push_vec_short_code(&mut bc_out, op, argc);
                    }
                    None
                }
                opcodes::OP_return
                | opcodes::OP_return_undef
                | opcodes::OP_return_async
                | opcodes::OP_throw
                | opcodes::OP_throw_error => {
                    pos_next =
                        skip_dead_code(self, bc_ptr, bc_len, pos_next, &mut (line_num as i32));
                    Some(ResolveLabelsState::NoChange)
                }
                opcodes::OP_goto => Some(ResolveLabelsState::HasGoto {
                    label: unsafe { read_from_ptr::<u32>(bc_ptr.add(pos + 1)) },
                    op: opcodes::OP_goto,
                }),
                opcodes::OP_gosub => {
                    label = unsafe { read_from_ptr::<u32>(bc_ptr.add(pos + 1)) };
                    // if (0 && OPTIMIZE) {
                    //     label = find_jump_target(s, label, &op1, NULL);
                    //     if (op1 == OP_ret) {
                    //         update_label(s, label, -1);
                    //         /* empty finally clause: remove gosub */
                    //         break;
                    //     }
                    // }
                    Some(ResolveLabelsState::HasGoto { label, op })
                }
                opcodes::OP_catch => Some(ResolveLabelsState::HasLabel {
                    label: unsafe { read_from_ptr::<u32>(bc_ptr.add(pos + 1)) },
                    op,
                }),
                opcodes::OP_if_true | opcodes::OP_if_false => {
                    if OPTIMIZE {
                        self.optimize_if_true_false(
                            op,
                            pos,
                            &mut pos_next,
                            &mut line_num,
                            &mut bc_out,
                        )
                        .map(|(op1, label)| ResolveLabelsState::HasLabel { label, op: op1 })
                    } else {
                        let label = unsafe { read_from_ptr::<u32>(bc_ptr.add(pos + 1)) };
                        Some(ResolveLabelsState::HasLabel { label, op })
                    }
                }

                opcodes::OP_with_get_var
                | opcodes::OP_with_put_var
                | opcodes::OP_with_delete_var
                | opcodes::OP_with_make_ref
                | opcodes::OP_with_get_ref
                | opcodes::OP_with_get_ref_undef => {
                    let atom = unsafe { read_from_ptr::<u32>(bc_ptr.add(pos + 1)) };
                    label = unsafe { read_from_ptr::<u32>(bc_ptr.add(pos + 5)) };
                    let is_with = unsafe { *bc_ptr.add(pos + 9) };
                    if OPTIMIZE {
                        (label, op1, _) = self.find_jump_target(label);
                    }
                    debug_assert!(label < self.label_slots.len() as u32);

                    self.add_pc2line_info(bc_out.len() as _, line_num);

                    #[cfg(feature = "short_opcode")]
                    self.jump_slots.push(super::JumpSlot {
                        op,
                        size: 4,
                        pos: bc_out.size + 1,
                        label,
                    });

                    push_vec_u8(&mut bc_out, op);
                    push_vec_u32(&mut bc_out, atom);

                    let lb = self.label_slots.get_mut(label as usize).unwrap();
                    let delta = (*lb).addr as i32 - bc_out.len() as i32;
                    push_vec_u32(&mut bc_out, delta as u32);

                    let mut goto = None;
                    if lb.addr == -1 {
                        // unresolved yet: create a new relocation entry
                        if lb.add_reloc(ctx, bc_out.len() - 4, 4).is_err() {
                            goto = Some(ResolveLabelsState::Fail);
                        }
                    }
                    push_vec_u8(&mut bc_out, is_with);
                    goto
                }

                opcodes::OP_drop => {
                    let mut goto = Some(ResolveLabelsState::NoChange);
                    if OPTIMIZE {
                        /* remove useless drops before return */
                        if let Some(m) =
                            self.code_match(pos_next, &[opcodes::OP_return_undef as i32])
                        {
                            if m.line_num >= 0 {
                                line_num = m.line_num;
                            }
                            goto = None;
                        }
                    }
                    goto
                }

                opcodes::OP_null | opcodes::OP_push_false | opcodes::OP_push_true => {
                    let mut goto = Some(ResolveLabelsState::NoChange);

                    #[cfg(feature = "short_opcode")]
                    {
                        // FIXME: XXX
                        if op == opcodes::OP_null {
                            if OPTIMIZE {
                                if cc.code_match(pos_next, OP_strict_eq, -1) {
                                    if cc.line_num >= 0 {
                                        line_num = cc.line_num;
                                    }
                                    self.add_pc2line_info(bc_out.len() as _, line_num);
                                    push_vec_u8(&mut bc_out, OP_is_null);
                                    pos_next = cc.pos;
                                    todo("break");
                                }
                                if cc.code_match(
                                    pos_next,
                                    OP_strict_neq,
                                    M2(OP_if_false, OP_if_true),
                                    -1,
                                ) {
                                    if cc.line_num >= 0 {
                                        line_num = cc.line_num;
                                    }
                                    self.add_pc2line_info(bc_out.len() as _, line_num);
                                    push_vec_u8(&mut bc_out, OP_is_null);
                                    pos_next = cc.pos;
                                    label = cc.label;
                                    op ^= OP_if_true ^ OP_if_false;
                                    // C2RS: Goto translated to loop control logic
                                    todo!(continue);
                                }
                            }
                        }
                    }

                    if OPTIMIZE {
                        if let Some(cc) = self.code_match(
                            pos_next,
                            &[M2(opcodes::OP_if_false as i32, opcodes::OP_if_true as i32)],
                        ) {
                            goto = Some(ResolveLabelsState::HasConstantTest {
                                code_context: cc,
                                val: op == opcodes::OP_push_true,
                            });
                        }
                    }
                    goto
                }
                opcodes::OP_push_i32 => {
                    let mut goto = Some(ResolveLabelsState::NoChange);

                    if OPTIMIZE {
                        let val = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 1)) };
                        if val != i32::MIN
                            && val != 0
                            && let Some(cc) = self.code_match(pos_next, &[OpCode::NEG as i32])
                        {
                            // transform i32(val) neg -> i32(-val)
                            if cc.line_num >= 0 {
                                line_num = cc.line_num as _;
                            }
                            if let Some(m1) = self.code_match(cc.pos, &[OpCode::DROP as i32]) {
                                if m1.line_num >= 0 {
                                    line_num = m1.line_num as _;
                                }
                            } else {
                                self.add_pc2line_info(bc_out.len() as _, line_num);
                                push_vec_short_int(&mut bc_out, -val);
                            }
                            pos_next = cc.pos;
                            goto.take(); // cancel process NoChange, continue with updated pos_next
                        } else if let Some(cc) = self.code_match(pos_next, &[OpCode::DROP as i32]) {
                            // remove push/drop pairs generated by the parser
                            if cc.line_num >= 0 {
                                line_num = cc.line_num as _;
                            }
                            pos_next = cc.pos;
                            goto.take(); // cancel process NoChange, continue with updated pos_next
                        } else if let Some(cc) = self.code_match(
                            pos_next,
                            &[M2(OpCode::IF_FALSE as i32, OpCode::IF_TRUE as i32)],
                        ) {
                            // Optimize constant tests: `if (0)`, `if (1)`, `if (!0)`...
                            goto = Some(ResolveLabelsState::HasConstantTest {
                                code_context: cc,
                                val: val != 0,
                            });
                        } else {
                            self.add_pc2line_info(bc_out.len() as _, line_num);
                            push_vec_short_int(&mut bc_out, val);
                            goto.take(); // cancel goto NoChange
                        }
                    }

                    goto
                }

                #[cfg(feature = "short_opcode")]
                opcodes::OP_push_const | opcodes::OP_fclosure => {
                    let mut goto = Some(ResolveLabelsState::NoChange);
                    if (OPTIMIZE) {
                        let idx = read_from_ptr::<u32>(bc_ptr + pos + 1);
                        if (idx < 256) {
                            self.add_pc2line_info(bc_out.len() as _, line_num);
                            dbuf_putc(&bc_out, OP_push_const8 + op - OP_push_const);
                            dbuf_putc(&bc_out, idx);
                            goto.take();
                        }
                    }
                    goto
                }
                #[cfg(feature = "short_opcode")]
                opcodes::OP_get_field => {
                    let mut goto = Some(ResolveLabelsState::NoChange);
                    if (OPTIMIZE) {
                        let atom = read_from_ptr::<u32>(bc_ptr + pos + 1);
                        if (atom == JS_ATOM_length) {
                            JS_FreeAtom(ctx, atom);
                            self.add_pc2line_info(bc_out.len() as _, line_num);
                            dbuf_putc(&bc_out, OP_get_length);
                            goto.take();
                        }
                    }
                    goto
                }

                opcodes::OP_push_atom_value => {
                    if OPTIMIZE {
                        let atom = JSAtom(unsafe { read_from_ptr::<u32>(bc_ptr.add(pos + 1)) });

                        // remove push/drop pairs generated by the parser
                        if let Some(cc) = self.code_match(pos_next, &[OpCode::DROP as i32]) {
                            JS_FreeAtom(ctx, atom);
                            if cc.line_num >= 0 {
                                line_num = cc.line_num;
                            }
                            pos_next = cc.pos;
                            None
                        } else {
                            #[cfg(feature = "short_opcode")]
                            if atom == JS_ATOM_empty_string {
                                JS_FreeAtom(ctx, atom);
                                self.add_pc2line_info(bc_out.size, line_num);
                                dbuf_putc(&bc_out, OP_push_empty_string);
                                goto.take(); // cancel goto no_change
                            }

                            Some(ResolveLabelsState::NoChange)
                        }
                    } else {
                        Some(ResolveLabelsState::NoChange)
                    }
                }
                opcodes::OP_to_propkey | opcodes::OP_to_propkey2 => {
                    let mut goto = Some(ResolveLabelsState::NoChange);
                    if OPTIMIZE {
                        // remove redundant to_propkey/to_propkey2 opcodes when storing simple data
                        if self
                            .code_match(
                                pos_next,
                                &[
                                    M3(
                                        OpCode::GET_LOC as _,
                                        OpCode::GET_ARG as _,
                                        OpCode::GET_VAR_REF as _,
                                    ),
                                    -1,
                                    OpCode::PUT_ARRAY_EL as _,
                                ],
                            )
                            .is_some()
                            || self
                                .code_match(
                                    pos_next,
                                    &[
                                        M3(
                                            OpCode::PUSH_I32 as _,
                                            OpCode::PUSH_CONST as _,
                                            OpCode::PUSH_ATOM_VALUE as _,
                                        ),
                                        OpCode::PUT_ARRAY_EL as _,
                                    ],
                                )
                                .is_some()
                            || self
                                .code_match(
                                    pos_next,
                                    &[
                                        M4(
                                            OpCode::UNDEFINED as _,
                                            OpCode::NULL as _,
                                            OpCode::PUSH_TRUE as _,
                                            OpCode::PUSH_FALSE as _,
                                        ),
                                        OpCode::PUT_ARRAY_EL as _,
                                    ],
                                )
                                .is_some()
                        {
                            goto.take();
                        }
                    }
                    goto
                }
                opcodes::OP_undefined => {
                    let mut goto = Some(ResolveLabelsState::NoChange);
                    if OPTIMIZE {
                        // remove push/drop pairs generated by the parser
                        if let Some(cc) = self.code_match(pos_next, &[OpCode::DROP as _]) {
                            if cc.line_num >= 0 {
                                line_num = cc.line_num as _;
                            }
                            pos_next = cc.pos;
                            goto.take();
                        } else if let Some(cc) = self.code_match(pos_next, &[OpCode::RETURN as _]) {
                            // transform undefined return -> return_undefined
                            if cc.line_num >= 0 {
                                line_num = cc.line_num as _;
                            }
                            self.add_pc2line_info(bc_out.len() as _, line_num);
                            push_vec_u8(&mut bc_out, OpCode::RETURN_UNDEF as _);
                            pos_next = cc.pos;
                            goto.take();
                        } else if let Some(cc) = self.code_match(
                            pos_next,
                            &[M2(OpCode::IF_FALSE as _, OpCode::IF_TRUE as _)],
                        ) {
                            // transform undefined if_true(l1)/if_false(l1) -> nop/goto(l1)
                            goto = Some(ResolveLabelsState::HasConstantTest {
                                code_context: cc,
                                val: false,
                            });
                        }

                        #[cfg(feature = "short_opcode")]
                        {
                            // transform undefined strict_eq -> is_undefined
                            if let Some(cc) = self.code_match(pos_next, &[OpCode::STRICT_EQ as _]) {
                                if cc.line_num >= 0 {
                                    line_num = cc.line_num as _;
                                }
                                self.add_pc2line_info(bc_out.size, line_num);
                                push_vec_u8(&mut bc_out, OpCode::IS_UNDEFINED as _);
                                pos_next = cc.pos;
                                goto.take();
                            } else if let Some(cc) = self.code_match(
                                pos_next,
                                &[
                                    OpCode::STRICT_NEQ as _,
                                    M2(OpCode::IF_FALSE as _, OpCode::IF_TRUE as _),
                                ],
                            ) {
                                // transform undefined strict_neq if_false/if_true -> is_undefined if_true/if_false
                                if cc.line_num >= 0 {
                                    line_num = cc.line_num;
                                }
                                self.add_pc2line_info(bc_out.size, line_num);
                                push_vec_u8(&mut bc_out, OpCode::IS_UNDEFINED as _);
                                pos_next = cc.pos;
                                op = cc.op ^ OP_if_false ^ OP_if_true;
                                goto = Some(ResolveLabelsState::HasLabel(cc.label));
                            }
                        }
                    }
                    goto
                }
                opcodes::OP_insert2 => {
                    let mut goto = Some(ResolveLabelsState::NoChange);
                    if OPTIMIZE {
                        // Transformation:
                        // insert2 put_field(a) drop -> put_field(a)
                        // insert2 put_var_strict(a) drop -> put_var_strict(a)
                        if let Some(cc) = self.code_match(
                            pos_next,
                            &[
                                M2(OpCode::PUT_FIELD as _, OpCode::PUT_VAR_STRICT as _),
                                OpCode::DROP as _,
                            ],
                        ) {
                            if cc.line_num >= 0 {
                                line_num = cc.line_num as _;
                            }
                            self.add_pc2line_info(bc_out.len() as _, line_num);
                            push_vec_u8(&mut bc_out, cc.op as u8);
                            push_vec_u32(&mut bc_out, cc.atom.0);
                            pos_next = cc.pos;
                            goto.take();
                        }
                    }
                    goto
                }
                opcodes::OP_dup => {
                    if OPTIMIZE && self.optimize_dup(pos, &mut pos_next, &mut line_num, &mut bc_out)
                    {
                        None
                    } else {
                        Some(ResolveLabelsState::NoChange)
                    }
                }
                opcodes::OP_get_loc => {
                    if OPTIMIZE
                        && self.optimize_get_loc(pos, &mut pos_next, &mut line_num, &mut bc_out)
                    {
                        None
                    } else {
                        Some(ResolveLabelsState::NoChange)
                    }
                }

                #[cfg(feature = "short_opcode")]
                opcodes::OP_get_arg | opcodes::OP_get_var_ref => {
                    unimplemented!()
                }

                opcodes::OP_put_loc | opcodes::OP_put_arg | opcodes::OP_put_var_ref => {
                    if OPTIMIZE {
                        self.optimize_put_loc(op, pos, &mut pos_next, &mut line_num, &mut bc_out);
                        None
                    } else {
                        Some(ResolveLabelsState::NoChange)
                    }
                }
                opcodes::OP_post_inc | opcodes::OP_post_dec => {
                    if OPTIMIZE
                        && self.optimize_post_dec_inc(
                            op,
                            pos,
                            &mut pos_next,
                            &mut line_num,
                            &mut bc_out,
                        )
                    {
                        None
                    } else {
                        Some(ResolveLabelsState::NoChange)
                    }
                }

                #[cfg(feature = "short_opcode")]
                opcodes::OP_typeof => {
                    unimplemented!()
                }

                _ => Some(ResolveLabelsState::NoChange),
            }; // match op

            // dispatch state
            while let Some(p) = state.take() {
                match p {
                    ResolveLabelsState::NoChange => {
                        self.add_pc2line_info(bc_out.len() as _, line_num);
                        push_vec_slice(&mut bc_out, unsafe { bc_ptr.add(pos) }, len)?;
                    }
                    ResolveLabelsState::HasGoto {
                        mut label,
                        op: goto_op,
                    } => {
                        if OPTIMIZE {
                            // Use custom matcher because multiple labels can follow
                            let line1;
                            (label, op1, line1) = self.find_jump_target(label);

                            if self.code_has_label(pos_next as usize, label) {
                                /* jump to next instruction: remove jump */
                                self.update_label_ref_count(label, -1);
                            } else if matches!(
                                op1,
                                opcodes::OP_return | opcodes::OP_return_undef | opcodes::OP_throw
                            ) {
                                /* jump to return/throw: remove jump, append return/throw */
                                /* updating the line number obfuscates assembly listing */
                                //if (line1 >= 0) line_num = line1;

                                self.update_label_ref_count(label, -1);
                                self.add_pc2line_info(bc_out.len() as _, line_num);
                                push_vec_u8(&mut bc_out, op1);
                                pos_next = skip_dead_code(
                                    self,
                                    bc_ptr,
                                    bc_len,
                                    pos_next,
                                    &mut (line_num as i32),
                                );
                            } else {
                                /* XXX: should duplicate single instructions followed by goto or return */
                                /* For example, can match one of these followed by return:
                                  push_i32 / push_const / push_atom_value / get_var /
                                  undefined / null / push_false / push_true / get_ref_value /
                                  get_loc / get_arg / get_var_ref
                                */
                                state = Some(ResolveLabelsState::HasLabel { label, op: goto_op });
                            }
                        } else {
                            state = Some(ResolveLabelsState::HasLabel { label, op: goto_op });
                        }
                    }
                    ResolveLabelsState::HasLabel {
                        label,
                        op: label_op,
                    } => {
                        debug_assert!(label < self.label_slots.len() as u32);

                        self.add_pc2line_info(bc_out.len() as _, line_num);
                        if label_op == opcodes::OP_goto {
                            pos_next = skip_dead_code(
                                self,
                                bc_ptr,
                                bc_len,
                                pos_next,
                                &mut (line_num as i32),
                            );
                        }
                        let ls = self.label_slots.get_mut(label as usize).unwrap();

                        #[cfg(feature = "short_opcode")]
                        {
                            let mut jp = super::JumpSlot {
                                op: label_op,
                                size: 4,
                                pos: bc_out.len() + 1,
                                label,
                            };

                            if (*ls).addr == -1 {
                                let diff = (*ls).pos2 - pos as i32 - 1;
                                if diff < 128
                                    && matches!(
                                        label_op,
                                        opcodes::OP_if_false
                                            | opcodes::OP_if_true
                                            | opcodes::OP_goto
                                    )
                                {
                                    jp.size = 1;
                                    jp.op =
                                        opcodes::OP_if_false8 + (label_op - opcodes::OP_if_false);
                                    push_vec_u8(
                                        &mut bc_out,
                                        opcodes::OP_if_false8 + (label_op - opcodes::OP_if_false),
                                    );
                                    push_vec_u8(&mut bc_out, 0);
                                    if ls.add_reloc(ctx, bc_out.len() - 1, 1).is_err() {
                                        state = Some(ResolveLabelsState::Fail);
                                    }
                                } else if (diff < 32768 && label_op == OP_goto) {
                                    jp.size = 2;
                                    jp.op = OP_goto16;
                                    push_vec_u8(&mut bc_out, opcodes::OP_goto16);
                                    push_vec_u16(&mut bc_out, 0);
                                    if ls.add_reloc(ctx, bc_out.len() - 2, 2).is_err() {
                                        state = Some(ResolveLabelsState::Fail);
                                    }
                                }
                            } else {
                                let diff = (*ls).addr - bc_out.len() as i32 - 1;
                                if diff == diff as u8 as i32
                                    && matches!(
                                        label_op,
                                        opcodes::OP_if_false
                                            | opcodes::OP_if_true
                                            | opcodes::OP_goto
                                    )
                                {
                                    jp.size = 1;
                                    jp.op = OP_if_false8 + (label_op - OP_if_false);
                                    push_vec_u8(
                                        &mut bc_out,
                                        opcodes::OP_if_false8 + (label_op - opcodes::OP_if_false),
                                    );
                                    push_vec_u8(&mut bc_out, diff as u8);
                                } else if diff == diff as u16 as i32 && label_op == opcodes::OP_goto
                                {
                                    jp.size = 2;
                                    jp.op = OP_goto16;
                                    push_vec_u8(&mut bc_out, opcodes::OP_goto16);
                                    push_vec_u16(&mut bc_out, diff as u16);
                                }
                            }

                            self.jump_slots.push(jp);
                        }

                        push_vec_u8(&mut bc_out, label_op);
                        let delta = ls.addr as i32 - bc_out.len() as i32; // could be minus
                        push_vec_u32(&mut bc_out, delta as u32);

                        if ls.addr == -1 {
                            // unresolved yet: create a new relocation entry
                            if ls.add_reloc(ctx, bc_out.len() - 4, 4).is_err() {
                                state = Some(ResolveLabelsState::Fail);
                            }
                        }
                    }
                    ResolveLabelsState::HasConstantTest {
                        code_context: cc,
                        val,
                    } => {
                        debug_assert!(matches!(
                            cc.op as u8,
                            opcodes::OP_if_true | opcodes::OP_if_false
                        ));

                        if cc.line_num >= 0 {
                            line_num = cc.line_num as _;
                        }
                        match (cc.op as u8, val) {
                            (opcodes::OP_if_true, true) | (opcodes::OP_if_false, false) => {
                                // transform null|false if_false(l1) -> goto l1
                                // transform true if_true(l1) -> goto l1 */
                                pos_next = cc.pos;
                                state = Some(ResolveLabelsState::HasGoto {
                                    label: cc.label as _,
                                    op: opcodes::OP_goto,
                                });
                            }

                            _ => {
                                // transform null|false if_true(l1) -> nop
                                // transform true if_false(l1) -> nop
                                pos_next = cc.pos;
                                self.update_label_ref_count(cc.label as _, -1);
                            }
                        }
                    }
                    ResolveLabelsState::Fail => {
                        drop(bc_out);
                        return Err(-1);
                    }
                }
            }

            pos = pos_next;
        } // while pos < bc_len

        // check that there were no missing labels
        #[cfg(debug_assertions)]
        for lb in self.label_slots.iter() {
            debug_assert!(lb.pending_relocs.is_empty());
        }

        #[cfg(feature = "short_opcode")]
        {
            if OPTIMIZE {
                let mut patch_offsets = 0;
                for jp in self.jump_slots.iter_mut() {
                    let mut delta = 3;
                    let mut op = jp.op;
                    match op {
                        OP_goto16 => {
                            delta = 1;
                            // fall thru
                        }
                        OP_if_false | OP_if_true | OP_goto => {
                            let pos = jp.pos;
                            let diff = (*self.label_slots.offset(jp.label as isize)).addr - pos;
                            if diff >= -128 && diff <= 127 + delta {
                                jp.size = 1;
                                if op == OP_goto16 {
                                    *bc_out.buf.offset(pos - 1) = OP_goto8;
                                } else {
                                    *bc_out.buf.offset(pos - 1) = OP_if_false8 + (op - OP_if_false);
                                }
                                // C2RS: memmove translated to copy_within
                                std::ptr::copy(
                                    bc_out.buf.offset(pos + (*jp).size as isize),
                                    bc_out.buf.offset(pos + (*jp).size as isize + delta),
                                    (bc_out.size - pos - jp.size - delta) as usize,
                                );
                                bc_out.size -= delta;
                                patch_offsets += 1;
                                for j in 0..self.label_count {
                                    let ls = self.label_slots.offset(j as isize);
                                    if (*ls).addr > pos {
                                        (*ls).addr -= delta;
                                    }
                                }

                                for j in i + 1..self.jump_count {
                                    let jp1 = self.jump_slots.offset(j as isize);
                                    if (*jp1).pos > pos {
                                        (*jp1).pos -= delta;
                                    }
                                }
                                for j in 0..self.line_number_count {
                                    if (*self.line_number_slots.offset(j as isize)).pc > pos {
                                        (*self.line_number_slots.offset(j as isize)).pc -= delta;
                                    }
                                }
                                continue;
                            } else if diff == diff as i32 && op == OP_goto {
                                jp.size = 2;
                                delta = 2;
                                *bc_out.buf.offset(pos - 1) = OP_goto16;
                                // C2RS: memmove translated to copy_within
                                std::ptr::copy(
                                    bc_out.buf.offset(pos + jp.size as isize),
                                    bc_out.buf.offset(pos + jp.size as isize + delta),
                                    (bc_out.size - pos - jp.size - delta) as usize,
                                );
                                bc_out.size -= delta;
                                patch_offsets += 1;
                                for j in 0..self.label_count {
                                    let ls = self.label_slots.offset(j as isize);
                                    if (*ls).addr > pos {
                                        (*ls).addr -= delta;
                                    }
                                }
                                for j in i + 1..self.jump_count {
                                    let jp1 = self.jump_slots.offset(j as isize);
                                    if (*jp1).pos > pos {
                                        (*jp1).pos -= delta;
                                    }
                                }
                                for j in 0..self.line_number_count {
                                    if (*self.line_number_slots.offset(j as isize)).pc > pos {
                                        (*self.line_number_slots.offset(j as isize)).pc -= delta;
                                    }
                                }
                                continue;
                            }
                            break;
                        }
                        _ => {}
                    }
                }
                if patch_offsets != 0 {
                    for i in 0..self.jump_count {
                        let jp1 = self.jump_slots.offset(i as isize);
                        let diff1 =
                            (*self.label_slots.offset((*jp1).label as isize)).addr - (*jp1).pos;
                        match (*jp1).size {
                            1 => *bc_out.buf.offset((*jp1).pos as isize) = diff1 as u8,
                            2 => write_to_ptr::<u16>(
                                bc_out.buf.offset((*jp1).pos as isize),
                                diff1 as u16,
                            ),
                            4 => write_to_ptr::<u32>(
                                bc_out.buf.offset((*jp1).pos as isize),
                                diff1 as u32,
                            ),
                            _ => {}
                        }
                    }
                }
            }

            self.jump_slots.clear();
        }

        // XXX: should delay until copying to runtime bytecode function /
        self.compute_pc2line_info();

        self.label_slots.clear();
        self.line_number_slots.clear();

        // set the new byte code
        bc_out.shrink_to_fit();
        self.byte_code = bc_out;
        self.use_short_opcodes = true;

        Ok(())
    }
}
