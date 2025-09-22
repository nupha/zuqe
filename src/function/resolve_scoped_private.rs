use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, js_atom_concat_str},
    bytecode::opcodes,
    context::JSContext,
    error::JS_THROW_VAR_RO,
    function::{JSFunctionDef, JSVarKindEnum, get_loc_or_ref},
};

impl JSFunctionDef {
    /// `var_name` is not consumed
    fn resolve_scope_private_field1(
        &mut self,
        ctx: &JSContext,
        var_name: JSAtom,
        mut scope_level: i32,
    ) -> Result<(u16, JSVarKindEnum, bool), i32> {
        let mut root_fd: Option<&JSFunctionDef> = None;
        let mut is_ref = false;

        for cur_fd in self.parent_iter(true) {
            if let Some(mut idx) = cur_fd.find_private_class_field_all(var_name, scope_level) {
                let var_kind = cur_fd.vars[idx as usize].var_kind();
                if is_ref {
                    idx = self.get_closure_var2(
                        ctx,
                        cur_fd,
                        true,
                        false,
                        idx as u16,
                        var_name,
                        true,
                        true,
                        JSVarKindEnum::JS_VAR_NORMAL,
                    )?;
                }
                return Ok((idx, var_kind, is_ref));
            }

            scope_level = cur_fd.parent_scope_level;
            root_fd = Some(cur_fd);
            is_ref = true;
        }

        // reach here because var_name was not found till root fd.

        let root_fd = root_fd.unwrap();
        if root_fd.is_eval {
            // find in closure of the eval function (the root fd)
            if let Some((i, cv)) = root_fd.closure_vars.as_ref().and_then(|cvars| {
                cvars.iter().enumerate().find_map(|(i, cv)| {
                    if cv.var_name == var_name {
                        Some((i, cv))
                    } else {
                        None
                    }
                })
            }) {
                let var_kind = cv.var_kind();
                let idx = if !std::ptr::eq(root_fd, self) {
                    self.get_closure_var2(
                        ctx,
                        root_fd,
                        false,
                        cv.is_arg(),
                        i as _,
                        cv.var_name,
                        cv.is_const(),
                        cv.is_lexical(),
                        cv.var_kind(),
                    )?
                } else {
                    i as u16
                };
                return Ok((idx, var_kind, true));
            }
        }

        // XXX: no line number info
        ctx.throw_syntax_error(&format!(
            "undefined private field '{}'",
            var_name.to_utf8(ctx.rt())
        ));
        return Err(-1);
    }

    /// search `var_name` in all scopes since `scope_level`.
    /// `var_name` is not consumed.
    fn find_private_class_field_all(&self, var_name: JSAtom, scope_level: i32) -> Option<u16> {
        debug_assert!(scope_level >= 0);

        let mut idx = self.scopes[scope_level as usize].first;
        while idx >= 0 {
            if self.vars[idx as usize].var_name == var_name {
                return Some(idx as _);
            }
            idx = self.vars.get(idx as usize).unwrap().scope_next;
        }

        None
    }

