// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    eval::EvalType,
    function::{FunctionKind, GLOBAL_VAR_OFFSET, JSVarDefEnum, JSVarKindEnum},
    module::ExportType,
    stack_frame::ExecMode,
};

use super::{state::JSParseState, tokenize::TokenKind};

impl<'s> JSParseState<'s> {
    /// define variable for current function.
    /// `name` is not consumed.
    pub fn define_var_cur_func(
        &mut self,
        name: Atom,
        var_def_type: JSVarDefEnum,
    ) -> Result<u32, i32> {
        let fd = self.cur_func_mut();
        let mut var_id: u32;

        match var_def_type {
            JSVarDefEnum::JS_VAR_DEF_WITH => {
                var_id = fd.add_scope_var(&self.ctx, name, JSVarKindEnum::JS_VAR_NORMAL)? as u32;
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
                            if !(!fd.attrs.exec_mode.contains(ExecMode::STRICT)
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
                    } else if fd.scope_level == fd.body_scope {
                        self.throw_syntax_error("invalid redefinition of lexical identifier");
                        return Err(-1);
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

                if fd.is_global_var
                    && let Some(hf) = fd.find_global_var(name)
                    && fd.is_child_scope(hf.scope_level, fd.scope_level)
                {
                    self.throw_syntax_error("invalid redefinition of global identifier");
                    return Err(-1);
                }

                if fd.is_eval
                    && matches!(fd.eval_type, EvalType::Global | EvalType::Module)
                    && fd.scope_level == fd.body_scope
                {
                    let hf = fd.add_global_var(&self.ctx, name)?;
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
                    var_id = fd.add_scope_var(&self.ctx, name, var_kind)? as u32;
                    let vd = fd.vars.get_mut(var_id as usize).unwrap();
                    vd.set_is_lexical(true);
                    vd.set_is_const(var_def_type == JSVarDefEnum::JS_VAR_DEF_CONST);
                }
            }
            JSVarDefEnum::JS_VAR_DEF_CATCH => {
                var_id = fd.add_scope_var(&self.ctx, name, JSVarKindEnum::JS_VAR_CATCH)? as u32;
            }
            JSVarDefEnum::JS_VAR_DEF_VAR => {
                if fd.find_lexical_decl(name, fd.scope_first, false).is_some() {
                    self.throw_syntax_error("invalid redefinition of lexical identifier");
                    return Err(-1);
                }

                if fd.is_global_var {
                    if let Some(hf) = fd.find_global_var(name)
                        && hf.is_lexical()
                        && hf.scope_level == fd.scope_level
                        && fd.eval_type == EvalType::Module
                    {
                        self.throw_syntax_error("invalid redefinition of lexical identifier");
                        return Err(-1);
                    }

                    fd.add_global_var(&self.ctx, name)?;
                    var_id = GLOBAL_VAR_OFFSET as u32;
                } else {
                    var_id = if let Some(id) = fd.find_var(name) {
                        // the variable already exists, don't add it again
                        id
                    } else {
                        let id = fd.add_var(&self.ctx, name)?;
                        if name == Atom::CONST_arguments && fd.has_arguments_binding {
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

    /// in case current function def contains module, `export` suggests if this var is exported.
    pub(super) fn define_var(
        &mut self,
        name: Atom,
        tok: TokenKind,
        export: bool,
    ) -> Result<(), i32> {
        {
            let func = self.cur_func();
            match name {
                Atom::CONST_yield if func.attrs.func_kind == FunctionKind::Generator => {
                    self.throw_syntax_error("yield is a reserved identifier");
                    return Err(-1);
                }
                Atom::CONST_arguments | Atom::CONST_eval
                    if func.attrs.exec_mode.contains(ExecMode::STRICT) =>
                {
                    self.throw_syntax_error("invalid variable name in strict mode");
                    return Err(-1);
                }
                Atom::CONST_let | Atom::CONST_undefined
                    if matches!(tok, TokenKind::LET | TokenKind::CONST) =>
                {
                    self.throw_syntax_error("invalid lexical variable name");
                    return Err(-1);
                }
                _ => {}
            }
        }

        self.define_var_cur_func(
            name,
            match tok {
                TokenKind::LET => JSVarDefEnum::JS_VAR_DEF_LET,
                TokenKind::CONST => JSVarDefEnum::JS_VAR_DEF_CONST,
                TokenKind::VAR => JSVarDefEnum::JS_VAR_DEF_VAR,
                TokenKind::CATCH => JSVarDefEnum::JS_VAR_DEF_CATCH,
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
