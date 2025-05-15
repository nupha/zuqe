use crate::{
    atom::{JSAtom, JS_FreeAtom},
    bytecode::{opcode_info_intermediate, opcodes, OPSpecialObjectEnum, OpCode},
    context::{JSContext, JS_MODE_STRICT, JS_MODE_STRIP},
    function::{
        skip_dead_code, JSFunctionDef, LineNumberSlot, PC2LINE_BASE, PC2LINE_DIFF_PC_MAX,
        PC2LINE_OP_FIRST, PC2LINE_RANGE,
    },
    mem::{js_free, js_mallocz},
    parse::JSParseState,
    utils::{
        get_u16, get_u32, push_vec_leb128, push_vec_short_code, push_vec_short_int,
        push_vec_sleb128, push_vec_slice, push_vec_u16, push_vec_u32, push_vec_u8, put_u16,
        put_u32, put_u8, M2, M3, M4,
    },
};

use super::optimize::{CodeContext, OPTIMIZE};

#[derive(Debug)]
pub struct RelocEntry {
    pub addr: usize, /* address to patch */
    pub size: i32,   /* address size: 1, 2 or 4 bytes */
}

#[derive(Debug)]
pub struct LabelInfo {
    pub ref_count: i32,

    /// phase 1 address, -1 means not resolved yet
    pub pos: i32,
    /// phase 2 address, -1 means not resolved yet
    pub pos2: i32,
    /// phase 3 address, -1 means not resolved yet
    pub addr: i32,

    /// pending relocations
    pub pending_relocs: Vec<RelocEntry>,
}

