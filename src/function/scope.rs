use crate::{
    atom::{consts::*, JSAtom, JS_DupAtom},
    bytecode::{opcodes, OpCode},
    context::JSContext,
    error::JS_THROW_VAR_RO,
    function::{
        add_arguments_var, add_func_var, resolve_pseudo_var, var_object_test, JSFunctionDef,
        JSVarKindEnum, LabelInfo, ARGUMENT_VAR_OFFSET, ARG_SCOPE_END,
    },
    utils::{push_vec_u16, push_vec_u32, push_vec_u8},
};

use super::optimize::{
    can_opt_put_global_ref_value, can_opt_put_ref_value, optimize_scope_make_global_ref,
    optimize_scope_make_ref,
};

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JSVarScope {
    pub parent: i32, // index into fd->scopes of the enclosing scope
    pub first: i32,  // index into fd->vars of the last variable in this scope
}

impl JSFunctionDef {
    /// define a variable in the current scope
    pub unsafe fn add_scope_var(
        &mut self,
        ctx: &mut JSContext,
        name: JSAtom,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        let idx = self.add_var(ctx, name)?;

        let vd = self.vars.get_mut(idx as usize).unwrap();
        vd.set_var_kind(var_kind);
        vd.scope_level = self.scope_level;
        vd.scope_next = self.scope_first;

        self.scopes
            .get_mut((*self).scope_level as usize)
            .unwrap()
            .first = idx as i32;

        self.scope_first = idx as i32;

        Ok(idx)
    }

    // find a 'var' declaration in same or descendant scope of `scope_level`
    pub fn find_var_in_child_scope(&self, name: JSAtom, scope_level: i32) -> Option<u16> {
        self.vars
            .iter()
            .position(|vd| {
                vd.var_name == name
                    && vd.scope_level == 0
                    && is_child_scope(self, vd.scope_next, scope_level)
            })
            .map(|x| x as u16)

        // for i in 0..self.var_count {
        //     let vd = self.vars.add(i as usize).as_mut().unwrap();
        //     if vd.var_name == name && vd.scope_level == 0 {
        //         if is_child_scope(self, vd.scope_next, scope_level) {
        //             return Some(i);
        //         }
        //     }
        // }
        // None
    }
}

/* return true if scope == parent_scope or if scope is a child of parent_scope */
pub fn is_child_scope(fd: *const JSFunctionDef, mut scope: i32, parent_scope: i32) -> bool {
    while scope >= 0 {
        if scope == parent_scope {
            return true;
        }
        scope = unsafe { (*fd).scopes[scope as usize].parent };
    }

    return false;
}

