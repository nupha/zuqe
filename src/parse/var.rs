use crate::{
    atom::{
        JSAtom,
        consts::{JS_ATOM_arguments, JS_ATOM_eval, JS_ATOM_let, JS_ATOM_undefined, JS_ATOM_yield},
    },
    context::JS_MODE_STRICT,
    eval::{JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MODULE},
    function::{GLOBAL_VAR_OFFSET, JSFunctionKindEnum, JSVarDefEnum, JSVarKindEnum},
    module::ExportType,
    parse::token::{TOK_CATCH, TOK_CONST, TOK_LET, TOK_VAR},
};

use super::state::JSParseState;

impl<'s> JSParseState<'s> {
    /// define variable for current function.
    /// `name` is not consumed.
    pub fn define_var_cur_func(
        &mut self,
        name: JSAtom,
        var_def_type: JSVarDefEnum,
    ) -> Result<u32, i32> {
        let ctx = self.ctx.clone();
        let fd = self.cur_func_mut();
        let mut var_id: u32;

        match var_def_type {
            JSVarDefEnum::JS_VAR_DEF_WITH => {
                var_id =
                    unsafe { fd.add_scope_var(&ctx, name, JSVarKindEnum::JS_VAR_NORMAL)? as u32 };
            }
            JSVarDefEnum::JS_VAR_DEF_LET
            | JSVarDefEnum::JS_VAR_DEF_CONST
            | JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
            | JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL => {
                if let Some(id) = fd.find_lexical_decl(name, fd.scope_first, true) {
                    var_id = id;

                    if var_id < GLOBAL_VAR_OFFSET as u32 {
                        let vd = fd.vars.get(var_id as usize).unwrap();
                        if vd.scope_level == fd.scope_level {
                            // same scope: in non strict mode, functions can be redefined (annex B.3.3.4).
                            if !(fd.attrs.js_mode & JS_MODE_STRICT as u8 == 0
                                && var_def_type == JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
                                && vd.var_kind() == JSVarKindEnum::JS_VAR_FUNCTION_DECL)
                            {
                                self.throw_syntax_error(
                                    "invalid redefinition of lexical identifier",
                                );
                                return Err(-1);
                            }
                        } else if vd.var_kind() == JSVarKindEnum::JS_VAR_CATCH
                            && vd.scope_level + 2 == fd.scope_level
                        {
                            self.throw_syntax_error("invalid redefinition of lexical identifier");
                            return Err(-1);
                        }
                    } else {
                        if fd.scope_level == fd.body_scope {
                            self.throw_syntax_error("invalid redefinition of lexical identifier");
                            return Err(-1);
                        }
                    }
                }

                if var_def_type != JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
                    && var_def_type != JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL
                    && fd.scope_level == fd.body_scope
                    && fd.find_arg(name).is_some()
                {
                    // lexical variable redefines a parameter name
                    self.throw_syntax_error("invalid redefinition of parameter name");
                    return Err(-1);
                }

                if fd.find_var_in_child_scope(name, fd.scope_level).is_some() {
                    self.throw_syntax_error("invalid redefinition of a variable");
                    return Err(-1);
                }

                if fd.is_global_var {
                    if let Some(hf) = fd.find_global_var(name) {
                        if fd.is_child_scope(hf.scope_level, (*fd).scope_level) {
                            self.throw_syntax_error("invalid redefinition of global identifier");
                            return Err(-1);
                        }
                    }
                }

                if fd.is_eval
                    && matches!(fd.eval_type, JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE)
                    && fd.scope_level == fd.body_scope
                {
                    let hf = fd.add_global_var(&ctx, name)?;
                    hf.set_is_lexical(true);
                    hf.set_is_const(var_def_type == JSVarDefEnum::JS_VAR_DEF_CONST);
                    var_id = GLOBAL_VAR_OFFSET as u32;
                } else {
                    let var_kind = if var_def_type == JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL {
                        JSVarKindEnum::JS_VAR_FUNCTION_DECL
                    } else if var_def_type == JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL {
                        JSVarKindEnum::JS_VAR_NEW_FUNCTION_DECL
                    } else {
                        JSVarKindEnum::JS_VAR_NORMAL
                    };
                    var_id = unsafe { fd.add_scope_var(&ctx, name, var_kind)? as u32 };
                    let vd = fd.vars.get_mut(var_id as usize).unwrap();
                    vd.set_is_lexical(true);
                    vd.set_is_const(var_def_type == JSVarDefEnum::JS_VAR_DEF_CONST);
                }
            }
            JSVarDefEnum::JS_VAR_DEF_CATCH => {
                var_id =
                    unsafe { fd.add_scope_var(&ctx, name, JSVarKindEnum::JS_VAR_CATCH)? as u32 };
            }
            JSVarDefEnum::JS_VAR_DEF_VAR => {
                if fd.find_lexical_decl(name, fd.scope_first, false).is_some() {
                    self.throw_syntax_error("invalid redefinition of lexical identifier");
                    return Err(-1);
                }

                if fd.is_global_var {
                    if let Some(hf) = fd.find_global_var(name) {
                        if hf.is_lexical()
                            && hf.scope_level == fd.scope_level
                            && fd.eval_type == JS_EVAL_TYPE_MODULE
                        {
                            self.throw_syntax_error("invalid redefinition of lexical identifier");
                            return Err(-1);
                        }
                    }

                    fd.add_global_var(&ctx, name)?;
                    var_id = GLOBAL_VAR_OFFSET as u32;
                } else {
                    #[cfg(debug_assertions)]
                    let _name_refcnt = name.ref_count(&ctx.rt());

                    var_id = if let Some(id) = fd.find_var(name) {
                        // the variable already exists, don't add it again
                        id
                    } else {
                        let id = fd.add_var(&ctx, name)?;
                        if name == JS_ATOM_arguments && fd.has_arguments_binding {
                            fd.arguments_var_idx.replace(id);
                        }
                        fd.vars[id as usize].scope_next = fd.scope_level;
                        id as u32
                    };
                }
            }
        }

        Ok(var_id)
    }