    /// `var_name` is not consumed.
    pub(super) fn resolve_scope_private_field(
        &mut self,
        ctx: &JSContext,
        var_name: JSAtom,
        scope_level: i32,
        op: u8,
        bc: &mut Vec<u8>,
    ) -> Result<(), i32> {
        let (mut idx, mut var_kind, mut is_ref) =
            self.resolve_scope_private_field1(ctx, var_name, scope_level)?;
        debug_assert_ne!(var_kind, JSVarKindEnum::JS_VAR_NORMAL);

        match op as u8 {
            opcodes::op_scope_get_private_field | opcodes::op_scope_get_private_field2 => {
                match var_kind {
                    JSVarKindEnum::JS_VAR_PRIVATE_FIELD => {
                        if op == opcodes::op_scope_get_private_field2 {
                            bc.push(opcodes::OP_dup);
                        }
                        get_loc_or_ref(bc, is_ref, idx);
                        bc.push(opcodes::OP_get_private_field);
                    }
                    JSVarKindEnum::JS_VAR_PRIVATE_METHOD => {
                        get_loc_or_ref(bc, is_ref, idx);
                        bc.push(opcodes::OP_check_brand);
                        if op != opcodes::op_scope_get_private_field2 {
                            bc.push(opcodes::OP_nip);
                        }
                    }
                    JSVarKindEnum::JS_VAR_PRIVATE_GETTER
                    | JSVarKindEnum::JS_VAR_PRIVATE_GETTER_SETTER => {
                        if op == opcodes::op_scope_get_private_field2 {
                            bc.push(opcodes::OP_dup);
                        }
                        get_loc_or_ref(bc, is_ref, idx);

                        bc.push(opcodes::OP_check_brand);
                        bc.push(opcodes::OP_call_method);
                        bc.extend_from_slice(&[0, 0]); // 0_u16
                    }
                    JSVarKindEnum::JS_VAR_PRIVATE_SETTER => {
                        // XXX: add clearer error message
                        bc.push(opcodes::OP_throw_error);
                        bc.extend_from_slice(var_name.dup(ctx.rt()).0.to_ne_bytes().as_slice());
                        bc.push(JS_THROW_VAR_RO);
                    }
                    _ => {
                        unreachable!();
                    }
                }
            }
            opcodes::op_scope_put_private_field => {
                match var_kind {
                    JSVarKindEnum::JS_VAR_PRIVATE_FIELD => {
                        get_loc_or_ref(bc, is_ref, idx);
                        bc.push(opcodes::OP_put_private_field);
                    }
                    JSVarKindEnum::JS_VAR_PRIVATE_METHOD | JSVarKindEnum::JS_VAR_PRIVATE_GETTER => {
                        // XXX: add clearer error message
                        bc.push(opcodes::OP_throw_error);
                        bc.extend_from_slice(var_name.dup(ctx.rt()).0.to_ne_bytes().as_slice());
                        bc.push(JS_THROW_VAR_RO);
                    }
                    JSVarKindEnum::JS_VAR_PRIVATE_SETTER
                    | JSVarKindEnum::JS_VAR_PRIVATE_GETTER_SETTER => {
                        let setter_name = get_private_setter_name(ctx, var_name);
                        if setter_name == JS_ATOM_NULL {
                            return Err(-1);
                        }

                        let r = self.resolve_scope_private_field1(ctx, setter_name, scope_level);
                        JS_FreeAtom(ctx, setter_name);
                        (idx, var_kind, is_ref) = r?;
                        debug_assert_eq!(var_kind, JSVarKindEnum::JS_VAR_PRIVATE_SETTER);

                        get_loc_or_ref(bc, is_ref, idx);

                        bc.push(opcodes::OP_swap);
                        // obj func value
                        bc.push(opcodes::OP_rot3r);
                        // value obj func
                        bc.push(opcodes::OP_check_brand);
                        bc.push(opcodes::OP_rot3l);
                        // obj func value
                        bc.push(opcodes::OP_call_method);
                        bc.extend_from_slice(1_u16.to_ne_bytes().as_slice());
                        bc.push(opcodes::OP_drop);
                    }
                    _ => {
                        unreachable!(); // C2RS: Replaced abort() with unreachable!()
                    }
                }
            }
            opcodes::op_scope_in_private_field => {
                get_loc_or_ref(bc, is_ref, idx);
                bc.push(opcodes::OP_private_in);
            }

            _ => {
                unreachable!();
            }
        }

        Ok(())
    }
}

/// build a private setter function name from the private getter name
fn get_private_setter_name(ctx: &JSContext, name: JSAtom) -> JSAtom {
    unsafe { js_atom_concat_str(ctx, name, c"<set>".as_ptr().cast()) }
}
