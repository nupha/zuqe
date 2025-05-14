use std::ops::DerefMut;

use crate::{
    atom::{consts::JS_ATOM_arguments, JSAtom},
    context::JS_MODE_STRICT,
    eval::{JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MODULE},
    function::{
        add_global_var, find_lexical_decl, is_child_scope, JSFunctionDef, JSVarDefEnum,
        JSVarKindEnum, GLOBAL_VAR_OFFSET,
    },
};

use super::{error::js_parse_error, state::JSParseState};

pub unsafe fn define_var(
    s: *mut JSParseState,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    var_def_type: JSVarDefEnum,
) -> i32 {
    macro_rules! goto_redef_lex_error {
        () => {
            // redefining a scoped var in the same scope: error
            return js_parse_error(s, c"invalid redefinition of lexical identifier");
        };
    }
    macro_rules! goto_invalid_lexical_redefinition {
        () => {
            // error to redefine a var that inside a lexical scope
            return js_parse_error(s, c"invalid redefinition of lexical identifier");
        };
    }

    let mut ctx = (*s).ctx;
    let mut idx: i32;

    match var_def_type {
        JSVarDefEnum::JS_VAR_DEF_WITH => {
            idx = (*fd)
                .add_scope_var(ctx.deref_mut(), name, JSVarKindEnum::JS_VAR_NORMAL)
                .map_or(-1, |x| x as i32);
        }
        JSVarDefEnum::JS_VAR_DEF_LET
        | JSVarDefEnum::JS_VAR_DEF_CONST
        | JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
        | JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL => {
            idx = find_lexical_decl(ctx.as_ptr(), fd, name, (*fd).scope_first, true);
            if idx >= 0 {
                if idx < GLOBAL_VAR_OFFSET {
                    let vd = (*fd).vars.get(idx as usize).unwrap();
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
                return js_parse_error(s, c"invalid redefinition of parameter name");
            }

            if (*fd)
                .find_var_in_child_scope(name, (*fd).scope_level)
                .is_some()
            {
                return js_parse_error(s, c"invalid redefinition of a variable");
            }

            if (*fd).is_global_var {
                if let Some(hf) = (*fd).find_global_var(name) {
                    if is_child_scope(fd, hf.scope_level, (*fd).scope_level) {
                        return js_parse_error(s, c"invalid redefinition of global identifier");
                    }
                }
            }

            if (*fd).is_eval
                && matches!((*fd).eval_type, JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE)
                && (*fd).scope_level == (*fd).body_scope
            {
                let hf = add_global_var(ctx.as_ptr(), fd, name);
                if hf.is_null() {
                    return -1;
                }
                (*hf).set_is_lexical(true);
                (*hf).set_is_const(var_def_type == JSVarDefEnum::JS_VAR_DEF_CONST);
                idx = GLOBAL_VAR_OFFSET;
            } else {
                let var_kind = if var_def_type == JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL {
                    JSVarKindEnum::JS_VAR_FUNCTION_DECL
                } else if var_def_type == JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL {
                    JSVarKindEnum::JS_VAR_NEW_FUNCTION_DECL
                } else {
                    JSVarKindEnum::JS_VAR_NORMAL
                };
                idx = (*fd)
                    .add_scope_var(ctx.deref_mut(), name, var_kind)
                    .map_or(-1, |x| x as i32);
                if idx >= 0 {
                    let vd = (*fd).vars.get_mut(idx as usize).unwrap();
                    vd.set_is_lexical(true);
                    vd.set_is_const(var_def_type == JSVarDefEnum::JS_VAR_DEF_CONST);
                }
            }
        }
        JSVarDefEnum::JS_VAR_DEF_CATCH => {
            idx = (*fd)
                .add_scope_var(ctx.deref_mut(), name, JSVarKindEnum::JS_VAR_CATCH)
                .map_or(-1, |x| x as i32);
        }
        JSVarDefEnum::JS_VAR_DEF_VAR => {
            if find_lexical_decl(ctx.as_ptr(), fd, name, (*fd).scope_first, false) >= 0 {
                goto_invalid_lexical_redefinition!();
            }
            if (*fd).is_global_var {
                if let Some(hf) = (*fd).find_global_var(name) {
                    if (*hf).is_lexical()
                        && (*hf).scope_level == (*fd).scope_level
                        && (*fd).eval_type == JS_EVAL_TYPE_MODULE
                    {
                        goto_invalid_lexical_redefinition!();
                    }
                }

                let hf = add_global_var(ctx.as_ptr(), fd, name);
                if hf.is_null() {
                    return -1;
                }
                idx = GLOBAL_VAR_OFFSET;
            } else {
                #[cfg(debug_assertions)]
                let _name_refcnt = name.ref_count(ctx.rt());

                idx = if let Some(id) = (*fd).find_var(name) {
                    // the variable already exists, don't add it again
                    id
                } else if let Ok(id) = (*fd).add_var(ctx.deref_mut(), name) {
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

    idx
}
