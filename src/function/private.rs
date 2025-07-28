use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, js_atom_concat_str},
    bytecode::opcodes,
    context::JSContext,
    error::{JS_THROW_VAR_RO, JS_ThrowSyntaxError},
    function::{JSFunctionDef, JSVarKindEnum, get_loc_or_ref},
};

/// var_name is not consumed.
// return 0 if OK or -1 if the private field could not be resolved
pub fn resolve_scope_private_field(
    ctx: &JSContext,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
    scope_level: i32,
    op: u8,
    bc: &mut Vec<u8>,
) -> i32 {
    let mut is_ref = false;
    let mut var_kind = JSVarKindEnum::JS_VAR_NORMAL;

    let idx =
        resolve_scope_private_field1(ctx, &mut is_ref, &mut var_kind, s, var_name, scope_level);
    if idx < 0 {
        return -1;
    }
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
                        return -1;
                    }
                    let idx = resolve_scope_private_field1(
                        ctx,
                        &mut is_ref,
                        &mut var_kind,
                        s,
                        setter_name,
                        scope_level,
                    );
                    JS_FreeAtom(ctx, setter_name);
                    if idx < 0 {
                        return -1;
                    }
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

    0
}

// search in all scopes
pub fn find_private_class_field_all(fd: *mut JSFunctionDef, name: JSAtom, scope_level: i32) -> i32 {
    debug_assert!(scope_level >= 0);

    let mut idx = unsafe { (*fd).scopes[scope_level as usize].first };
    while idx >= 0 {
        if unsafe { (*fd).vars.get(idx as usize).unwrap().var_name } == name {
            return idx;
        }
        idx = unsafe { (*fd).vars.get(idx as usize).unwrap().scope_next };
    }

    return -1;
}

/// var_name is not consumed
pub fn resolve_scope_private_field1(
    ctx: &JSContext,
    pis_ref: *mut bool,
    pvar_kind: *mut JSVarKindEnum,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
    mut scope_level: i32,
) -> i32 {
    let mut fd = s;
    let mut is_ref = false;
    let mut idx: i32;
    let var_kind: JSVarKindEnum;

    'outer: loop {
        idx = find_private_class_field_all(fd, var_name, scope_level);
        if idx >= 0 {
            var_kind = unsafe { (*fd).vars.get(idx as usize).unwrap().var_kind() };
            if is_ref {
                idx = if let Ok(vi) = unsafe {
                    (*s).get_closure_var2(
                        ctx,
                        fd,
                        true,
                        false,
                        idx as u16,
                        var_name,
                        true,
                        true,
                        JSVarKindEnum::JS_VAR_NORMAL,
                    )
                } {
                    vi as i32
                } else {
                    return -1;
                }
            }
            break 'outer;
        }

        scope_level = unsafe { (*fd).parent_scope_level };
        if unsafe { (*fd).parent.is_null() } {
            if unsafe { (*fd).is_eval } {
                // closure of the eval function (top level)
                if let Some(cvars) = unsafe { (*fd).closure_vars.as_ref() } {
                    for (i, cv) in cvars.iter().enumerate() {
                        if cv.var_name == var_name {
                            var_kind = cv.var_kind();
                            is_ref = true;

                            if fd != s {
                                idx = if let Ok(vi) = unsafe {
                                    (*s).get_closure_var2(
                                        ctx,
                                        fd,
                                        false,
                                        (*cv).is_arg(),
                                        i as _,
                                        cv.var_name,
                                        cv.is_const(),
                                        cv.is_lexical(),
                                        cv.var_kind(),
                                    )
                                } {
                                    vi as i32
                                } else {
                                    return -1;
                                }
                            }

                            break 'outer; // goto done
                        }
                    }
                }
            }

            // XXX: no line number info
            JS_ThrowSyntaxError(
                ctx,
                &format!("undefined private field '{}'", var_name.to_str(ctx.rt())),
            );
            return -1;
        } else {
            fd = unsafe { (*fd).parent };
        }

        is_ref = true;
    }

    // done:
    unsafe {
        *pis_ref = is_ref;
        *pvar_kind = var_kind;
    }

    idx
}

/* build a private setter function name from the private getter name */
pub fn get_private_setter_name(ctx: &JSContext, name: JSAtom) -> JSAtom {
    unsafe { js_atom_concat_str(ctx, name, c"<set>".as_ptr().cast()) }
}
