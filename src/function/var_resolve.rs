use fallible_collections::FallibleVec;

use crate::{
    atom::{consts::*, JSAtom, JS_DupAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{opcode_info_intermediate, opcodes, OpCode, OpCodeTemp},
    context::JSContext,
    error::JS_THROW_VAR_REDECL,
    eval::JS_EVAL_TYPE_DIRECT,
    function::{
        get_label_pos, instantiate_hoisted_definitions, mark_eval_captured_variables,
        resolve_scope_private_field, skip_dead_code, JSFunctionDef, JSVarKindEnum,
    },
    utils::{get_u16, get_u32, push_vec_slice, push_vec_u16, push_vec_u32, push_vec_u8, M2},
};

use super::{
    optimize::{CodeContext, OPTIMIZE},
    DEFINE_GLOBAL_FUNC_VAR, DEFINE_GLOBAL_LEX_VAR,
};

impl JSFunctionDef {
    /// Performs initial runtime checks for global variables
    /// This must be executed before creating any variables
    /// Checks for potential conflicts between global and closure variables
    unsafe fn resolve_variables_check_global_vars(
        &self,
        ctx: &mut JSContext,
        bc_buf: &mut Vec<u8>,
    ) {
        'g_var: for hf in self.global_vars.iter() {
            // check if global variable (XXX: simplify)
            if let Some(cvars) = self.closure_vars.as_ref() {
                for cv in cvars {
                    match cv.var_name {
                        JS_ATOM__var_ | JS_ATOM__arg_var_ => {
                            continue 'g_var;
                        }
                        name if name == hf.var_name => {
                            if self.eval_type == JS_EVAL_TYPE_DIRECT && cv.is_lexical() {
                                // Check if a lexical variable is
                                // redefined as 'var'. XXX: Could abort
                                // compilation here, but for consistency
                                // with the other checks, we delay the
                                // error generation.
                                push_vec_u8(bc_buf, OpCode::THROW_ERROR as u8);
                                push_vec_u32(bc_buf, JS_DupAtom(ctx, hf.var_name).0);
                                push_vec_u8(bc_buf, JS_THROW_VAR_REDECL);
                            }
                            continue 'g_var;
                        }
                        _ => {}
                    }
                }
            }

            push_vec_u8(bc_buf, OpCode::CHECK_DEFINE_VAR as u8);
            push_vec_u32(bc_buf, JS_DupAtom(ctx, hf.var_name).0);

            let mut flags = 0;
            if hf.is_lexical() {
                flags |= DEFINE_GLOBAL_LEX_VAR;
            }
            if hf.cpool_idx >= 0 {
                flags |= DEFINE_GLOBAL_FUNC_VAR;
            }
            push_vec_u8(bc_buf, flags);
        }
    }
}