impl LabelInfo {
    fn add_reloc(&mut self, ctx: &mut JSContext, addr: usize, size: i32) -> Result<(), i32> {
        if self.pending_relocs.len() == self.pending_relocs.capacity() {
            if self.pending_relocs.try_reserve_exact(4).is_err() {
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        self.pending_relocs.push(RelocEntry { addr, size });
        Ok(())
    }
}

impl JSFunctionDef {
    /// Create new label in JSFunctionDef, returns new label id.
    pub unsafe fn add_label(&mut self) -> Result<u32, i32> {
        if self.label_slots.len() == self.label_slots.capacity() {
            if self.label_slots.try_reserve_exact(4).is_err() {
                (*self.ctx).JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        self.label_slots.push(LabelInfo {
            ref_count: 0,
            pos: -1,
            pos2: -1,
            addr: -1,
            pending_relocs: Vec::new(),
        });

        Ok(self.label_slots.len() as u32 - 1)
    }
}

#[deprecated(note = "use JSFunctionDef::add_label() instead")]
pub unsafe fn new_label_fd(fd: *mut JSFunctionDef, label: i32) -> i32 {
    if label < 0 {
        (*fd).add_label().map_or(-1, |x| x as i32)
    } else {
        label
    }
}

#[deprecated(note = "use JSFunctionDef::add_label() instead")]
pub unsafe fn new_label(s: *mut JSParseState) -> i32 {
    (*(*s).cur_func).add_label().map_or(-1, |x| x as i32)
}

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
    let mut jp: *mut JumpSlot = std::ptr::null_mut();
    #[cfg(feature = "short_opcode")]
    if (*s).jump_size != 0 {
        (*s).jump_slots = js_mallocz(
            (*s).ctx,
            std::mem::size_of::<*mut JumpSlot>() * (*s).jump_size,
        );
        if (*s).jump_slots.is_null() {
            return -1;
        }
    }

    if (*s).line_number_size != 0 && (*s).js_mode & JS_MODE_STRIP as u8 == 0 {
        (*s).line_number_slots = js_mallocz(
            (*s).ctx,
            std::mem::size_of::<LineNumberSlot>() * (*s).line_number_size as usize,
        )
        .cast();
        if (*s).line_number_slots.is_null() {
            return Err(-1);
        }
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
                line_num = get_u32(bc_buf.add(pos as usize + 1));
                None
            }
            opcodes::op_label => {
                label = get_u32(bc_buf.add(pos as usize + 1));
                debug_assert!(label < (*s).label_slots.len() as u32);

                let lb = (*s).label_slots.get_mut(label as usize).unwrap();
                debug_assert_eq!(lb.addr, -1);
                lb.addr = bc_out.len() as _;

                // resolve pending relocation entries
                while let Some(re) = lb.pending_relocs.pop() {
                    let delta = lb.addr as usize - re.addr;
                    match re.size {
                        4 => put_u32(bc_out.as_mut_ptr().add(re.addr as usize), delta as u32),
                        2 => {
                            debug_assert_eq!(delta, (delta as i16) as usize);
                            put_u16(bc_out.as_mut_ptr().add(re.addr as usize), delta as u16);
                        }
                        1 => {
                            assert_eq!(delta, (delta as i8) as usize);
                            put_u8(bc_out.as_mut_ptr().add(re.addr as usize), delta as u8);
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
                let argc = get_u16(bc_buf.add(pos as usize + 1));
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
                label: get_u32(bc_buf.add(pos + 1)),
                op: opcodes::OP_goto,
            }),
            opcodes::OP_gosub => {
                label = get_u32(bc_buf.add(pos as usize + 1));
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
                label: get_u32(bc_buf.add(pos + 1)),
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
                    let label = get_u32(bc_buf.add(pos + 1));
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
                let atom = get_u32(bc_buf.add(pos + 1));
                label = get_u32(bc_buf.add(pos + 5));
                let is_with = *bc_buf.add(pos + 9);
                if OPTIMIZE {
                    label = find_jump_target(s, label, &mut op1, std::ptr::null_mut());
                }
                debug_assert!(label < (*s).label_slots.len() as u32);
                let lb = (*s).label_slots.get_mut(label as usize).unwrap();
                (*s).add_pc2line_info(bc_out.len() as _, line_num);

                #[cfg(feature = "short_opcode")]
                {
                    jp = &mut (*(*s).jump_slots.offset((*s).jump_count as isize));
                    (*jp).op = op;
                    (*jp).size = 4;
                    (*jp).pos = bc_out.size + 1;
                    (*jp).label = label;
                    (*s).jump_count += 1;
                }

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
                            line_num = cc.line_num as u32;
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
                    let val = get_u32(bc_buf.add(pos + 1)) as i32;
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
                    let idx = get_u32(bc_buf + pos + 1);
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
                    let atom = get_u32(bc_buf + pos + 1);
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
                    let atom = JSAtom(get_u32(bc_buf.add(pos as usize + 1)));
                    // remove push/drop pairs generated by the parser
                    if cc.code_match(pos_next, &[opcodes::OP_drop as i32]) {
                        JS_FreeAtom(ctx, atom);
                        if cc.line_num >= 0 {
                            line_num = cc.line_num as u32;
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
                        let jp = (*s).jump_slots.add({
                            let n = (*s).jump_count;
                            (*s).jump_count += 1;
                            n
                        });
                        (*jp).op = label_op;
                        (*jp).size = 4;
                        (*jp).pos = bc_out.len() + 1;
                        (*jp).label = label;

                        if (*ls).addr == -1 {
                            let diff = (*ls).pos2 - pos as i32 - 1;
                            if diff < 128
                                && matches!(
                                    label_op,
                                    opcodes::OP_if_false | opcodes::OP_if_true | opcodes::OP_goto
                                )
                            {
                                (*jp).size = 1;
                                (*jp).op =
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
                                (*jp).size = 2;
                                (*jp).op = OP_goto16;
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
                                (*jp).size = 1;
                                (*jp).op = OP_if_false8 + (label_op - OP_if_false);
                                push_vec_u8(
                                    &mut bc_out,
                                    opcodes::OP_if_false8 + (label_op - opcodes::OP_if_false),
                                );
                                push_vec_u8(&mut bc_out, diff as u8);
                            } else if diff == diff as u16 as i32 && label_op == opcodes::OP_goto {
                                (*jp).size = 2;
                                (*jp).op = OP_goto16;
                                push_vec_u8(&mut bc_out, opcodes::OP_goto16);
                                push_vec_u16(&mut bc_out, diff as u16);
                            }
                        }
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
            for i in 0..(*s).jump_count {
                let jp = (*s).jump_slots.offset(i as isize);
                let mut delta = 3;
                let mut op = (*jp).op;
                match op {
                    OP_goto16 => {
                        delta = 1;
                        // fall thru
                    }
                    OP_if_false | OP_if_true | OP_goto => {
                        let pos = (*jp).pos;
                        let diff = (*(*s).label_slots.offset((*jp).label as isize)).addr - pos;
                        if diff >= -128 && diff <= 127 + delta {
                            (*jp).size = 1;
                            if op == OP_goto16 {
                                *bc_out.buf.offset(pos - 1) = OP_goto8;
                            } else {
                                *bc_out.buf.offset(pos - 1) = OP_if_false8 + (op - OP_if_false);
                            }
                            // C2RS: memmove translated to copy_within
                            std::ptr::copy(
                                bc_out.buf.offset(pos + (*jp).size as isize),
                                bc_out.buf.offset(pos + (*jp).size as isize + delta),
                                (bc_out.size - pos - (*jp).size - delta) as usize,
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
                        } else if diff == (diff as i16) as i32 && op == OP_goto {
                            (*jp).size = 2;
                            delta = 2;
                            *bc_out.buf.offset(pos - 1) = OP_goto16;
                            // C2RS: memmove translated to copy_within
                            std::ptr::copy(
                                bc_out.buf.offset(pos + (*jp).size as isize),
                                bc_out.buf.offset(pos + (*jp).size as isize + delta),
                                (bc_out.size - pos - (*jp).size - delta) as usize,
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
                        1 => put_u8(bc_out.buf.offset((*jp1).pos as isize), diff1),
                        2 => put_u16(bc_out.buf.offset((*jp1).pos as isize), diff1 as i16),
                        4 => put_u32(bc_out.buf.offset((*jp1).pos as isize), diff1),
                        _ => {}
                    }
                }
            }
        }
        js_free(ctx, (*s).jump_slots as *mut std::ffi::c_void);
        (*s).jump_slots = std::ptr::null_mut();
    }

    (*s).label_slots.clear();

    // XXX: should delay until copying to runtime bytecode function /
    compute_pc2line_info(s);
    if !(*s).line_number_slots.is_null() {
        js_free(ctx, (*s).line_number_slots.cast());
        (*s).line_number_slots = std::ptr::null_mut();
    }

    // set the new byte code
    bc_out.shrink_to_fit();
    (*s).byte_code = bc_out;
    (*s).use_short_opcodes = true;

    Ok(())
}

/// the pc2line table gives a line number for each PC value
#[deprecated(note = "use JSFunctionDef::add_pc2line_info() instead")]
pub unsafe fn add_pc2line_info(s: *mut JSFunctionDef, pc: u32, line_num: u32) {
    (*s).add_pc2line_info(pc, line_num);
}

pub unsafe fn code_has_label(s: *mut CodeContext, mut pos: usize, label: u32) -> bool {
    while pos < (*s).bc_len {
        let op = *(*s).bc_buf.offset(pos as isize);
        if op == opcodes::op_line_num {
            pos += 5;
            continue;
        }
        if op == opcodes::op_label {
            let lab = get_u32((*s).bc_buf.offset((pos + 1) as isize));
            if lab == label {
                return true;
            }
            pos += 5;
            continue;
        }
        if op == opcodes::OP_goto {
            let lab = get_u32((*s).bc_buf.offset((pos + 1) as isize));
            if lab == label {
                return true;
            }
        }
        break;
    }

    false
}

pub unsafe fn compute_pc2line_info(s: *mut JSFunctionDef) {
    if !((*s).js_mode & JS_MODE_STRIP as u8 != 0) && !((*s).line_number_slots.is_null()) {
        let mut last_line_num = (*s).line_num as i32;
        let mut last_pc: u32 = 0;

        //js_dbuf_init((*s).ctx, &mut (*s).pc2line);
        if (*s).pc2line.is_none() {
            (*s).pc2line = Some(Vec::with_capacity(64));
        }
        let lst = (*s).pc2line.as_mut().unwrap();

        for i in 0..(*s).line_number_count {
            let pc = (*(*s).line_number_slots.offset(i as isize)).pc;
            let line_num = (*(*s).line_number_slots.offset(i as isize)).line_num as i32;
            if line_num < 0 {
                continue;
            }

            let diff_pc = pc.wrapping_sub(last_pc) as i32;
            let diff_line = line_num - last_line_num;
            if diff_line == 0 || diff_pc < 0 {
                continue;
            }

            if diff_line >= PC2LINE_BASE
                && diff_line < PC2LINE_BASE + PC2LINE_RANGE
                && diff_pc <= PC2LINE_DIFF_PC_MAX
            {
                push_vec_u8(
                    lst,
                    ((diff_line - PC2LINE_BASE) + diff_pc * PC2LINE_RANGE + PC2LINE_OP_FIRST) as u8,
                );
            } else {
                // longer encoding
                push_vec_u8(lst, 0);
                push_vec_leb128(lst, diff_pc as u32);
                push_vec_sleb128(lst, diff_line);
            }

            last_pc = pc;
            last_line_num = line_num;
        }
    }
}

/// return the target label, following the OP_goto jumps
/// the first opcode at destination is stored in *pop
pub unsafe fn find_jump_target(
    s: *mut JSFunctionDef,
    mut label: u32,
    pop: *mut u8,
    pline: *mut i32,
) -> u32 {
    (*s).update_label_ref_count(label as _, -1);

    let mut op: u8 = 0;

    for _ in 0..10 {
        debug_assert!(label < (*s).label_slots.len() as u32);
        let mut pos = (*s).label_slots[label as usize].pos2;

        loop {
            op = (*s).byte_code[pos as usize];
            match op {
                opcodes::op_line_num | opcodes::op_label => {
                    if op == opcodes::op_line_num {
                        if !pline.is_null() {
                            *pline =
                                get_u32((*s).byte_code.as_ptr().offset(pos as isize + 1)) as i32;
                        }
                    }
                    pos += opcode_info_intermediate(op).size as i32;
                    continue;
                }
                opcodes::OP_goto => {
                    label = get_u32((*s).byte_code.as_ptr().offset((pos + 1) as isize));
                }
                opcodes::OP_drop => {
                    // ignore drop opcodes if followed by OP_return_undef
                    while *(*s).byte_code.as_ptr().offset((pos + 1) as isize) == opcodes::OP_drop {
                        pos += 1;
                    }
                    if (*s).byte_code[pos as usize] == opcodes::OP_return_undef {
                        op = opcodes::OP_return_undef;
                    }
                }
                _ => {}
            }

            break;
        } // loop
    } // for

    // done:
    *pop = op;
    (*s).update_label_ref_count(label, 1);
    label
}
