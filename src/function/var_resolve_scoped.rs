use crate::{
    atom::{consts::*, JSAtom, JS_DupAtom},
    bytecode::{opcodes, OpCode},
    context::JSContext,
    error::JS_THROW_VAR_RO,
    function::{
        resolve_pseudo_var, var_object_test, JSFunctionDef, JSVarKindEnum, LabelInfo,
        ARGUMENT_VAR_OFFSET, ARG_SCOPE_END,
    },
    utils::{push_vec_u16, push_vec_u32, push_vec_u8},
};

use super::optimize::{
    can_opt_put_global_ref_value, can_opt_put_ref_value, optimize_scope_make_global_ref,
    optimize_scope_make_ref,
};

impl JSFunctionDef {
    /// resolve variable since `scope` upward to root scope.
    /// returns the position of the next opcode
    pub unsafe fn resolve_scope_var(
        &mut self,
        ctx: &mut JSContext,
        var_name: JSAtom,
        mut scope_level: i32,
        op: u8,
        bc: &mut Vec<u8>,
        bc_buf: *mut u8, // JSFunctionDef::byte_code buffer ptr
        label: Option<&LabelInfo>,
        mut pos_next: usize,
    ) -> usize {
        macro_rules! done {
            ($label:expr) => {
                if let Some(l) = $label {
                    push_vec_u8(bc, opcodes::op_label);
                    push_vec_u32(bc, l);
                    self.labels[l as usize].pos2 = bc.len() as i32;
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
        let mut var_id: i32 = -1;
        let mut idx = self.scopes[scope_level as usize].first;
        while idx >= 0 {
            let (cur_var_name, is_const, scope_next) = {
                let vd = self.vars.get(idx as usize).unwrap();
                (vd.var_name, vd.is_const(), vd.scope_next)
            };

            if cur_var_name == var_name {
                if matches!(op, opcodes::op_scope_put_var | opcodes::op_scope_make_ref) {
                    if is_const {
                        push_vec_u8(bc, opcodes::OP_throw_error);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                        push_vec_u8(bc, JS_THROW_VAR_RO);
                        done!(label_done);
                    }
                }
                var_id = idx;
                break;
            }

            if cur_var_name == JS_ATOM__with_ && !is_pseudo_var {
                push_vec_u8(bc, opcodes::OP_get_loc);
                push_vec_u16(bc, idx as _);
                var_object_test(ctx, self, var_name, op, bc, &mut label_done, true);
            }

            idx = scope_next;
        }

        let mut is_arg_scope = idx == ARG_SCOPE_END;
        if var_id < 0 {
            // argument scope: variables are not visible but pseudo
            // variables are visible
            if !is_arg_scope {
                var_id = self.find_var(var_name).map_or(-1, |x| x as i32);
            }
            if var_id < 0 && is_pseudo_var {
                var_id = resolve_pseudo_var(ctx, self, var_name).map_or(-1, |x| x as i32);
            }
            if var_id < 0 && var_name == JS_ATOM_arguments && self.has_arguments_binding {
                // 'arguments' pseudo variable
                var_id = self.add_arguments(ctx, false).map_or(-1, |x| x as i32);
            }
            if var_id < 0 && self.is_func_expr && var_name == self.func_name {
                // add a new variable with the function name
                var_id = self.add_func_var(ctx, var_name).map_or(-1, |x| x as i32)
            }
        }

        if var_id >= 0 {
            if var_id & ARGUMENT_VAR_OFFSET == 0
                && matches!(
                    op as u8,
                    opcodes::op_scope_put_var | opcodes::op_scope_make_ref
                )
                && self.vars.get(var_id as usize).unwrap().is_const()
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

                    if var_id & ARGUMENT_VAR_OFFSET == 0
                        && self.vars.get(var_id as usize).unwrap().var_kind()
                            == JSVarKindEnum::JS_VAR_FUNCTION_NAME
                    {
                        // Create a dummy object reference for the func_var
                        push_vec_u8(bc, opcodes::OP_object);
                        push_vec_u8(bc, opcodes::OP_get_loc);
                        push_vec_u16(bc, var_id as u16);
                        push_vec_u8(bc, opcodes::OP_define_field);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                        push_vec_u8(bc, opcodes::OP_push_atom_value);
                        push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                    } else if label_done.is_none()
                        && can_opt_put_ref_value(bc_buf, label.unwrap().pos)
                    {
                        let get_op = if var_id & ARGUMENT_VAR_OFFSET != 0 {
                            var_id -= ARGUMENT_VAR_OFFSET;
                            OpCode::GET_ARG
                        } else {
                            if self.vars.get(var_id as usize).unwrap().is_lexical() {
                                OpCode::GET_LOC_CHECK
                            } else {
                                OpCode::GET_LOC
                            }
                        };
                        pos_next = optimize_scope_make_ref(
                            ctx,
                            self,
                            bc,
                            bc_buf,
                            label.unwrap(),
                            pos_next,
                            get_op,
                            var_id,
                        );
                    } else {
                        // Create a dummy object with a named slot that is
                        // a reference to the local variable
                        if var_id & ARGUMENT_VAR_OFFSET != 0 {
                            push_vec_u8(bc, opcodes::OP_make_arg_ref);
                            push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                            push_vec_u16(bc, (var_id - ARGUMENT_VAR_OFFSET) as _);
                        } else {
                            push_vec_u8(bc, opcodes::OP_make_loc_ref);
                            push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
                            push_vec_u16(bc, var_id as u16);
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

                    if var_id & ARGUMENT_VAR_OFFSET != 0 {
                        push_vec_u8(
                            bc,
                            if is_put {
                                OpCode::PUT_ARG
                            } else {
                                OpCode::GET_ARG
                            } as u8,
                        );
                        push_vec_u16(bc, (var_id - ARGUMENT_VAR_OFFSET) as u16);
                    } else {
                        let vd = self.vars.get(var_id as usize).unwrap();
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
                        push_vec_u16(bc, var_id as _);
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
        if !is_arg_scope && self.var_object_idx.is_some() && !is_pseudo_var {
            push_vec_u8(bc, opcodes::OP_get_loc);
            push_vec_u16(bc, self.var_object_idx.unwrap());
            var_object_test(ctx, self, var_name, op, bc, &mut label_done, false);
        }
        // check eval object in argument scope
        if self.arg_var_object_idx.is_some() && !is_pseudo_var {
            push_vec_u8(bc, opcodes::OP_get_loc);
            push_vec_u16(bc, self.arg_var_object_idx.unwrap());
            var_object_test(ctx, self, var_name, op, bc, &mut label_done, false);
        }

        // check parent scopes
        let mut fd: *mut JSFunctionDef = self;
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
                    var_id = idx;
                    break;
                }

                if vd.var_name == JS_ATOM__with_ && !is_pseudo_var {
                    vd.set_is_captured(true);
                    match self.get_closure_var2(
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
                            var_object_test(ctx, self, var_name, op, bc, &mut label_done, true);
                        }
                        Err(_) => {
                            idx = -1;
                        }
                    }
                }
                idx = vd.scope_next;
            }

            is_arg_scope = idx == ARG_SCOPE_END;
            if var_id >= 0 {
                break;
            }

            if !is_arg_scope {
                var_id = (*fd).find_var(var_name).map_or(-1, |x| x as i32);
                if var_id >= 0 {
                    break;
                }
            }
            if is_pseudo_var {
                if resolve_pseudo_var(ctx, fd, var_name).is_some() {
                    break;
                } else {
                    var_id = -1;
                }
            }
            if var_name == JS_ATOM_arguments && (*fd).has_arguments_binding {
                var_id = (*fd).add_arguments(ctx, false).map_or(-1, |x| x as i32);
                break;
            }
            if (*fd).is_func_expr && (*fd).func_name == var_name {
                // add a new variable with the function name
                var_id = (*fd).add_func_var(ctx, var_name).map_or(-1, |x| x as i32);
                break;
            }

            // check eval object
            if !is_arg_scope && (*fd).var_object_idx.is_some() && !is_pseudo_var {
                let vd1 = (*fd)
                    .vars
                    .get_mut((*fd).var_object_idx.unwrap() as usize)
                    .unwrap();
                vd1.set_is_captured(true);
                idx = self
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
                var_object_test(ctx, self, var_name, op, bc, &mut label_done, false);
            }

            // check eval object in argument scope
            if !is_pseudo_var && (*fd).arg_var_object_idx.is_some() {
                let vd2 = (*fd)
                    .vars
                    .get_mut((*fd).arg_var_object_idx.unwrap() as usize)
                    .unwrap();
                vd2.set_is_captured(true);
                idx = self
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
                var_object_test(ctx, self, var_name, op, bc, &mut label_done, false);
            }

            if (*fd).is_eval {
                break; // it it necessarily the top level function
            }
        }

        // check direct eval scope (in the closure of the eval function
        // which is necessarily at the top level)
        if fd.is_null() {
            fd = self;
        }

        // find closure variable
        let mut is_closure_idx = false;
        if var_id < 0 && (*fd).is_eval {
            if let Some(cvars) = (*fd).closure_vars.as_ref() {
                for (i, cv) in cvars.iter().enumerate() {
                    if var_name == cv.var_name {
                        idx = if fd != self {
                            self.get_closure_var2(
                                ctx,
                                fd,
                                false,
                                cv.is_arg(),
                                i as _,
                                cv.var_name,
                                cv.is_const(),
                                cv.is_lexical(),
                                cv.var_kind(),
                            )
                            .unwrap() as i32 // can't fail
                        } else {
                            i as i32
                        };

                        is_closure_idx = true;
                        break;
                    } else if !is_pseudo_var
                        && matches!(
                            (*cv).var_name,
                            JS_ATOM__var_ | JS_ATOM__arg_var_ | JS_ATOM__with_
                        )
                    {
                        let is_with = cv.var_name == JS_ATOM__with_;
                        idx = if fd != self {
                            self.get_closure_var2(
                                ctx,
                                fd,
                                false,
                                cv.is_arg(),
                                i as _,
                                cv.var_name,
                                false,
                                false,
                                JSVarKindEnum::JS_VAR_NORMAL,
                            )
                            .expect("can't fail") as i32
                        } else {
                            i as i32
                        };

                        push_vec_u8(bc, opcodes::OP_get_var_ref);
                        push_vec_u16(bc, idx as _);
                        var_object_test(ctx, self, var_name, op, bc, &mut label_done, is_with);
                    }
                }
            }
        }

        if var_id >= 0 {
            // find the corresponding closure variable
            idx = if var_id & ARGUMENT_VAR_OFFSET != 0 {
                (*fd)
                    .args
                    .get_mut((var_id - ARGUMENT_VAR_OFFSET) as usize)
                    .unwrap()
                    .set_is_captured(true);

                self.get_closure_var2(
                    ctx,
                    fd,
                    true,
                    true,
                    (var_id - ARGUMENT_VAR_OFFSET) as u16,
                    var_name,
                    false,
                    false,
                    JSVarKindEnum::JS_VAR_NORMAL,
                )
            } else {
                let vd3 = (*fd).vars.get_mut(var_id as usize).unwrap();
                vd3.set_is_captured(true);
                self.get_closure_var2(
                    ctx,
                    fd,
                    true,
                    false,
                    var_id as u16,
                    var_name,
                    vd3.is_const(),
                    vd3.is_lexical(),
                    vd3.var_kind(),
                )
            }
            .map_or(-1, |x| x as i32);

            if idx >= 0 {
                is_closure_idx = true;
            }
        }

        if is_closure_idx {
            // found closure var idx
            debug_assert!(idx >= 0);
            return self.resolve_scope_var_closure(
                ctx, var_name, op, bc, bc_buf, label, label_done, idx as _, pos_next,
            );
        }

        // global var access
        self.resolve_scope_var_global(ctx, var_name, op, bc, bc_buf, label, label_done, pos_next)
    }

    /// closure var
    unsafe fn resolve_scope_var_closure(
        &mut self,
        ctx: &mut JSContext,
        var_name: JSAtom,
        op: u8,
        bc: &mut Vec<u8>,
        bc_buf: *mut u8,
        label: Option<&LabelInfo>,
        label_done: Option<u32>,
        idx: u32,
        mut pos_next: usize,
    ) -> usize {
        let cv = self
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
                            self,
                            bc,
                            bc_buf,
                            label.unwrap(),
                            pos_next,
                            if cv.is_lexical() {
                                OpCode::GET_VAR_REF_CHECK
                            } else {
                                OpCode::GET_VAR_REF
                            },
                            idx as _,
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

        if let Some(l) = label_done {
            push_vec_u8(bc, opcodes::op_label);
            push_vec_u32(bc, l);
            self.labels[l as usize].pos2 = bc.len() as i32;
        }
        pos_next
    }

    /// global var
    unsafe fn resolve_scope_var_global(
        &mut self,
        ctx: &mut JSContext,
        var_name: JSAtom,
        op: u8,
        bc: &mut Vec<u8>,
        bc_buf: *mut u8, // JSFunctionDef::byte_code buffer ptr
        label: Option<&LabelInfo>,
        label_done: Option<u32>,
        mut pos_next: usize,
    ) -> usize {
        match op as u8 {
            opcodes::op_scope_make_ref => {
                debug_assert!(label.is_some());

                if label_done.is_none() && can_opt_put_global_ref_value(bc_buf, label.unwrap().pos)
                {
                    pos_next = optimize_scope_make_global_ref(
                        ctx,
                        self,
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
            opcodes::op_scope_get_var_undef
            | opcodes::op_scope_get_var
            | opcodes::op_scope_put_var => {
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

        if let Some(l) = label_done {
            push_vec_u8(bc, opcodes::op_label);
            push_vec_u32(bc, l);
            self.labels[l as usize].pos2 = bc.len() as i32;
        }
        pos_next
    }
}
