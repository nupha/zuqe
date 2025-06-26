use crate::{
    atom::{JSAtom, consts::JS_ATOM_arguments},
    context::JS_MODE_STRICT,
    eval::{JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MODULE},
    function::{GLOBAL_VAR_OFFSET, JSFunctionDef, JSVarDefEnum, JSVarKindEnum},
};

use super::state::JSParseState;

pub unsafe fn define_var(
    s: *mut JSParseState,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    var_def_type: JSVarDefEnum,
) -> i32 {
    macro_rules! goto_redef_lex_error {
        () => {
            // redefining a scoped var in the same scope: error
            (*s).throw_syntax_error("invalid redefinition of lexical identifier");
            return -1;
        };
    }
    macro_rules! goto_invalid_lexical_redefinition {
        () => {
            // error to redefine a var that inside a lexical scope
            (*s).throw_syntax_error("invalid redefinition of lexical identifier");
            return -1;
        };
    }

    let mut ctx = &(*s).ctx;
    let mut vid: i32;

    match var_def_type {
        JSVarDefEnum::JS_VAR_DEF_WITH => {
            vid = (*fd)
                .add_scope_var(ctx, name, JSVarKindEnum::JS_VAR_NORMAL)
                .map_or(-1, |x| x as i32);
        }
        JSVarDefEnum::JS_VAR_DEF_LET
        | JSVarDefEnum::JS_VAR_DEF_CONST
        | JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
        | JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL => {
            if let Some(id) = (*fd).find_lexical_decl(name, (*fd).scope_first, true) {
                vid = id as i32;

                if vid < GLOBAL_VAR_OFFSET {
                    let vd = (*fd).vars.get(vid as usize).unwrap();
                    if vd.scope_level == (*fd).scope_level {
                        // same scope: in non strict mode, functions can be redefined (annex B.3.3.4).
                        if !((*fd).js_mode & JS_MODE_STRICT as u8 == 0
                            && var_def_type == JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
                            && vd.var_kind() == JSVarKindEnum::JS_VAR_FUNCTION_DECL)
                        {
                            goto_redef_lex_error!();
                        }
                    } else if vd.var_kind() == JSVarKindEnum::JS_VAR_CATCH
                        && vd.scope_level + 2 == (*fd).scope_level
                    {
                        goto_redef_lex_error!();
                    }
                } else {
                    if (*fd).scope_level == (*fd).body_scope {
                        goto_redef_lex_error!();
                    }
                }
            }

            if var_def_type != JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
                && var_def_type != JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL
                && (*fd).scope_level == (*fd).body_scope
                && (*fd).find_arg(name).is_some()
            {
                // lexical variable redefines a parameter name
                (*s).throw_syntax_error("invalid redefinition of parameter name");
                return -1;
            }

            if (*fd)
                .find_var_in_child_scope(name, (*fd).scope_level)
                .is_some()
            {
                (*s).throw_syntax_error("invalid redefinition of a variable");
                return -1;
            }

            if (*fd).is_global_var {
                if let Some(hf) = (*fd).find_global_var(name) {
                    if (*fd).is_child_scope(hf.scope_level, (*fd).scope_level) {
                        (*s).throw_syntax_error("invalid redefinition of global identifier");
                        return -1;
                    }
                }
            }

            if (*fd).is_eval
                && matches!((*fd).eval_type, JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE)
                && (*fd).scope_level == (*fd).body_scope
            {
                if let Ok(hf) = (*fd).add_global_var(ctx, name) {
                    hf.set_is_lexical(true);
                    hf.set_is_const(var_def_type == JSVarDefEnum::JS_VAR_DEF_CONST);
                    vid = GLOBAL_VAR_OFFSET;
                } else {
                    return -1;
                }
            } else {
                let var_kind = if var_def_type == JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL {
                    JSVarKindEnum::JS_VAR_FUNCTION_DECL
                } else if var_def_type == JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL {
                    JSVarKindEnum::JS_VAR_NEW_FUNCTION_DECL
                } else {
                    JSVarKindEnum::JS_VAR_NORMAL
                };
                vid = (*fd)
                    .add_scope_var(ctx, name, var_kind)
                    .map_or(-1, |x| x as i32);
                if vid >= 0 {
                    let vd = (*fd).vars.get_mut(vid as usize).unwrap();
                    vd.set_is_lexical(true);
                    vd.set_is_const(var_def_type == JSVarDefEnum::JS_VAR_DEF_CONST);
                }
            }
        }
        JSVarDefEnum::JS_VAR_DEF_CATCH => {
            vid = (*fd)
                .add_scope_var(ctx, name, JSVarKindEnum::JS_VAR_CATCH)
                .map_or(-1, |x| x as i32);
        }
        JSVarDefEnum::JS_VAR_DEF_VAR => {
            if (*fd)
                .find_lexical_decl(name, (*fd).scope_first, false)
                .is_some()
            {
                goto_invalid_lexical_redefinition!();
            }

            if (*fd).is_global_var {
                if let Some(hf) = (*fd).find_global_var(name) {
                    if hf.is_lexical()
                        && hf.scope_level == (*fd).scope_level
                        && (*fd).eval_type == JS_EVAL_TYPE_MODULE
                    {
                        goto_invalid_lexical_redefinition!();
                    }
                }

                if (*fd).add_global_var(ctx, name).is_ok() {
                    vid = GLOBAL_VAR_OFFSET;
                } else {
                    return -1;
                }
            } else {
                #[cfg(debug_assertions)]
                let _name_refcnt = name.ref_count(ctx.rt());

                vid = if let Some(id) = (*fd).find_var(name) {
                    // the variable already exists, don't add it again
                    id as i32
                } else if let Ok(id) = (*fd).add_var(ctx, name) {
                    if name == JS_ATOM_arguments && (*fd).has_arguments_binding {
                        (*fd).arguments_var_idx.replace(id);
                    }
                    (*fd).vars.get_mut(id as usize).unwrap().scope_next = (*fd).scope_level;
                    id as i32
                } else {
                    // TODO: how to handle add_var() error?
                    -1
                };
            }
        }
    }

    vid
}