/// Resolves variable references in the function definition
/// Converts global variable accesses to local/closure variables when needed
/// This is the second pass of bytecode generation
/// Handles scope resolution and variable binding
pub unsafe fn resolve_variables(ctx: *mut JSContext, s: *mut JSFunctionDef) -> Result<(), i32> {
    let bc_buf = (*s).byte_code.as_mut_ptr();
    let bc_len = (*s).byte_code.len();

    let mut cc: CodeContext = std::mem::zeroed();
    cc.bc_buf = bc_buf;
    cc.bc_len = bc_len;

    // TODO: use SmallVec
    let mut bc_out = Vec::<u8>::try_with_capacity(bc_len + 64).map_err(|_| {
        (*ctx).JS_ThrowOutOfMemory();
        -1
    })?;

    // first pass for runtime checks (must be done before the variables are created)
    (*s).resolve_variables_check_global_vars(ctx.as_mut().unwrap(), &mut bc_out);

    macro_rules! no_change {
        ($pos:expr, $len:expr) => {
            push_vec_slice(&mut bc_out, bc_buf.add($pos as usize), $len as usize)
                .expect("TODO: handle bytecode alloc exception");
        };
    }

    let mut line_num = 0_i32;
    let mut pos = 0_usize;
    let mut pos_next: usize;

    while pos < bc_len {
        let op = *bc_buf.add(pos as usize);
        let oi = opcode_info_intermediate(op);
        let len = oi.size as usize;
        pos_next = pos + len;

        match op {
            opcodes::op_line_num => {
                line_num = get_u32(bc_buf.add(pos + 1)) as i32;
                (*s).line_number_size += 1;
                no_change!(pos, len);
            }
            opcodes::OP_eval => {
                // convert scope index to adjusted variable index
                let call_argc = get_u16(bc_buf.add(pos + 1));
                let scope = get_u16(bc_buf.add(pos + 1 + 2));
                mark_eval_captured_variables(ctx, s, scope as _);
                push_vec_u8(&mut bc_out, op);
                push_vec_u16(&mut bc_out, call_argc);
                push_vec_u16(&mut bc_out, ((*s).scopes[scope as usize].first + 1) as _);
            }
            opcodes::OP_apply_eval => {
                // convert scope index to adjusted variable index
                let scope = get_u16(bc_buf.add(pos + 1));
                mark_eval_captured_variables(ctx, s, scope as _);
                push_vec_u8(&mut bc_out, op);
                push_vec_u16(&mut bc_out, ((*s).scopes[scope as usize].first + 1) as _);
            }
            opcodes::op_scope_get_var_checkthis
            | opcodes::op_scope_get_var_undef
            | opcodes::op_scope_get_var
            | opcodes::op_scope_put_var
            | opcodes::op_scope_delete_var
            | opcodes::op_scope_get_ref
            | opcodes::op_scope_put_var_init => {
                let var_name = JSAtom(get_u32(bc_buf.add(pos + 1))).into_rc((*ctx).rt());
                let scope = get_u16(bc_buf.add(pos + 5));

                pos_next = (*s).resolve_scope_var(
                    ctx.as_mut().unwrap(),
                    *var_name,
                    scope as _,
                    op,
                    &mut bc_out,
                    std::ptr::null_mut(),
                    None,
                    pos_next,
                ) as usize;
            }
            opcodes::op_scope_make_ref => {
                let var_name = JSAtom(get_u32(bc_buf.add(pos + 1)));
                let label_id = get_u32(bc_buf.add(pos + 5));
                let scope = get_u16(bc_buf.add(pos + 9));

                let lb = (*s).labels.get_mut(label_id as usize).unwrap();
                lb.ref_count -= 1; // always remove label reference

                // debug: get atom refcount before resolve_scope_var()
                #[cfg(debug_assertions)]
                let _name_refcnt = var_name.ref_count((*ctx).rt());

                pos_next = (*s).resolve_scope_var(
                    ctx.as_mut().unwrap(),
                    var_name,
                    scope as _,
                    op,
                    &mut bc_out,
                    bc_buf,
                    Some(lb),
                    pos_next,
                );

                // check atom refcount after resolve_scope_var()
                #[cfg(debug_assertions)]
                debug_assert!(
                    var_name.ref_count((*ctx).rt()) >= _name_refcnt,
                    "[O.o] {var_name:?}, {}",
                    var_name.to_str((*ctx).rt())
                );

                JS_FreeAtom(ctx, var_name);
            }
            opcodes::op_scope_get_private_field
            | opcodes::op_scope_get_private_field2
            | opcodes::op_scope_put_private_field
            | opcodes::op_scope_in_private_field => {
                let var_name = JSAtom(get_u32(bc_buf.add(pos + 1)));
                let scope = get_u16(bc_buf.add(pos + 5));
                if resolve_scope_private_field(
                    ctx.as_mut().unwrap(),
                    s,
                    var_name,
                    scope as i32,
                    op,
                    &mut bc_out,
                ) < 0
                {
                    // fail: continue the copy to keep the atom refcounts consistent
                    // XXX: find a better solution ? */
                    while pos < bc_len {
                        let op_len1 =
                            opcode_info_intermediate(*bc_buf.add(pos as usize)).size as usize;
                        pos_next = pos + op_len1;
                        if push_vec_slice(&mut bc_out, bc_buf.add(pos as usize), op_len1).is_err() {
                            ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
                            break;
                        }
                        pos = pos_next;
                    }
                    bc_out.shrink_to_fit();
                    (*s).byte_code = bc_out;
                    return Err(-1);
                }

                JS_FreeAtom(ctx, var_name);
            }
            opcodes::OP_gosub => {
                (*s).jump_size += 1;
                if OPTIMIZE {
                    // remove calls to empty finalizers
                    let label_id = get_u32(bc_buf.add(pos + 1));
                    debug_assert!(label_id < (*s).labels.len() as u32);
                    let lb = (*s).labels.get_mut(label_id as usize).unwrap();
                    if cc.code_match(lb.pos as _, &[OpCode::RET as i32]) {
                        lb.ref_count -= 1;
                    } else {
                        no_change!(pos, len);
                    }
                } else {
                    no_change!(pos, len);
                }
            }
            opcodes::OP_drop => {
                // if (0) {
                //     /* remove drops before return_undef */
                //     /* do not perform this optimization in pass2 because
                //        it breaks patterns recognised in resolve_labels */
                //     int pos1 = pos_next;
                //     int line1 = line_num;
                //     while (code_match(&cc, pos1, OP_drop, -1)) {
                //         if (cc.line_num >= 0) line1 = cc.line_num;
                //         pos1 = cc.pos;
                //     }
                //     if (code_match(&cc, pos1, OP_return_undef, -1)) {
                //         pos_next = pos1;
                //         if (line1 != -1 && line1 != line_num) {
                //             line_num = line1;
                //             s->line_number_size++;
                //             dbuf_putc(&bc_out, op_line_num);
                //             dbuf_put_u32(&bc_out, line_num);
                //         }
                //         break;
                //     }
                // }
                no_change!(pos, len);
            }
            opcodes::OP_insert3 => {
                // Optimize transformation:
                // insert3 put_array_el|put_ref_value drop -> put_array_el|put_ref_value */
                if OPTIMIZE
                    && cc.code_match(
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
                        (*s).line_number_size += 1;
                        push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                        push_vec_u32(&mut bc_out, line_num as u32);
                    }
                } else {
                    no_change!(pos, len);
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
                if op == opcodes::OP_goto {
                    (*s).jump_size += 1;
                }

                if OPTIMIZE {
                    // remove dead code
                    push_vec_slice(&mut bc_out, bc_buf.add(pos), len as _).expect("TODO");

                    let mut line = -1_i32;
                    pos = skip_dead_code(s, bc_buf, bc_len, pos + len, &mut line);
                    pos_next = pos;
                    if pos < bc_len && line >= 0 && line_num != line {
                        line_num = line;
                        (*s).line_number_size += 1;
                        push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                        push_vec_u32(&mut bc_out, line_num as u32);
                    }
                } else {
                    no_change!(pos, len);
                }
            }
            opcodes::op_label => {
                let label_id = get_u32(bc_buf.add(pos + 1));
                debug_assert!(label_id < (*s).labels.len() as u32);
                let lb = (*s).labels.get_mut(label_id as usize).unwrap();
                debug_assert_eq!(lb.pos2, -1);
                lb.pos2 = bc_out.len() as i32 + oi.size as i32;
                no_change!(pos, len);
            }
            opcodes::op_enter_scope => {
                let scope = get_u16(bc_buf.add(pos as usize + 1)) as i32;
                if scope == (*s).body_scope {
                    instantiate_hoisted_definitions(ctx, s, &mut bc_out);
                }

                let mut scope_idx = (*s).scopes[scope as usize].first;
                while scope_idx >= 0 {
                    let vd = (*s).vars.get(scope_idx as usize).unwrap();
                    if vd.scope_level == scope {
                        if scope_idx as u16 != (*s).arguments_arg_idx.unwrap_or(u16::MAX) {
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
                let scope = get_u16(bc_buf.add(pos as usize + 1)) as i32;
                let mut scope_idx = (*s).scopes[scope as usize].first;
                while scope_idx >= 0 {
                    let vd = (*s).vars.get(scope_idx as usize).unwrap();
                    if vd.scope_level == scope {
                        if vd.is_captured() {
                            push_vec_u8(&mut bc_out, OpCode::CLOSE_LOC as u8);
                            push_vec_u16(&mut bc_out, scope_idx as u16);
                        }
                        scope_idx = (*vd).scope_next;
                    } else {
                        break;
                    }
                }
            }
            opcodes::OP_set_name => {
                /* remove dummy set_name opcodes */
                let name = JSAtom(get_u32(bc_buf.add(pos as usize + 1)));
                if name != JS_ATOM_NULL {
                    no_change!(pos, len);
                }
            }
            opcodes::OP_if_false | opcodes::OP_if_true | opcodes::OP_catch => {
                (*s).jump_size += 1;
                no_change!(pos, len);
            }
            opcodes::OP_dup => {
                // Optimize:
                // Transformation: dup if_false(l1) drop, l1: if_false(l2) -> if_false(l2) /
                // Transformation: dup if_true(l1) drop, l1: if_true(l2) -> if_true(l2)
                if OPTIMIZE
                    && cc.code_match(
                        pos_next,
                        &[
                            M2(opcodes::OP_if_false as i32, opcodes::OP_if_true as i32),
                            opcodes::OP_drop as i32,
                        ],
                    )
                {
                    let lab0 = cc.label;
                    let mut lab1 = lab0;
                    debug_assert!(lab1 >= 0 && lab1 < (*s).labels.len() as i32);

                    let op1 = cc.op;
                    let pos1 = cc.pos;
                    let line1 = cc.line_num;
                    let mut pos2: usize;
                    while cc.code_match(
                        {
                            pos2 = get_label_pos(s, lab1) as _;
                            pos2
                        },
                        &[OpCode::DUP as i32, op1, OpCode::DROP as i32],
                    ) {
                        lab1 = cc.label;
                    }

                    if cc.code_match(pos2, &[op1]) {
                        (*s).jump_size += 1;

                        (*s).update_label_ref_count(lab0 as _, -1);
                        (*s).update_label_ref_count(cc.label as _, 1);

                        push_vec_u8(&mut bc_out, op1 as u8);
                        push_vec_u32(&mut bc_out, cc.label as u32);

                        pos_next = pos1;
                        if line1 != -1 && line1 != line_num {
                            line_num = line1;
                            (*s).line_number_size += 1;

                            push_vec_u8(&mut bc_out, opcodes::op_line_num);
                            push_vec_u32(&mut bc_out, line_num as u32);
                        }
                    } else {
                        no_change!(pos, len);
                    }
                } else {
                    no_change!(pos, len);
                }
            }
            opcodes::OP_nop => { /* remove erased code */ }
            opcodes::op_set_class_name => { /* only used during parsing */ }
            opcodes::op_get_field_opt_chain => {
                let name = get_u32(bc_buf.add(pos as usize + 1));
                bc_out.push(opcodes::OP_get_field);
                bc_out.extend_from_slice(name.to_ne_bytes().as_slice());
            }
            opcodes::op_get_array_el_opt_chain => {
                bc_out.push(opcodes::OP_get_array_el);
            }
            _ => {
                // Default: copy opcode as-is
                no_change!(pos, len);
            }
        }

        // check buffer capacity, if no more space left, grow the buffer
        if bc_out.capacity() - bc_out.len() < 16 {
            if bc_out.try_reserve(64).is_err() {
                // can't alloc memory, fail
                ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        pos = pos_next;
    } // while

    // New bytecode is done, set it to JSFunctionDef
    // (*s).byte_code.clear(); // not nessicary, rust will drop the vec automatically
    bc_out.shrink_to_fit();
    (*s).byte_code = bc_out;

    Ok(())
}

/// Resolves special pseudo variables (this, new.target, etc.)
/// Returns the variable definition index if found
/// Returns None if the variable is not a pseudo variable
pub unsafe fn resolve_pseudo_var(
    ctx: *mut JSContext,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
) -> Option<u16> {
    if !(*s).has_this_binding {
        return None;
    }

    let ctx = ctx.as_mut().unwrap();

    match var_name {
        JS_ATOM__home_object_ => {
            // 'home_object' pseudo variable
            if (*s).home_object_var_idx.is_none() {
                (*s).home_object_var_idx
                    .replace((*s).add_var(ctx, var_name).expect("TODO"));
            }
            (*s).home_object_var_idx
        }
        JS_ATOM_this_active_func => {
            // 'this.active_func' pseudo variable
            if (*s).this_active_func_var_idx.is_none() {
                (*s).this_active_func_var_idx
                    .replace((*s).add_var(ctx, var_name).expect("TODO"));
            }
            (*s).this_active_func_var_idx
        }
        JS_ATOM_new_target => {
            // 'new.target' pseudo variable
            if (*s).new_target_var_idx.is_none() {
                (*s).new_target_var_idx
                    .replace((*s).add_var(ctx, var_name).expect("TODO"));
            }
            (*s).new_target_var_idx
        }
        JS_ATOM_this => {
            // 'this' pseudo variable
            if (*s).this_var_idx.is_none() {
                (*s).this_var_idx = Some((*s).add_var_this(ctx).unwrap());
            }
            (*s).this_var_idx
        }
        _ => None,
    }
}