    /// `name` is not consumed.
    /// in case current function def contains module, `export` suggests if this var is exported.
    pub fn define_var(&mut self, name: JSAtom, tok: i32, export: bool) -> Result<(), i32> {
        {
            let func = self.cur_func();

            match name {
                JS_ATOM_yield if func.attrs.func_kind == JSFunctionKindEnum::JS_FUNC_GENERATOR => {
                    self.throw_syntax_error("yield is a reserved identifier");
                    return Err(-1);
                }
                JS_ATOM_arguments | JS_ATOM_eval
                    if func.attrs.js_mode & JS_MODE_STRICT as u8 != 0 =>
                {
                    self.throw_syntax_error("invalid variable name in strict mode");
                    return Err(-1);
                }
                JS_ATOM_let | JS_ATOM_undefined if matches!(tok, TOK_LET | TOK_CONST) => {
                    self.throw_syntax_error("invalid lexical variable name");
                    return Err(-1);
                }
                _ => {}
            }
        }

        self.define_var_cur_func(
            name,
            match tok {
                TOK_LET => JSVarDefEnum::JS_VAR_DEF_LET,
                TOK_CONST => JSVarDefEnum::JS_VAR_DEF_CONST,
                TOK_VAR => JSVarDefEnum::JS_VAR_DEF_VAR,
                TOK_CATCH => JSVarDefEnum::JS_VAR_DEF_CATCH,
                _ => unreachable!(),
            },
        )?;

        if export {
            debug_assert!(
                self.cur_func().module.is_some(),
                "function def must contain module"
            );

            self.add_module_export(
                None,
                name,
                name,
                ExportType::JS_EXPORT_TYPE_LOCAL {
                    var_idx: u16::MAX,
                    var_ref: None,
                },
            )?;
        }

        Ok(())
    }
}
