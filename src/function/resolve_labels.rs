use crate::{
    atom::{JSAtom, JS_FreeAtom},
    bytecode::{opcode_info_intermediate, opcodes, OPSpecialObjectEnum, OpCode},
    context::{JSContext, JS_MODE_STRICT, JS_MODE_STRIP},
    function::{find_jump_target, skip_dead_code, JSFunctionDef},
    utils::{
        push_vec_short_code, push_vec_short_int, push_vec_slice, push_vec_u16, push_vec_u32,
        push_vec_u8, read_from_ptr, write_to_ptr, M2, M3, M4,
    },
};

use super::{
    code_has_label, compute_pc2line_info,
    optimize::{CodeContext, OPTIMIZE},
};

#[derive(Copy, Clone, PartialEq, Eq)]
enum ResolveLabelsState {
    NoChange,
    HasLabel { label: u32, op: u8 },
    HasGoto { label: u32, op: u8 },
    HasConstantTest(bool),
    Fail,
}

/// pass 3: peephole optimizations and resolve goto/labels.
pub unsafe fn resolve_labels(ctx: *mut JSContext, s: *mut JSFunctionDef) -> Result<(), i32> {
    let bc_buf = (*s).byte_code.as_ptr();
    let bc_len = (*s).byte_code.len();
    let mut bc_out = Vec::<u8>::with_capacity(bc_len);

    #[cfg(feature = "short_opcode")]
    if (*s).jump_size != 0 {
        if (*s).jump_slots.try_reserve_exact((*s).jump_size).is_err() {
            (*s).ctx.JS_ThrowOutOfMemory();
            return -1;
        }
    }

    if (*s).line_number_slots_cap != 0 && (*s).js_mode & JS_MODE_STRIP as u8 == 0 {
        if (*s)
            .line_number_slots
            .try_reserve_exact((*s).line_number_slots_cap)
            .is_err()
        {
            (*ctx).JS_ThrowOutOfMemory();
            return Err(-1);
        }

        // (*s).line_number_slots = js_mallocz(
        //     (*s).ctx.as_ptr(),
        //     std::mem::size_of::<LineNumberSlot>() * (*s).line_number_size as usize,
        // )
        // .cast();
        // if (*s).line_number_slots.is_null() {
        //     return Err(-1);
        // }

        (*s).line_number_last = (*s).line_num;
        (*s).line_number_last_pc = 0;
    }

    if let Some(vidx) = (*s).home_object_var_idx {
        push_vec_u8(&mut bc_out, opcodes::OP_special_object);
        push_vec_u8(
            &mut bc_out,
            OPSpecialObjectEnum::OP_SPECIAL_OBJECT_HOME_OBJECT as _,
        );
        push_vec_short_code(&mut bc_out, opcodes::OP_put_loc, vidx);
    }

    if let Some(vidx) = (*s).this_active_func_var_idx {
        push_vec_u8(&mut bc_out, opcodes::OP_special_object);
        push_vec_u8(
            &mut bc_out,
            OPSpecialObjectEnum::OP_SPECIAL_OBJECT_THIS_FUNC as _,
        );
        push_vec_short_code(&mut bc_out, opcodes::OP_put_loc, vidx);
    }

    if let Some(vidx) = (*s).new_target_var_idx {
        push_vec_u8(&mut bc_out, opcodes::OP_special_object);
        push_vec_u8(
            &mut bc_out,
            OPSpecialObjectEnum::OP_SPECIAL_OBJECT_NEW_TARGET as _,
        );
        push_vec_short_code(&mut bc_out, opcodes::OP_put_loc, vidx);
    }

    if let Some(vidx) = (*s).this_var_idx {
        if (*s).is_derived_class_constructor {
            push_vec_u8(&mut bc_out, opcodes::OP_set_loc_uninitialized);
            push_vec_u16(&mut bc_out, vidx);
        } else {
            push_vec_u8(&mut bc_out, opcodes::OP_push_this);
            push_vec_short_code(&mut bc_out, opcodes::OP_put_loc, vidx);
        }
    }

    if let Some(vidx) = (*s).arguments_var_idx {
        if (*s).js_mode & JS_MODE_STRICT as u8 != 0 || !(*s).has_simple_parameter_list {
            push_vec_u8(&mut bc_out, opcodes::OP_special_object);
            push_vec_u8(
                &mut bc_out,
                OPSpecialObjectEnum::OP_SPECIAL_OBJECT_ARGUMENTS as _,
            );
        } else {
            push_vec_u8(&mut bc_out, opcodes::OP_special_object);
            push_vec_u8(
                &mut bc_out,
                OPSpecialObjectEnum::OP_SPECIAL_OBJECT_MAPPED_ARGUMENTS as _,
            );
        }
        if let Some(aidx) = (*s).arguments_arg_idx {
            push_vec_short_code(&mut bc_out, opcodes::OP_set_loc, aidx);
        }
        push_vec_short_code(&mut bc_out, opcodes::OP_put_loc, vidx);
    }

    if let Some(vidx) = (*s).func_var_idx {
        push_vec_u8(&mut bc_out, opcodes::OP_special_object);
        push_vec_u8(
            &mut bc_out,
            OPSpecialObjectEnum::OP_SPECIAL_OBJECT_THIS_FUNC as _,
        );
        push_vec_short_code(&mut bc_out, opcodes::OP_put_loc, vidx);
    }

    if let Some(vidx) = (*s).var_object_idx {
        push_vec_u8(&mut bc_out, opcodes::OP_special_object);
        push_vec_u8(
            &mut bc_out,
            OPSpecialObjectEnum::OP_SPECIAL_OBJECT_VAR_OBJECT as _,
        );
        push_vec_short_code(&mut bc_out, opcodes::OP_put_loc, vidx);
    }

    if let Some(vidx) = (*s).arg_var_object_idx {
        push_vec_u8(&mut bc_out, opcodes::OP_special_object);
        push_vec_u8(
            &mut bc_out,
            OPSpecialObjectEnum::OP_SPECIAL_OBJECT_VAR_OBJECT as _,
        );
        push_vec_short_code(&mut bc_out, opcodes::OP_put_loc, vidx);
    }

    let mut cc = CodeContext::new(&(*s).byte_code);

    let mut label: u32;
    let mut line_num = (*s).line_num;
    let mut pos = 0_usize;
    let mut pos_next: usize;
    let mut op1: u8 = 0;

    while pos < bc_len {
        let op = *bc_buf.offset(pos as isize);
        let oi = opcode_info_intermediate(op);
        let len = oi.size as usize;
        pos_next = pos + len;

        let mut state: Option<ResolveLabelsState> = match op {
            opcodes::op_line_num => {
                /* line number info (for debug). We put it in a separate
                compressed table to reduce memory usage and get better
                performance */
                line_num = read_from_ptr::<i32>(bc_buf.add(pos + 1));
                None
            }
            opcodes::op_label => {
                label = read_from_ptr::<u32>(bc_buf.add(pos + 1));
                debug_assert!(label < (*s).label_slots.len() as u32);

                let lb = (*s).label_slots.get_mut(label as usize).unwrap();
                debug_assert_eq!(lb.addr, -1);
                lb.addr = bc_out.len() as _;

                // resolve pending relocation entries
                while let Some(re) = lb.pending_relocs.pop() {
                    let delta = lb.addr as usize - re.addr;
                    match re.size {
                        4 => {
                            write_to_ptr::<u32>(bc_out.as_mut_ptr().add(re.addr), delta as u32);
                        }
                        2 => {
                            debug_assert_eq!(delta, (delta as i16) as usize);
                            write_to_ptr::<u16>(bc_out.as_mut_ptr().add(re.addr), delta as u16);
                        }
                        1 => {
                            assert_eq!(delta, (delta as i8) as usize);
                            *bc_out.as_mut_ptr().add(re.addr) = delta as u8;
                        }
                        _ => {
                            unreachable!()
                        }
                    }
                }

                None
            }
            opcodes::OP_call | opcodes::OP_call_method => {
                /* detect and transform tail calls */
                let argc = read_from_ptr::<u16>(bc_buf.add(pos + 1));
                if cc.code_match(pos_next, &[OpCode::RETURN as i32]) {
                    if cc.line_num >= 0 {
                        line_num = cc.line_num as _;
                    }
                    (*s).add_pc2line_info(bc_out.len() as _, line_num);
                    push_vec_short_code(&mut bc_out, op + 1, argc);
                    pos_next = skip_dead_code(s, bc_buf, bc_len, cc.pos, &mut (line_num as i32));
                } else {
                    (*s).add_pc2line_info(bc_out.len() as _, line_num);
                    push_vec_short_code(&mut bc_out, op, argc);
                }
                None
            }
            opcodes::OP_return
            | opcodes::OP_return_undef
            | opcodes::OP_return_async
            | opcodes::OP_throw
            | opcodes::OP_throw_error => {
                pos_next = skip_dead_code(s, bc_buf, bc_len, pos_next, &mut (line_num as i32));
                Some(ResolveLabelsState::NoChange)
            }
            opcodes::OP_goto => Some(ResolveLabelsState::HasGoto {
                label: read_from_ptr::<u32>(bc_buf.add(pos + 1)),
                op: opcodes::OP_goto,
            }),
            opcodes::OP_gosub => {
                label = read_from_ptr::<u32>(bc_buf.add(pos + 1));
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
                label: read_from_ptr::<u32>(bc_buf.add(pos + 1)),
                op,
            }),
            opcodes::OP_if_true | opcodes::OP_if_false => {
                if OPTIMIZE {
                    cc.optimize_if_true_false(s, op, pos, &mut pos_next, &mut line_num, &mut bc_out)
                        .map(|(op1, label)| {
                            //op = op1;
                            ResolveLabelsState::HasLabel { label, op: op1 }
                        })
                } else {
                    let label = read_from_ptr::<u32>(bc_buf.add(pos + 1));
                    Some(ResolveLabelsState::HasLabel { label, op })
                }
            }

            opcodes::OP_with_get_var
            | opcodes::OP_with_put_var
            | opcodes::OP_with_delete_var
            | opcodes::OP_with_make_ref
            | opcodes::OP_with_get_ref
            | opcodes::OP_with_get_ref_undef => {
                let mut goto = None;
                let atom = read_from_ptr::<u32>(bc_buf.add(pos + 1));
                label = read_from_ptr::<u32>(bc_buf.add(pos + 5));
                let is_with = *bc_buf.add(pos + 9);
                if OPTIMIZE {
                    label = find_jump_target(s, label, &mut op1, std::ptr::null_mut());
                }
                debug_assert!(label < (*s).label_slots.len() as u32);
                let lb = (*s).label_slots.get_mut(label as usize).unwrap();
                (*s).add_pc2line_info(bc_out.len() as _, line_num);

                #[cfg(feature = "short_opcode")]
                (*s).jump_slots.push(super::JumpSlot {
                    op,
                    size: 4,
                    pos: bc_out.size + 1,
                    label,
                });

                push_vec_u8(&mut bc_out, op);
                push_vec_u32(&mut bc_out, atom);
                let delta = (*lb).addr as i32 - bc_out.len() as i32;
                push_vec_u32(&mut bc_out, delta as u32);
                if lb.addr == -1 {
                    // unresolved yet: create a new relocation entry
                    if lb
                        .add_reloc(ctx.as_mut().unwrap(), bc_out.len() - 4, 4)
                        .is_err()
                    {
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
                    if cc.code_match(pos_next, &[opcodes::OP_return_undef as i32]) {
                        if cc.line_num >= 0 {
                            line_num = cc.line_num;
                            goto.take();
                        }
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
                                (*s).add_pc2line_info(bc_out.len() as _, line_num);
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
                                (*s).add_pc2line_info(bc_out.len() as _, line_num);
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
                    if cc.code_match(
                        pos_next,
                        &[M2(opcodes::OP_if_false as i32, opcodes::OP_if_true as i32)],
                    ) {
                        goto = Some(ResolveLabelsState::HasConstantTest(
                            op == opcodes::OP_push_true,
                        ));
                    }
                }
                goto
            }
            opcodes::OP_push_i32 => {
                let mut goto = Some(ResolveLabelsState::NoChange);

                if OPTIMIZE {
                    let val = read_from_ptr::<i32>(bc_buf.add(pos + 1));
                    if val != i32::MIN && val != 0 && cc.code_match(pos_next, &[OpCode::NEG as i32])
                    {
                        // transform i32(val) neg -> i32(-val)
                        if cc.line_num >= 0 {
                            line_num = cc.line_num as _;
                        }
                        if cc.code_match(cc.pos, &[OpCode::DROP as i32]) {
                            if cc.line_num >= 0 {
                                line_num = cc.line_num as _;
                            }
                        } else {
                            (*s).add_pc2line_info(bc_out.len() as _, line_num);
                            push_vec_short_int(&mut bc_out, -val);
                        }
                        pos_next = cc.pos;
                        goto.take(); // cancel process NoChange, continue with updated pos_next
                    } else if cc.code_match(pos_next, &[OpCode::DROP as i32]) {
                        // remove push/drop pairs generated by the parser
                        if cc.line_num >= 0 {
                            line_num = cc.line_num as _;
                        }
                        pos_next = cc.pos;
                        goto.take(); // cancel process NoChange, continue with updated pos_next
                    } else if cc.code_match(
                        pos_next,
                        &[M2(OpCode::IF_FALSE as i32, OpCode::IF_TRUE as i32)],
                    ) {
                        // Optimize constant tests: `if (0)`, `if (1)`, `if (!0)`...
                        goto = Some(ResolveLabelsState::HasConstantTest(val != 0));
                    } else {
                        (*s).add_pc2line_info(bc_out.len() as _, line_num);
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
                    let idx = read_from_ptr::<u32>(bc_buf + pos + 1);
                    if (idx < 256) {
                        (*s).add_pc2line_info(bc_out.len() as _, line_num);
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
                    let atom = read_from_ptr::<u32>(bc_buf + pos + 1);
                    if (atom == JS_ATOM_length) {
                        JS_FreeAtom(ctx, atom);
                        (*s).add_pc2line_info(bc_out.len() as _, line_num);
                        dbuf_putc(&bc_out, OP_get_length);
                        goto.take();
                    }
                }
                goto
            }

            opcodes::OP_push_atom_value => {
                let mut goto = Some(ResolveLabelsState::NoChange);
                if OPTIMIZE {
                    let atom = JSAtom(read_from_ptr::<u32>(bc_buf.add(pos + 1)));
                    // remove push/drop pairs generated by the parser
                    if cc.code_match(pos_next, &[opcodes::OP_drop as i32]) {
                        JS_FreeAtom(ctx, atom);
                        if cc.line_num >= 0 {
                            line_num = cc.line_num;
                        }
                        pos_next = cc.pos;
                        goto.take(); // cancel goto no_change
                    } else {
                        #[cfg(feature = "short_opcode")]
                        if atom == JS_ATOM_empty_string {
                            JS_FreeAtom(ctx, atom);
                            (*s).add_pc2line_info(bc_out.size, line_num);
                            dbuf_putc(&bc_out, OP_push_empty_string);
                            goto.take(); // cancel goto no_change
                        }
                    }
                }

                goto
            }
            opcodes::OP_to_propkey | opcodes::OP_to_propkey2 => {
                let mut goto = Some(ResolveLabelsState::NoChange);
                if OPTIMIZE {
                    // remove redundant to_propkey/to_propkey2 opcodes when storing simple data
                    if cc.code_match(
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
                    ) || cc.code_match(
                        pos_next,
                        &[
                            M3(
                                OpCode::PUSH_I32 as _,
                                OpCode::PUSH_CONST as _,
                                OpCode::PUSH_ATOM_VALUE as _,
                            ),
                            OpCode::PUT_ARRAY_EL as _,
                        ],
                    ) || cc.code_match(
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
                    ) {
                        goto.take();
                    }
                }
                goto
            }
            opcodes::OP_undefined => {
                let mut goto = Some(ResolveLabelsState::NoChange);
                if OPTIMIZE {
                    // remove push/drop pairs generated by the parser
                    if cc.code_match(pos_next, &[OpCode::DROP as _]) {
                        if cc.line_num >= 0 {
                            line_num = cc.line_num as _;
                        }
                        pos_next = cc.pos;
                        goto.take();
                    } else if cc.code_match(pos_next, &[OpCode::RETURN as _]) {
                        // transform undefined return -> return_undefined
                        if cc.line_num >= 0 {
                            line_num = cc.line_num as _;
                        }
                        (*s).add_pc2line_info(bc_out.len() as _, line_num);
                        push_vec_u8(&mut bc_out, OpCode::RETURN_UNDEF as _);
                        pos_next = cc.pos;
                        goto.take();
                    } else if cc
                        .code_match(pos_next, &[M2(OpCode::IF_FALSE as _, OpCode::IF_TRUE as _)])
                    {
                        // transform undefined if_true(l1)/if_false(l1) -> nop/goto(l1)
                        goto = Some(ResolveLabelsState::HasConstantTest(false));
                    }

                    #[cfg(feature = "short_opcode")]
                    {
                        // transform undefined strict_eq -> is_undefined
                        if cc.code_match(pos_next, &[OpCode::STRICT_EQ as _]) {
                            if cc.line_num >= 0 {
                                line_num = cc.line_num as _;
                            }
                            (*s).add_pc2line_info(bc_out.size, line_num);
                            push_vec_u8(&mut bc_out, OpCode::IS_UNDEFINED as _);
                            pos_next = cc.pos;
                            goto.take();
                        } else if cc.code_match(
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
                            (*s).add_pc2line_info(bc_out.size, line_num);
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
                    if cc.code_match(
                        pos_next,
                        &[
                            M2(OpCode::PUT_FIELD as _, OpCode::PUT_VAR_STRICT as _),
                            OpCode::DROP as _,
                        ],
                    ) {
                        if cc.line_num >= 0 {
                            line_num = cc.line_num as _;
                        }
                        (*s).add_pc2line_info(bc_out.len() as _, line_num);
                        push_vec_u8(&mut bc_out, cc.op as u8);
                        push_vec_u32(&mut bc_out, cc.atom.0);
                        pos_next = cc.pos;
                        goto.take();
                    }
                }
                goto
            }
            opcodes::OP_dup => {
                if OPTIMIZE && cc.optimize_dup(s, pos, &mut pos_next, &mut line_num, &mut bc_out) {
                    None
                } else {
                    Some(ResolveLabelsState::NoChange)
                }
            }
            opcodes::OP_get_loc => {
                if OPTIMIZE
                    && cc.optimize_get_loc(s, pos, &mut pos_next, &mut line_num, &mut bc_out)
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
                    cc.optimize_put_loc(s, op, pos, &mut pos_next, &mut line_num, &mut bc_out);
                    None
                } else {
                    Some(ResolveLabelsState::NoChange)
                }
            }
            opcodes::OP_post_inc | opcodes::OP_post_dec => {
                if OPTIMIZE
                    && cc.optimize_post_dec_inc(
                        s,
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

        // dispatch post process
        while let Some(p) = state.take() {
            match p {
                ResolveLabelsState::NoChange => {
                    (*s).add_pc2line_info(bc_out.len() as _, line_num);
                    push_vec_slice(&mut bc_out, bc_buf.add(pos as usize), len as usize)?;
                }
                ResolveLabelsState::HasGoto {
                    mut label,
                    op: goto_op,
                } => {
                    state = Some(ResolveLabelsState::HasLabel { label, op: goto_op });
                    if OPTIMIZE {
                        let mut line1 = -1;
                        /* Use custom matcher because multiple labels can follow */
                        label = find_jump_target(s, label, &mut op1, &mut line1);
                        if code_has_label(&mut cc, pos_next as _, label) {
                            /* jump to next instruction: remove jump */
                            (*s).update_label_ref_count(label, -1);
                            state.take(); // don't process label
                        } else if matches!(
                            op1,
                            opcodes::OP_return | opcodes::OP_return_undef | opcodes::OP_throw
                        ) {
                            /* jump to return/throw: remove jump, append return/throw */
                            /* updating the line number obfuscates assembly listing */
                            //if (line1 >= 0) line_num = line1;
                            (*s).update_label_ref_count(label, -1);
                            (*s).add_pc2line_info(bc_out.len() as _, line_num);
                            push_vec_u8(&mut bc_out, op1);
                            pos_next =
                                skip_dead_code(s, bc_buf, bc_len, pos_next, &mut (line_num as i32));
                            state.take(); // don't process label
                        }

                        /* XXX: should duplicate single instructions followed by goto or return */
                        /* For example, can match one of these followed by return:
                          push_i32 / push_const / push_atom_value / get_var /
                          undefined / null / push_false / push_true / get_ref_value /
                          get_loc / get_arg / get_var_ref
                        */
                    }
                }
                ResolveLabelsState::HasLabel {
                    label,
                    op: label_op,
                } => {
                    debug_assert!(label < (*s).label_slots.len() as u32);

                    (*s).add_pc2line_info(bc_out.len() as _, line_num);
                    if label_op == opcodes::OP_goto {
                        pos_next =
                            skip_dead_code(s, bc_buf, bc_len, pos_next, &mut (line_num as i32));
                    }
                    let lb = (*s).label_slots.get_mut(label as usize).unwrap();

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
                                    opcodes::OP_if_false | opcodes::OP_if_true | opcodes::OP_goto
                                )
                            {
                                jp.size = 1;
                                jp.op = opcodes::OP_if_false8 + (label_op - opcodes::OP_if_false);
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
                                    opcodes::OP_if_false | opcodes::OP_if_true | opcodes::OP_goto
                                )
                            {
                                jp.size = 1;
                                jp.op = OP_if_false8 + (label_op - OP_if_false);
                                push_vec_u8(
                                    &mut bc_out,
                                    opcodes::OP_if_false8 + (label_op - opcodes::OP_if_false),
                                );
                                push_vec_u8(&mut bc_out, diff as u8);
                            } else if diff == diff as u16 as i32 && label_op == opcodes::OP_goto {
                                jp.size = 2;
                                jp.op = OP_goto16;
                                push_vec_u8(&mut bc_out, opcodes::OP_goto16);
                                push_vec_u16(&mut bc_out, diff as u16);
                            }
                        }

                        (*s).jump_slots.push(jp);
                    }

                    push_vec_u8(&mut bc_out, label_op);
                    let delta = (*lb).addr as i32 - bc_out.len() as i32; // could be minus
                    push_vec_u32(&mut bc_out, delta as u32);

                    if (*lb).addr == -1 {
                        // unresolved yet: create a new relocation entry
                        if lb
                            .add_reloc(ctx.as_mut().unwrap(), bc_out.len() - 4, 4)
                            .is_err()
                        {
                            state = Some(ResolveLabelsState::Fail);
                        }
                    }
                }
                ResolveLabelsState::HasConstantTest(val) => {
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
                            (*s).update_label_ref_count(cc.label as _, -1);
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
    for lb in (*s).label_slots.iter() {
        debug_assert!(lb.pending_relocs.is_empty());
    }

    #[cfg(feature = "short_opcode")]
    {
        if OPTIMIZE {
            let mut patch_offsets = 0;
            for jp in (*s).jump_slots.iter_mut() {
                let mut delta = 3;
                let mut op = jp.op;
                match op {
                    OP_goto16 => {
                        delta = 1;
                        // fall thru
                    }
                    OP_if_false | OP_if_true | OP_goto => {
                        let pos = jp.pos;
                        let diff = (*(*s).label_slots.offset(jp.label as isize)).addr - pos;
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
                            for j in 0..(*s).label_count {
                                let ls = (*s).label_slots.offset(j as isize);
                                if (*ls).addr > pos {
                                    (*ls).addr -= delta;
                                }
                            }

                            for j in i + 1..(*s).jump_count {
                                let jp1 = (*s).jump_slots.offset(j as isize);
                                if (*jp1).pos > pos {
                                    (*jp1).pos -= delta;
                                }
                            }
                            for j in 0..(*s).line_number_count {
                                if (*(*s).line_number_slots.offset(j as isize)).pc > pos {
                                    (*(*s).line_number_slots.offset(j as isize)).pc -= delta;
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
                            for j in 0..(*s).label_count {
                                let ls = (*s).label_slots.offset(j as isize);
                                if (*ls).addr > pos {
                                    (*ls).addr -= delta;
                                }
                            }
                            for j in i + 1..(*s).jump_count {
                                let jp1 = (*s).jump_slots.offset(j as isize);
                                if (*jp1).pos > pos {
                                    (*jp1).pos -= delta;
                                }
                            }
                            for j in 0..(*s).line_number_count {
                                if (*(*s).line_number_slots.offset(j as isize)).pc > pos {
                                    (*(*s).line_number_slots.offset(j as isize)).pc -= delta;
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
                for i in 0..(*s).jump_count {
                    let jp1 = (*s).jump_slots.offset(i as isize);
                    let diff1 = (*(*s).label_slots.offset((*jp1).label as isize)).addr - (*jp1).pos;
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

        (*s).jump_slots.clear();
    }

    // XXX: should delay until copying to runtime bytecode function /
    compute_pc2line_info(s);

    (*s).label_slots.clear();
    (*s).line_number_slots.clear();

    // set the new byte code
    bc_out.shrink_to_fit();
    (*s).byte_code = bc_out;
    (*s).use_short_opcodes = true;

    Ok(())
}
