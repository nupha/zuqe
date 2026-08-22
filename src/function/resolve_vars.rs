// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, opcode_info_intermediate, opcodes},
    context::ScopedContext,
    eval::EvalType,
    stack_frame::ExecMode,
    utils::{M2, read_from_ptr},
};

use super::{
    FunctionDef, JSVarKindEnum, get_label_pos, instantiate_hoisted_definitions, optimize::OPTIMIZE,
    push_vec_slice, push_vec_u8, push_vec_u16, push_vec_u32, skip_dead_code,
};

impl FunctionDef {
    /// Resolves variable references in the function definition
    /// Converts global variable accesses to local/closure variables when needed
    /// This is the second pass of bytecode generation
    /// Handles scope resolution and variable binding
    pub fn resolve_variables(&mut self, ctx: &mut ScopedContext) -> Result<(), i32> {
        let bc_buf_ptr = self.byte_code.as_mut_ptr();
        let bc_buf_len = self.byte_code.len();

        let mut bc_out = Vec::<u8>::new();
        if bc_out
            .try_reserve(bc_buf_len + std::cmp::max(bc_buf_len >> 3, 64))
            .is_err()
        {
            return Err(ctx.throw_out_of_memory());
        };

        let mut line_num = 0_i32;

        macro_rules! no_change {
            ($pos:expr, $len:expr) => {
                push_vec_slice(
                    &mut bc_out,
                    unsafe { bc_buf_ptr.add($pos as usize) },
                    $len as usize,
                )
                .expect("TODO: handle bytecode alloc exception");
            };
        }

        let mut line_pos = 0_u32;
        let mut pos = 0_usize;
        let mut pos_next: usize;

        while pos < bc_buf_len {
            let op = self.byte_code[pos];
            let oi = opcode_info_intermediate(op);
            let op_len = oi.size as usize;

            pos_next = pos + op_len;

            match op {
                opcodes::op_line_num => {
                    line_num = unsafe { read_from_ptr::<i32>(bc_buf_ptr.add(pos + 1)) };
                    line_pos = unsafe { read_from_ptr::<u32>(bc_buf_ptr.add(pos + 5)) };
                    self.line_number_slots_cap += 1;
                    no_change!(pos, op_len);
                }
                opcodes::OP_eval => {
                    // convert scope index to adjusted variable index
                    let (call_argc, scope) = unsafe {
                        (
                            read_from_ptr::<u16>(bc_buf_ptr.add(pos + 1)),
                            read_from_ptr::<u16>(bc_buf_ptr.add(pos + 3)),
                        )
                    };

                    mark_eval_captured_variables(self, scope as _);

                    // TCO: `eval(...) + return` (possibly separated by labels/
                    // line_nums, e.g. ??/&&/||) → tail_eval. At runtime a
                    // resolved value that isn't %eval% becomes a proper tail
                    // call; %eval% still runs as direct eval in place.
                    // PTC applies only in strict mode code (ES IsInTailPosition)
                    let mut scan_line_num = line_num;
                    let mut found_return = false;
                    if OPTIMIZE && self.attrs.exec_mode.contains(ExecMode::STRICT) {
                        let mut scan = pos_next;
                        while scan < bc_buf_len {
                            let scan_op = unsafe { *bc_buf_ptr.add(scan) };
                            let scan_len = opcode_info_intermediate(scan_op).size as usize;
                            if scan + scan_len > bc_buf_len {
                                break;
                            }
                            match scan_op {
                                opcodes::op_line_num => {
                                    scan_line_num = unsafe {
                                        read_from_ptr::<i32>(bc_buf_ptr.add(scan + 1))
                                    };
                                    scan += scan_len;
                                }
                                opcodes::op_label | opcodes::OP_nip_catch => {
                                    scan += scan_len;
                                }
                                opcodes::OP_gosub => {
                                    // A gosub to a real try-finally body must run
                                    // after the call: not tail position. An empty
                                    // finally region is a no-op indirection.
                                    let label_id = unsafe {
                                        read_from_ptr::<u32>(bc_buf_ptr.add(scan + 1))
                                    };
                                    if self
                                        .code_match(
                                            self.label_slots[label_id as usize].pos as _,
                                            &[OpCode::RET as i32],
                                        )
                                        .is_some()
                                    {
                                        scan += scan_len;
                                    } else {
                                        break;
                                    }
                                }
                                opcodes::OP_return | opcodes::OP_return_undef => {
                                    found_return = true;
                                    break;
                                }
                                _ => break,
                            }
                        }
                    }

                    if found_return {
                        if scan_line_num >= 0 && scan_line_num != line_num {
                            line_num = scan_line_num;
                            self.line_number_slots_cap += 1;
                            push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                            push_vec_u32(&mut bc_out, line_num as u32);
                            push_vec_u32(&mut bc_out, 0); // line_pos: not available in resolve
                        }
                        self.add_pc2line_info(bc_out.len() as _, line_num, line_pos);

                        push_vec_u8(&mut bc_out, opcodes::OP_tail_eval);
                    } else {
                        push_vec_u8(&mut bc_out, op);
                    }
                    push_vec_u16(&mut bc_out, call_argc);
                    push_vec_u16(&mut bc_out, (self.scopes[scope as usize].first + 1) as _);
                }
                opcodes::OP_apply_eval => {
                    // convert scope index to adjusted variable index
                    let scope = unsafe { read_from_ptr::<u16>(bc_buf_ptr.add(pos + 1)) };

                    mark_eval_captured_variables(self, scope as _);

                    push_vec_u8(&mut bc_out, op);
                    push_vec_u16(&mut bc_out, (self.scopes[scope as usize].first + 1) as _);
                }
                opcodes::op_scope_get_var_checkthis
                | opcodes::op_scope_get_var_undef
                | opcodes::op_scope_get_var
                | opcodes::op_scope_put_var
                | opcodes::op_scope_delete_var
                | opcodes::op_scope_get_ref
                | opcodes::op_scope_put_var_init => {
                    let var_name =
                        unsafe { Atom::from_id(read_from_ptr::<u32>(bc_buf_ptr.add(pos + 1))) };
                    let scope = unsafe { read_from_ptr::<u16>(bc_buf_ptr.add(pos + 5)) };
                    pos_next = self.resolve_scope_var(
                        ctx,
                        var_name,
                        scope as _,
                        op,
                        &mut bc_out,
                        std::ptr::null_mut(),
                        None,
                        pos_next,
                    );
                }
                opcodes::op_scope_make_ref => {
                    let (label_id, scope) = unsafe {
                        (
                            read_from_ptr::<u32>(bc_buf_ptr.add(pos + 5)),
                            read_from_ptr::<u16>(bc_buf_ptr.add(pos + 9)),
                        )
                    };

                    // always remove label reference
                    self.label_slots[label_id as usize].ref_count -= 1;

                    let var_name =
                        unsafe { Atom::from_id(read_from_ptr::<u32>(bc_buf_ptr.add(pos + 1))) };

                    pos_next = self.resolve_scope_var(
                        ctx,
                        var_name,
                        scope as _,
                        op,
                        &mut bc_out,
                        bc_buf_ptr,
                        Some(label_id),
                        pos_next,
                    );
                }
                opcodes::op_scope_get_private_field
                | opcodes::op_scope_get_private_field2
                | opcodes::op_scope_put_private_field
                | opcodes::op_scope_in_private_field => {
                    let var_name =
                        unsafe { Atom::from_id(read_from_ptr::<u32>(bc_buf_ptr.add(pos + 1))) };
                    let scope = unsafe { read_from_ptr::<u16>(bc_buf_ptr.add(pos + 5)) };

                    if self
                        .resolve_scope_private_field(ctx, var_name, scope as _, op, &mut bc_out)
                        .is_err()
                    {
                        // fail: continue the copy to keep the atom refcounts consistent
                        // TODO: use free_atoms_in_bytecode() with sub-slice from pos through end
                        while pos < bc_buf_len {
                            let op_len1 =
                                opcode_info_intermediate(self.byte_code[pos]).size as usize;
                            pos_next = pos + op_len1;

                            if push_vec_slice(&mut bc_out, unsafe { bc_buf_ptr.add(pos) }, op_len1)
                                .is_err()
                            {
                                let _ = ctx.throw_out_of_memory();
                                break;
                            }
                            pos = pos_next;
                        }

                        bc_out.shrink_to_fit();
                        self.byte_code = bc_out;
                        return Err(-1);
                    }
                }
                opcodes::OP_gosub => {
                    #[cfg(feature = "short_opcode")]
                    {
                        self.jump_slots_cap += 1;
                    }

                    if OPTIMIZE {
                        // remove calls to empty finalizers
                        let label_id = unsafe { read_from_ptr::<u32>(bc_buf_ptr.add(pos + 1)) };
                        debug_assert!(label_id < self.label_slots.len() as u32);

                        if self
                            .code_match(
                                self.label_slots[label_id as usize].pos as _,
                                &[OpCode::RET as i32],
                            )
                            .is_some()
                        {
                            self.label_slots[label_id as usize].ref_count -= 1;
                        } else {
                            no_change!(pos, op_len);
                        }
                    } else {
                        no_change!(pos, op_len);
                    }
                }
                opcodes::OP_drop => {
                    no_change!(pos, op_len);
                }
                opcodes::OP_insert3 => {
                    // Optimize transformation:
                    // insert3 put_array_el|put_ref_value drop -> put_array_el|put_ref_value
                    if OPTIMIZE
                        && let Some(cc) = self.code_match(
                            pos_next,
                            &[
                                M2(OpCode::PUT_ARRAY_EL as i32, OpCode::PUT_REF_VALUE as i32),
                                OpCode::DROP as i32,
                            ],
                        )
                    {
                        push_vec_u8(&mut bc_out, cc.op as u8);
                        pos_next = cc.pos;
                        if cc.line_num != -1 && cc.line_num != line_num {
                            line_num = cc.line_num;
                            self.line_number_slots_cap += 1;
                            push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                            push_vec_u32(&mut bc_out, line_num as u32);
                            push_vec_u32(&mut bc_out, 0); // line_pos: not available in resolve
                        }
                    } else {
                        no_change!(pos, op_len);
                    }
                }
                opcodes::OP_goto
                | opcodes::OP_tail_call
                | opcodes::OP_tail_call_method
                | opcodes::OP_return
                | opcodes::OP_return_undef
                | opcodes::OP_throw
                | opcodes::OP_throw_error
                | opcodes::OP_ret => {
                    #[cfg(feature = "short_opcode")]
                    if op == opcodes::OP_goto {
                        self.jump_slots_cap += 1;
                    }

                    if OPTIMIZE {
                        // remove dead code
                        push_vec_slice(&mut bc_out, unsafe { bc_buf_ptr.add(pos) }, op_len as _)
                            .expect("TODO");

                        let mut line = -1_i32;
                        pos = skip_dead_code(self, bc_buf_ptr, bc_buf_len, pos + op_len, &mut line);
                        pos_next = pos;
                        if pos < bc_buf_len && line >= 0 && line_num != line {
                            line_num = line;
                            self.line_number_slots_cap += 1;
                            push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                            push_vec_u32(&mut bc_out, line_num as u32);
                            push_vec_u32(&mut bc_out, 0); // line_pos: not available in resolve
                        }
                    } else {
                        no_change!(pos, op_len);
                    }
                }
                opcodes::op_label => {
                    let label_id = unsafe { read_from_ptr::<u32>(bc_buf_ptr.add(pos + 1)) };
                    debug_assert!(label_id < self.label_slots.len() as u32);

                    let lb = self.label_slots.get_mut(label_id as usize).unwrap();
                    debug_assert_eq!(lb.pos2, -1);
                    lb.pos2 = bc_out.len() as i32 + oi.size as i32;
                    no_change!(pos, op_len);
                }
                opcodes::op_enter_scope => {
                    let scope = unsafe { read_from_ptr::<u16>(bc_buf_ptr.add(pos + 1)) } as i32;
                    if scope == self.body_scope {
                        // Annex B.3.5: non-strict direct eval var declaration cannot shadow
                        // a lexical declaration in the enclosing scope.
                        if self.eval_type == EvalType::Direct
                            && let Some(cvars) = self.closure_vars.as_ref()
                        {
                            for hf in &self.global_vars {
                                for cv in cvars {
                                    if cv.var_name == hf.var_name && cv.is_lexical() {
                                        return Err(ctx.throw_syntax_error(
                                            "invalid redefinition of lexical identifier",
                                        ));
                                    }
                                }
                            }
                        }

                        instantiate_hoisted_definitions(ctx, self, &mut bc_out)?;
                    }

                    let mut scope_idx = self.scopes[scope as usize].first;
                    while scope_idx >= 0 {
                        let vd = self.vars.get(scope_idx as usize).unwrap();
                        if vd.scope_level == scope {
                            if scope_idx as u16 != self.arguments_arg_idx.unwrap_or(u16::MAX) {
                                if matches!(
                                    vd.var_kind(),
                                    JSVarKindEnum::JS_VAR_FUNCTION_DECL
                                        | JSVarKindEnum::JS_VAR_NEW_FUNCTION_DECL
                                ) {
                                    push_vec_u8(&mut bc_out, OpCode::FCLOSURE as u8);
                                    push_vec_u32(&mut bc_out, vd.func_pool_idx as u32);

                                    push_vec_u8(&mut bc_out, OpCode::PUT_LOC as u8);
                                    push_vec_u16(&mut bc_out, scope_idx as u16);
                                } else {
                                    // XXX: should check if variable can be used before initialization
                                    push_vec_u8(&mut bc_out, OpCode::SET_LOC_UNINITIALIZED as u8);
                                    push_vec_u16(&mut bc_out, scope_idx as u16);
                                }
                            }
                            scope_idx = vd.scope_next;
                        } else {
                            break;
                        }
                    }
                }
                opcodes::op_leave_scope => {
                    let scope = unsafe { read_from_ptr::<u16>(bc_buf_ptr.add(pos + 1)) } as i32;
                    let mut scope_idx = self.scopes[scope as usize].first;
                    while scope_idx >= 0 {
                        let vd = self.vars.get(scope_idx as usize).unwrap();
                        if vd.scope_level == scope {
                            if vd.is_captured() {
                                push_vec_u8(&mut bc_out, OpCode::CLOSE_LOC as u8);
                                push_vec_u16(&mut bc_out, scope_idx as u16);
                            }
                            scope_idx = vd.scope_next;
                        } else {
                            break;
                        }
                    }
                }
                opcodes::OP_set_name => {
                    // remove dummy set_name opcodes
                    let name =
                        Atom::from_id(unsafe { read_from_ptr::<u32>(bc_buf_ptr.add(pos + 1)) });
                    if !name.is_null() {
                        no_change!(pos, op_len);
                    }
                }
                opcodes::OP_if_false | opcodes::OP_if_true | opcodes::OP_catch => {
                    #[cfg(feature = "short_opcode")]
                    {
                        self.jump_slots_cap += 1;
                    }

                    no_change!(pos, op_len);
                }
                opcodes::OP_dup => {
                    // Optimize:
                    // Transformation: dup if_false(l1) drop, l1: if_false(l2) -> if_false(l2)
                    // Transformation: dup if_true(l1) drop, l1: if_true(l2) -> if_true(l2)
                    if OPTIMIZE
                        && let Some(cc) = self.code_match(
                            pos_next,
                            &[
                                M2(opcodes::OP_if_false as i32, opcodes::OP_if_true as i32),
                                opcodes::OP_drop as i32,
                            ],
                        )
                    {
                        let lab0 = cc.label;
                        let mut lab1 = lab0;
                        debug_assert!(lab1 >= 0 && lab1 < self.label_slots.len() as i32);

                        let op1 = cc.op;
                        let pos1 = cc.pos;
                        let line1 = cc.line_num;
                        let mut pos2: usize;
                        while let Some(cc) = self.code_match(
                            {
                                pos2 = get_label_pos(self, lab1) as _;
                                pos2
                            },
                            &[OpCode::DUP as i32, op1, OpCode::DROP as i32],
                        ) {
                            lab1 = cc.label;
                        }

                        if let Some(cc) = self.code_match(pos2, &[op1]) {
                            #[cfg(feature = "short_opcode")]
                            {
                                self.jump_slots_cap += 1;
                            }

                            self.update_label_ref_count(lab0 as _, -1);
                            self.update_label_ref_count(cc.label as _, 1);

                            push_vec_u8(&mut bc_out, op1 as u8);
                            push_vec_u32(&mut bc_out, cc.label as u32);

                            pos_next = pos1;
                            if line1 != -1 && line1 != line_num {
                                line_num = line1;
                                self.line_number_slots_cap += 1;

                                push_vec_u8(&mut bc_out, opcodes::op_line_num);
                                push_vec_u32(&mut bc_out, line_num as u32);
                                push_vec_u32(&mut bc_out, 0); // line_pos: not available in resolve
                            }
                        } else {
                            no_change!(pos, op_len);
                        }
                    } else {
                        no_change!(pos, op_len);
                    }
                }
                opcodes::OP_call | opcodes::OP_call_method => {
                    // Detect and transform tail calls:
                    //   call f, n  +  return/return_undef  →  tail_call f, n
                    //   call_method f, n  +  return/return_undef  →  tail_call_method f, n
                    // Also handles labels between call and return (e.g. ??, &&, ||):
                    //   call f, n  +  [label/line_num]*  +  return  →  tail_call f, n
                    //   + [label]* + return
                    // PTC applies only in strict mode code (ES IsInTailPosition)
                    if OPTIMIZE && self.attrs.exec_mode.contains(ExecMode::STRICT) {
                        let argc = unsafe { read_from_ptr::<u16>(bc_buf_ptr.add(pos + 1)) };
                        // Try 1: direct match (call + return, no intervening opcodes)
                        let mut tco_ok = false;
                        if let Some(cc) = self.code_match(
                            pos_next,
                            &[M2(
                                opcodes::OP_return as i32,
                                opcodes::OP_return_undef as i32,
                            )],
                        ) {
                            if cc.line_num >= 0 && cc.line_num != line_num {
                                line_num = cc.line_num;
                                self.line_number_slots_cap += 1;
                                push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                                push_vec_u32(&mut bc_out, line_num as u32);
                                push_vec_u32(&mut bc_out, 0); // line_pos: not available in resolve
                            }
                            self.add_pc2line_info(bc_out.len() as _, line_num, line_pos);

                            // op+1 maps CALL→TAIL_CALL, CALL_METHOD→TAIL_CALL_METHOD:
                            //   OP_call (N) + 1 = OP_tail_call (N+1)
                            //   OP_call_method (N+1) + 1 = OP_tail_call_method (N+2)
                            let tail_op = match op {
                                opcodes::OP_call => opcodes::OP_tail_call,
                                opcodes::OP_call_method => opcodes::OP_tail_call_method,
                                _ => unreachable!(),
                            };
                            push_vec_u8(&mut bc_out, tail_op);
                            push_vec_u16(&mut bc_out, argc);

                            pos_next = cc.pos;
                            tco_ok = true;
                        }

                        // Try 2: scan past labels/line_nums to find return.
                        // Needed for ??, &&, ||, ?: expressions where a label
                        // sits between the call and the final return.
                        // Emit tail_call and let subsequent loop iterations
                        // process the intervening labels/return normally (they
                        // become dead code after tail_call but labels must be
                        // registered in label_slots for jump resolution).
                        if !tco_ok {
                            let mut scan = pos_next;
                            let mut scan_line_num = line_num;
                            let mut found_return = false;
                            while scan < bc_buf_len {
                                let scan_op = unsafe { *bc_buf_ptr.add(scan) };
                                let scan_len = opcode_info_intermediate(scan_op).size as usize;
                                if scan + scan_len > bc_buf_len {
                                    break;
                                }
                                match scan_op {
                                    opcodes::op_line_num => {
                                        scan_line_num = unsafe {
                                            read_from_ptr::<i32>(bc_buf_ptr.add(scan + 1))
                                        };
                                        scan += scan_len;
                                    }
                                    opcodes::op_label | opcodes::OP_nip_catch => {
                                        scan += scan_len;
                                    }
                                    opcodes::OP_gosub => {
                                        // A gosub to a real try-finally body must complete
                                        // before returning, so the call is NOT in tail
                                        // position. A gosub to an empty finally region
                                        // (catch-only try) is a no-op indirection and TCO
                                        // is safe (same test as the empty-finalizer removal).
                                        let label_id =
                                            unsafe { read_from_ptr::<u32>(bc_buf_ptr.add(scan + 1)) };
                                        if self
                                            .code_match(
                                                self.label_slots[label_id as usize].pos as _,
                                                &[OpCode::RET as i32],
                                            )
                                            .is_some()
                                        {
                                            scan += scan_len;
                                        } else {
                                            break;
                                        }
                                    }
                                    opcodes::OP_return | opcodes::OP_return_undef => {
                                        found_return = true;
                                        break;
                                    }
                                    _ => break,
                                }
                            }

                            if found_return {
                                if scan_line_num >= 0 && scan_line_num != line_num {
                                    line_num = scan_line_num;
                                    self.line_number_slots_cap += 1;
                                    push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                                    push_vec_u32(&mut bc_out, line_num as u32);
                                    push_vec_u32(&mut bc_out, 0); // line_pos: not available in resolve
                                }
                                self.add_pc2line_info(bc_out.len() as _, line_num, line_pos);

                                // op+1 maps CALL→TAIL_CALL, CALL_METHOD→TAIL_CALL_METHOD:
                                //   OP_call (N) + 1 = OP_tail_call (N+1)
                                //   OP_call_method (N+1) + 1 = OP_tail_call_method (N+2)
                                // (opcodes are adjacent in the enum ordering)
                                let tail_op = match op {
                                    opcodes::OP_call => opcodes::OP_tail_call,
                                    opcodes::OP_call_method => opcodes::OP_tail_call_method,
                                    _ => unreachable!(),
                                };
                                push_vec_u8(&mut bc_out, tail_op);
                                push_vec_u16(&mut bc_out, argc);

                                // Do NOT skip past labels/return that follow — let the
                                // normal dispatch handle them (labels must be registered
                                // in label_slots for jump resolution, return is dead code).
                                tco_ok = true;
                            }
                        }

                        // Try 3: CALL + GOTO ⟶ label ⟶ RETURN (for ?: expressions)
                        if !tco_ok {
                            // Scan past any intervening op_line_num between call and goto
                            let mut goto_pos = pos_next;
                            loop {
                                let skip_op = unsafe { *bc_buf_ptr.add(goto_pos) };
                                let skip_len = opcode_info_intermediate(skip_op).size as usize;
                                if skip_op == opcodes::op_line_num {
                                    goto_pos += skip_len;
                                } else {
                                    break;
                                }
                            }
                            let goto_op = unsafe { *bc_buf_ptr.add(goto_pos) };
                            let goto_len = opcode_info_intermediate(goto_op).size as usize;
                            if goto_op == opcodes::OP_goto && goto_pos + goto_len <= bc_buf_len {
                                let goto_label =
                                    unsafe { read_from_ptr::<u32>(bc_buf_ptr.add(goto_pos + 1)) };
                                let mut scan = goto_pos + goto_len;
                                let mut scan_line_num = line_num;
                                let mut found = false;
                                while scan < bc_buf_len {
                                    let s_op = unsafe { *bc_buf_ptr.add(scan) };
                                    let s_len = opcode_info_intermediate(s_op).size as usize;
                                    if scan + s_len > bc_buf_len {
                                        break;
                                    }

                                    match s_op {
                                        opcodes::op_label => {
                                            let label_id = unsafe {
                                                read_from_ptr::<u32>(bc_buf_ptr.add(scan + 1))
                                            };
                                            if label_id == goto_label {
                                                let after = scan + s_len;
                                                let next = unsafe { *bc_buf_ptr.add(after) };
                                                if matches!(
                                                    next,
                                                    opcodes::OP_return | opcodes::OP_return_undef
                                                ) {
                                                    found = true;
                                                }
                                                break;
                                            }
                                            // Not the target label (e.g. L_else in ?:) —
                                            // continue scanning for the right one.
                                        }
                                        opcodes::op_line_num => {
                                            scan_line_num = unsafe {
                                                read_from_ptr::<i32>(bc_buf_ptr.add(scan + 1))
                                            };
                                        }
                                        _ => {}
                                    }

                                    scan += s_len;
                                }

                                if found {
                                    if scan_line_num >= 0 && scan_line_num != line_num {
                                        line_num = scan_line_num;
                                        self.line_number_slots_cap += 1;
                                        push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                                        push_vec_u32(&mut bc_out, line_num as u32);
                                        push_vec_u32(&mut bc_out, 0); // line_pos: not available in resolve
                                    }
                                    self.add_pc2line_info(bc_out.len() as _, line_num, line_pos);
                                    let tail_op = match op {
                                        opcodes::OP_call => opcodes::OP_tail_call,
                                        opcodes::OP_call_method => opcodes::OP_tail_call_method,
                                        _ => unreachable!(),
                                    };
                                    push_vec_u8(&mut bc_out, tail_op);
                                    push_vec_u16(&mut bc_out, argc);
                                    tco_ok = true;
                                }
                            }
                        }

                        if !tco_ok {
                            no_change!(pos, op_len);
                        }
                    } else {
                        no_change!(pos, op_len);
                    }
                }
                opcodes::OP_nop => {}
                opcodes::op_set_class_name => {
                    // only used during parsing
                }
                opcodes::op_get_field_opt_chain => {
                    let payload_len = op_len - 1;
                    bc_out.push(opcodes::OP_get_field);
                    bc_out.extend_from_slice(unsafe {
                        std::slice::from_raw_parts(bc_buf_ptr.add(pos + 1), payload_len)
                    });
                }
                opcodes::op_get_array_el_opt_chain => {
                    bc_out.push(opcodes::OP_get_array_el);
                }
                _ => {
                    // Default: copy opcode as-is
                    no_change!(pos, op_len);
                }
            }

            // check buffer capacity, if no more space left, grow the buffer
            if bc_out.capacity() - bc_out.len() < 16 && bc_out.try_reserve(64).is_err() {
                // can't alloc memory, fail
                return Err(ctx.throw_out_of_memory());
            }

            pos = pos_next;
        } // while

        // New bytecode is done, set it to FunctionDef
        bc_out.shrink_to_fit();
        self.byte_code = bc_out;

        Ok(())
    }
}

fn mark_eval_captured_variables(s: &mut FunctionDef, scope_level: i32) {
    let mut idx = s.scopes[scope_level as usize].first;
    while idx >= 0 {
        let vd = s.vars.get_mut(idx as usize).unwrap();
        vd.set_is_captured(true);
        idx = vd.scope_next;
    }
}