/// return the position of the next opcode
pub unsafe fn resolve_scope_var(
    ctx: *mut JSContext,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
    mut scope_level: i32,
    op: u8,
    bc: &mut Vec<u8>,
    bc_buf: *mut u8, // JSFunctionDef::byte_code buffer ptr
    label: Option<&LabelInfo>,
    mut pos_next: usize,
) -> usize {
    let ctx = ctx.as_mut().unwrap();

    macro_rules! done {
        ($label:expr) => {
            if let Some(l) = $label {
                push_vec_u8(bc, opcodes::op_label);
                push_vec_u32(bc, l);
                (*s).label_slots[l as usize].pos2 = bc.len() as i32;
            }
            return pos_next;
        };
    }

    // XXX: could be simpler to use a specific function to
    // resolve the pseudo variables
    let is_pseudo_var = matches!(
        var_name,
        JS_ATOM__home_object_ | JS_ATOM_this_active_func | JS_ATOM_new_target | JS_ATOM_this
    );

    let mut label_done: Option<u32> = None;

    // resolve local scoped variables
    let mut var_idx: i32 = -1;
    let mut idx = (*s).scopes[scope_level as usize].first;
    while idx >= 0 {
        let vd = (*s).vars.get(idx as usize).unwrap();
        if vd.var_name == var_name {
            if matches!(op, opcodes::op_scope_put_var | opcodes::op_scope_make_ref) {
                if vd.is_const() {
                    push_vec_u8(bc, opcodes::OP_throw_error);
                    push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                    push_vec_u8(bc, JS_THROW_VAR_RO);
                    done!(label_done);
                }
            }
            var_idx = idx;
            break;
        }

        if vd.var_name == JS_ATOM__with_ && !is_pseudo_var {
            push_vec_u8(bc, opcodes::OP_get_loc);
            push_vec_u16(bc, idx as _);
            var_object_test(ctx, s, var_name, op, bc, &mut label_done, true);
        }

        idx = vd.scope_next;
    }

    let mut is_arg_scope = idx == ARG_SCOPE_END;
    if var_idx < 0 {
        // argument scope: variables are not visible but pseudo
        // variables are visible
        if !is_arg_scope {
            var_idx = (*s).find_var(var_name).unwrap_or(-1);
        }
        if var_idx < 0 && is_pseudo_var {
            var_idx = resolve_pseudo_var(ctx, s, var_name).map_or(-1, |x| x as i32);
        }
        if var_idx < 0 && var_name == JS_ATOM_arguments && (*s).has_arguments_binding {
            // 'arguments' pseudo variable
            var_idx = (*s).add_arguments(ctx, false).map_or(-1, |x| x as i32);
        }
        if var_idx < 0 && (*s).is_func_expr && var_name == (*s).func_name {
            // add a new variable with the function name
            var_idx = (*s).add_func_var(ctx, var_name).map_or(-1, |x| x as i32)
        }
    }

    if var_idx >= 0 {
        if var_idx & ARGUMENT_VAR_OFFSET == 0
            && matches!(
                op as u8,
                opcodes::op_scope_put_var | opcodes::op_scope_make_ref
            )
            && (*s).vars.get(var_idx as usize).unwrap().is_const()
        {
            // only happens when assigning a function expression name
            // in strict mode
            push_vec_u8(bc, opcodes::OP_throw_error);
            push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
            push_vec_u8(bc, JS_THROW_VAR_RO);
            done!(label_done);
        };

        // op_scope_put_var_init is only used to initialize a
        // lexical variable, so it is never used in a with or var object. It
        // can be used with a closure (module global variable case).
        match op as u8 {
            opcodes::op_scope_make_ref => {
                debug_assert!(label.is_some());

                if var_idx & ARGUMENT_VAR_OFFSET == 0
                    && (*s).vars.get(var_idx as usize).unwrap().var_kind()
                        == JSVarKindEnum::JS_VAR_FUNCTION_NAME
                {
                    // Create a dummy object reference for the func_var
                    push_vec_u8(bc, opcodes::OP_object);
                    push_vec_u8(bc, opcodes::OP_get_loc);
                    push_vec_u16(bc, var_idx as u16);
                    push_vec_u8(bc, opcodes::OP_define_field);
                    push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                    push_vec_u8(bc, opcodes::OP_push_atom_value);
                    push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                } else if label_done.is_none() && can_opt_put_ref_value(bc_buf, label.unwrap().pos)
                {
                    let get_op = if var_idx & ARGUMENT_VAR_OFFSET != 0 {
                        var_idx -= ARGUMENT_VAR_OFFSET;
                        OpCode::GET_ARG
                    } else {
                        if (*s).vars.get(var_idx as usize).unwrap().is_lexical() {
                            OpCode::GET_LOC_CHECK
                        } else {
                            OpCode::GET_LOC
                        }
                    };
                    pos_next = optimize_scope_make_ref(
                        ctx,
                        s,
                        bc,
                        bc_buf,
                        label.unwrap(),
                        pos_next,
                        get_op,
                        var_idx,
                    );
                } else {
                    // Create a dummy object with a named slot that is
                    // a reference to the local variable
                    if var_idx & ARGUMENT_VAR_OFFSET != 0 {
                        push_vec_u8(bc, opcodes::OP_make_arg_ref);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                        push_vec_u16(bc, (var_idx - ARGUMENT_VAR_OFFSET) as _);
                    } else {
                        push_vec_u8(bc, opcodes::OP_make_loc_ref);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                        push_vec_u16(bc, var_idx as u16);
                    }
                }
            }
            opcodes::op_scope_get_ref
            | opcodes::op_scope_get_var_checkthis
            | opcodes::op_scope_get_var_undef
            | opcodes::op_scope_get_var
            | opcodes::op_scope_put_var
            | opcodes::op_scope_put_var_init => {
                debug_assert!(label.is_none());

                if op == opcodes::op_scope_get_ref {
                    push_vec_u8(bc, opcodes::OP_undefined);
                }

                let is_put = matches!(
                    op as u8,
                    opcodes::op_scope_put_var | opcodes::op_scope_put_var_init
                );

                if var_idx & ARGUMENT_VAR_OFFSET != 0 {
                    push_vec_u8(
                        bc,
                        if is_put {
                            OpCode::PUT_ARG
                        } else {
                            OpCode::GET_ARG
                        } as u8,
                    );
                    push_vec_u16(bc, (var_idx - ARGUMENT_VAR_OFFSET) as u16);
                } else {
                    let vd = (*s).vars.get(var_idx as usize).unwrap();
                    let is_lexical = vd.is_lexical();
                    if is_put {
                        // scope_put_XXX
                        if is_lexical {
                            if op == opcodes::op_scope_put_var_init {
                                push_vec_u8(
                                    bc,
                                    if var_name == JS_ATOM_this {
                                        // 'this' can only be initialized once
                                        OpCode::PUT_LOC_CHECK_INIT
                                    } else {
                                        OpCode::PUT_LOC
                                    } as u8,
                                );
                            } else {
                                push_vec_u8(bc, OpCode::PUT_LOC_CHECK as u8);
                            }
                        } else {
                            push_vec_u8(bc, OpCode::PUT_LOC as u8);
                        }
                    } else {
                        // scope_get_XXX
                        if is_lexical {
                            push_vec_u8(
                                bc,
                                if op == opcodes::op_scope_get_var_checkthis {
                                    // only used for 'this' return in derived class constructors
                                    OpCode::GET_LOC_CHECKTHIS
                                } else {
                                    OpCode::GET_LOC_CHECK
                                } as u8,
                            );
                        } else {
                            push_vec_u8(bc, OpCode::GET_LOC as u8);
                        }
                    }
                    push_vec_u16(bc, var_idx as _);
                }
            }
            opcodes::op_scope_delete_var => {
                push_vec_u8(bc, opcodes::OP_push_false);
            }
            _ => {}
        }

        done!(label_done);
    }

    // check eval object
    if !is_arg_scope && (*s).var_object_idx.is_some() && !is_pseudo_var {
        push_vec_u8(bc, opcodes::OP_get_loc);
        push_vec_u16(bc, (*s).var_object_idx.unwrap());
        var_object_test(ctx, s, var_name, op, bc, &mut label_done, false);
    }
    // check eval object in argument scope
    if (*s).arg_var_object_idx.is_some() && !is_pseudo_var {
        push_vec_u8(bc, opcodes::OP_get_loc);
        push_vec_u16(bc, (*s).arg_var_object_idx.unwrap());
        var_object_test(ctx, s, var_name, op, bc, &mut label_done, false);
    }

    // check parent scopes
    let mut fd = s;
    while !(*fd).parent.is_null() {
        scope_level = (*fd).parent_scope_level;
        fd = (*fd).parent;
        idx = (*fd).scopes[scope_level as usize].first;

        while idx >= 0 {
            let vd = (*fd).vars.get_mut(idx as usize).unwrap();
            if vd.var_name == var_name {
                if matches!(
                    op as u8,
                    opcodes::op_scope_put_var | opcodes::op_scope_make_ref
                ) {
                    if vd.is_const() {
                        push_vec_u8(bc, opcodes::OP_throw_error);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                        push_vec_u8(bc, JS_THROW_VAR_RO);
                        done!(label_done);
                    }
                }
                var_idx = idx;
                break;
            }

            if vd.var_name == JS_ATOM__with_ && !is_pseudo_var {
                vd.set_is_captured(true);
                match (*s).get_closure_var2(
                    ctx,
                    fd,
                    true,
                    false,
                    idx as u16,
                    vd.var_name,
                    false,
                    false,
                    JSVarKindEnum::JS_VAR_NORMAL,
                ) {
                    Ok(vi) => {
                        idx = vi as i32;
                        push_vec_u8(bc, opcodes::OP_get_var_ref);
                        push_vec_u16(bc, idx as _);
                        var_object_test(ctx, s, var_name, op, bc, &mut label_done, true);
                    }
                    Err(_) => {
                        idx = -1;
                    }
                }
            }
            idx = vd.scope_next;
        }

        is_arg_scope = idx == ARG_SCOPE_END;
        if var_idx >= 0 {
            break;
        }

        if !is_arg_scope {
            var_idx = (*fd).find_var(var_name).unwrap_or(-1);
            if var_idx >= 0 {
                break;
            }
        }
        if is_pseudo_var {
            if resolve_pseudo_var(ctx, fd, var_name).is_some() {
                break;
            } else {
                var_idx = -1;
            }
        }
        if var_name == JS_ATOM_arguments && (*fd).has_arguments_binding {
            var_idx = add_arguments_var(ctx, fd);
            break;
        }
        if (*fd).is_func_expr && (*fd).func_name == var_name {
            // add a new variable with the function name
            var_idx = add_func_var(ctx, fd, var_name);
            break;
        }

        // check eval object
        if !is_arg_scope && (*fd).var_object_idx.is_some() && !is_pseudo_var {
            let vd1 = (*fd)
                .vars
                .get_mut((*fd).var_object_idx.unwrap() as usize)
                .unwrap();
            vd1.set_is_captured(true);
            idx = (*s)
                .get_closure_var2(
                    ctx,
                    fd,
                    true,
                    false,
                    (*fd).var_object_idx.unwrap(),
                    vd1.var_name,
                    false,
                    false,
                    JSVarKindEnum::JS_VAR_NORMAL,
                )
                .unwrap() as i32;

            push_vec_u8(bc, opcodes::OP_get_var_ref);
            push_vec_u16(bc, idx as _);
            var_object_test(ctx, s, var_name, op, bc, &mut label_done, false);
        }

        // check eval object in argument scope
        if !is_pseudo_var && (*fd).arg_var_object_idx.is_some() {
            let vd2 = (*fd)
                .vars
                .get_mut((*fd).arg_var_object_idx.unwrap() as usize)
                .unwrap();
            vd2.set_is_captured(true);
            idx = (*s)
                .get_closure_var2(
                    ctx,
                    fd,
                    true,
                    false,
                    (*fd).arg_var_object_idx.unwrap(),
                    vd2.var_name,
                    false,
                    false,
                    JSVarKindEnum::JS_VAR_NORMAL,
                )
                .unwrap() as i32;

            push_vec_u8(bc, opcodes::OP_get_var_ref);
            push_vec_u16(bc, idx as _);
            var_object_test(ctx, s, var_name, op, bc, &mut label_done, false);
        }

        if (*fd).is_eval {
            break; // it it necessarily the top level function
        }
    }

    // check direct eval scope (in the closure of the eval function
    // which is necessarily at the top level)
    if fd.is_null() {
        fd = s;
    }

    // state flag. TODO: use Option<i32> to indicate closure var index found.
    let mut has_idx = false;

    if var_idx < 0 && (*fd).is_eval {
        if let Some(cvars) = (*fd).closure_vars.as_ref() {
            for (idx1, cv) in cvars.iter().enumerate() {
                if var_name == cv.var_name {
                    idx = if fd != s {
                        (*s).get_closure_var2(
                            ctx,
                            fd,
                            false,
                            cv.is_arg(),
                            idx1 as _,
                            cv.var_name,
                            cv.is_const(),
                            cv.is_lexical(),
                            cv.var_kind(),
                        )
                        .expect("can't fail") as i32
                    } else {
                        idx1 as i32
                    };

                    has_idx = true;
                    break;
                } else if !is_pseudo_var
                    && matches!(
                        (*cv).var_name,
                        JS_ATOM__var_ | JS_ATOM__arg_var_ | JS_ATOM__with_
                    )
                {
                    let is_with = cv.var_name == JS_ATOM__with_;
                    idx = if fd != s {
                        (*s).get_closure_var2(
                            ctx,
                            fd,
                            false,
                            cv.is_arg(),
                            idx1 as _,
                            cv.var_name,
                            false,
                            false,
                            JSVarKindEnum::JS_VAR_NORMAL,
                        )
                        .expect("can't fail") as i32
                    } else {
                        idx1 as i32
                    };

                    push_vec_u8(bc, opcodes::OP_get_var_ref);
                    push_vec_u16(bc, idx as _);
                    var_object_test(ctx, s, var_name, op, bc, &mut label_done, is_with);
                }
            }
        }
    }

    if var_idx >= 0 {
        // find the corresponding closure variable
        idx = if var_idx & ARGUMENT_VAR_OFFSET != 0 {
            (*fd)
                .args
                .get_mut((var_idx - ARGUMENT_VAR_OFFSET) as usize)
                .unwrap()
                .set_is_captured(true);

            (*s).get_closure_var2(
                ctx,
                fd,
                true,
                true,
                (var_idx - ARGUMENT_VAR_OFFSET) as u16,
                var_name,
                false,
                false,
                JSVarKindEnum::JS_VAR_NORMAL,
            )
        } else {
            let vd3 = (*fd).vars.get_mut(var_idx as usize).unwrap();
            vd3.set_is_captured(true);
            (*s).get_closure_var2(
                ctx,
                fd,
                true,
                false,
                var_idx as u16,
                var_name,
                vd3.is_const(),
                vd3.is_lexical(),
                vd3.var_kind(),
            )
        }
        .map_or(-1, |x| x as i32);

        if idx >= 0 {
            has_idx = true;
        }
    }

    if has_idx {
        // found closure var idx
        let cv = (*s)
            .closure_vars
            .as_ref()
            .and_then(|a| a.get(idx as usize))
            .unwrap();

        if cv.is_const()
            && matches!(
                op as u8,
                opcodes::op_scope_put_var | opcodes::op_scope_make_ref
            )
        {
            push_vec_u8(bc, opcodes::OP_throw_error);
            push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
            push_vec_u8(bc, JS_THROW_VAR_RO);
        } else {
            match op as u8 {
                opcodes::op_scope_make_ref => {
                    debug_assert!(label.is_some());

                    if cv.var_kind() == JSVarKindEnum::JS_VAR_FUNCTION_NAME {
                        // Create a dummy object reference for the func_var
                        push_vec_u8(bc, opcodes::OP_object);
                        push_vec_u8(bc, opcodes::OP_get_var_ref);
                        push_vec_u16(bc, idx as _);
                        push_vec_u8(bc, opcodes::OP_define_field);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                        push_vec_u8(bc, opcodes::OP_push_atom_value);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                    } else if label_done.is_none()
                        && can_opt_put_ref_value(bc_buf, label.unwrap().pos)
                    {
                        pos_next = optimize_scope_make_ref(
                            ctx,
                            s,
                            bc,
                            bc_buf,
                            label.unwrap(),
                            pos_next,
                            if cv.is_lexical() {
                                OpCode::GET_VAR_REF_CHECK
                            } else {
                                OpCode::GET_VAR_REF
                            },
                            idx,
                        );
                    } else {
                        // Create a dummy object with a named slot that is
                        // a reference to the closure variable
                        push_vec_u8(bc, opcodes::OP_make_var_ref_ref);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                        push_vec_u16(bc, idx as _);
                    }
                }
                opcodes::op_scope_get_ref
                | opcodes::op_scope_get_var_undef
                | opcodes::op_scope_get_var
                | opcodes::op_scope_put_var
                | opcodes::op_scope_put_var_init => {
                    if op == opcodes::op_scope_get_ref {
                        // XXX: should create a dummy object with a named slot that is
                        // a reference to the closure variable
                        push_vec_u8(bc, opcodes::OP_undefined);
                    }

                    let is_put = if matches!(
                        op as u8,
                        opcodes::op_scope_put_var | opcodes::op_scope_put_var_init
                    ) {
                        true
                    } else {
                        false
                    };
                    if is_put {
                        if cv.is_lexical() {
                            if op == opcodes::op_scope_put_var_init {
                                // 'this' can only be initialized once
                                if var_name == JS_ATOM_this {
                                    push_vec_u8(bc, opcodes::OP_put_var_ref_check_init);
                                } else {
                                    push_vec_u8(bc, opcodes::OP_put_var_ref);
                                }
                            } else {
                                push_vec_u8(bc, opcodes::OP_put_var_ref_check);
                            }
                        } else {
                            push_vec_u8(bc, opcodes::OP_put_var_ref);
                        }
                    } else {
                        if cv.is_lexical() {
                            push_vec_u8(bc, opcodes::OP_get_var_ref_check);
                        } else {
                            push_vec_u8(bc, opcodes::OP_get_var_ref);
                        }
                    }
                    push_vec_u16(bc, idx as _);
                }
                opcodes::op_scope_delete_var => {
                    push_vec_u8(bc, opcodes::OP_push_false);
                }
                _ => {}
            } // match op
        }

        done!(label_done);
    } // code block 'has_idx'

    //
    // global variable access
    //
    match op as u8 {
        opcodes::op_scope_make_ref => {
            debug_assert!(label.is_some());

            if label_done.is_none() && can_opt_put_global_ref_value(bc_buf, label.unwrap().pos) {
                pos_next = optimize_scope_make_global_ref(
                    ctx,
                    s,
                    bc,
                    bc_buf,
                    label.unwrap(),
                    pos_next as _,
                    var_name,
                ) as _;
            } else {
                push_vec_u8(bc, opcodes::OP_make_var_ref);
                push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
            }
        }
        opcodes::op_scope_get_ref => {
            // XXX: should create a dummy object with a named slot that is
            // a reference to the global variable
            push_vec_u8(bc, opcodes::OP_undefined);
            push_vec_u8(bc, opcodes::OP_get_var);
            push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
        }
        opcodes::op_scope_get_var_undef | opcodes::op_scope_get_var | opcodes::op_scope_put_var => {
            push_vec_u8(
                bc,
                opcodes::OP_get_var_undef + (op as u8 - opcodes::op_scope_get_var_undef),
            );
            push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
        }
        opcodes::op_scope_put_var_init => {
            push_vec_u8(bc, opcodes::OP_put_var_init);
            push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
        }
        opcodes::op_scope_delete_var => {
            push_vec_u8(bc, opcodes::OP_delete_var);
            push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
        }
        _ => {}
    }

    done!(label_done);
}
