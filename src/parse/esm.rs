use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_FreeAtomRT, JSAtom, consts::*},
    bytecode::OpCodeTemp,
    defer,
    function::{JSFunctionKindEnum, JSVarDefEnum, JSVarKindEnum},
    module::{JSExportEntry, JSExportTypeEnum, JSImportEntry, ModuleDef, add_star_export_entry},
    value::JSValue,
};

use super::{
    function::JSParseFunctionEnum,
    skip_shebang,
    state::JSParseState,
    token::{
        CHAR_BRACKET_L, CHAR_COMMA, CHAR_CURLY_R, CHAR_DOT, TOK_CLASS, TOK_CONST, TOK_DEFAULT,
        TOK_EXPORT, TOK_FUNCTION, TOK_IDENT, TOK_IMPORT, TOK_LET, TOK_STRING, TOK_VAR,
        simple_next_token,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSParseExportEnum {
    JS_PARSE_EXPORT_NONE,
    JS_PARSE_EXPORT_NAMED,
    JS_PARSE_EXPORT_DEFAULT,
}

impl<'s> JSParseState<'s> {
    /// return atom of module name.
    /// returned atom is duplicated, caller should free it after use.
    fn parse_from_clause(&mut self) -> Result<JSAtom, i32> {
        if !self.token_is_pseudo_keyword(JS_ATOM_from) {
            self.throw_syntax_error("from clause expected");
            return Err(-1);
        }

        self.next_token()?;

        if self.token.val != TOK_STRING {
            self.throw_syntax_error("string expected");
            return Err(-1);
        }

        let module_name = self
            .ctx
            .create_string_with_content(self.token.data.string().unwrap().clone())
            .and_then(|s| s.to_atom(&self.ctx))?;

        if self.next_token().is_err() {
            JS_FreeAtom(&self.ctx, module_name);
            return Err(-1);
        }

        Ok(module_name)
    }

    /// parse import statement
    pub fn parse_import(&mut self) -> Result<(), i32> {
        let md = self.cur_func().module.clone().unwrap();

        self.next_token()?;

        let first_import = md.0.import_entries.borrow().len();
        let module_name: JSAtom;

        if self.token.val == TOK_STRING {
            module_name = self
                .ctx
                .create_string_with_content(self.token.data.string().unwrap().clone())
                .and_then(|s| s.to_atom(&self.ctx))?;

            if self.next_token().is_err() {
                JS_FreeAtom(&self.ctx, module_name);
                return Err(-1);
            }
        } else {
            'outer: loop {
                if self.token.val == TOK_IDENT {
                    if self.token.data.is_reserved() {
                        self.throw_reserved_identifier_error();
                        return Err(-1);
                    }

                    // "default" import
                    let local_name = self
                        .token
                        .atom()
                        .unwrap()
                        .dup(self.ctx.rt())
                        .into_rc(self.ctx.rt());

                    self.next_token()?;
                    self.add_module_import(&md, *local_name, JS_ATOM_default)?;

                    if self.token.val != ',' as i32 {
                        break 'outer;
                    } else {
                        self.next_token()?;
                    }
                }

                if self.token.val == '*' as i32 {
                    // name space import
                    self.next_token()?;

                    if !self.token_is_pseudo_keyword(JS_ATOM_as) {
                        self.throw_syntax_error("expecting 'as'");
                        return Err(-1);
                    }

                    self.next_token()?;

                    if !Self::token_maybe_ident(self.token.val) {
                        self.throw_syntax_error("identifier expected");
                        return Err(-1);
                    }

                    let local_name = self
                        .token
                        .atom()
                        .unwrap()
                        .dup(self.ctx.rt())
                        .into_rc(self.ctx.rt());

                    self.next_token()?;
                    self.add_module_import(&md, *local_name, JS_ATOM__star_)?;
                } else if self.token.val == '{' as i32 {
                    self.next_token()?;

                    while self.token.val != '}' as i32 {
                        if !Self::token_maybe_ident(self.token.val) {
                            self.throw_syntax_error("identifier expected");
                            return Err(-1);
                        }

                        let import_name = self
                            .token
                            .atom()
                            .unwrap()
                            .dup(self.ctx.rt())
                            .into_rc(self.ctx.rt());

                        self.next_token()?;

                        let local_name = if self.token_is_pseudo_keyword(JS_ATOM_as) {
                            self.next_token()?;

                            if !Self::token_maybe_ident(self.token.val) {
                                self.throw_syntax_error("identifier expected");
                                return Err(-1);
                            }

                            let ln = self.token.atom().unwrap().dup(self.ctx.rt());
                            self.next_token()?;
                            ln
                        } else {
                            import_name.dup(&self.ctx.rt())
                        }
                        .into_rc(&self.ctx.rt());

                        self.add_module_import(&md, *local_name, *import_name)?;

                        if self.token.val != b',' as i32 {
                            break;
                        }

                        self.next_token()?;
                    }

                    self.parse_expect(CHAR_CURLY_R)?;
                }

                break 'outer;
            } // 'outer loop

            module_name = self.parse_from_clause()?;
        }

        let idx = md.add_dependency(&self.ctx, module_name);
        JS_FreeAtom(&self.ctx, module_name);
        let idx = idx?;

        for imp in &mut md.0.import_entries.borrow_mut()[first_import..] {
            imp.req_module_idx = idx as _;
        }

        self.parse_expect_semi()
    }

    pub fn parse_export(&mut self) -> Result<(), i32> {
        let md = self.cur_func().module.as_ref().cloned().unwrap();

        self.next_token()?;
        let tok = self.token.val;

        if tok == TOK_CLASS {
            return self.parse_class(false, JSParseExportEnum::JS_PARSE_EXPORT_NAMED);
        } else if tok == TOK_FUNCTION
            || (self.token_is_pseudo_keyword(JS_ATOM_async)
                && self.peek_token(true) == TOK_FUNCTION)
        {
            return unsafe {
                self.parse_function_decl2(
                    JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                    JSFunctionKindEnum::JS_FUNC_NORMAL,
                    JS_ATOM_NULL,
                    self.token.ptr,
                    self.token.line_num,
                    JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
                    std::ptr::null_mut(),
                )
            };
        }

        self.next_token()?;

        let mut local_name = JS_ATOM_NULL;
        let mut export_name = JS_ATOM_NULL;
        let local_name = &mut local_name;
        let export_name = &mut export_name;

        // free atom when exit
        let local_name_ptr = local_name as *mut JSAtom;
        let export_name_ptr = export_name as *mut JSAtom;
        let rt_ptr = self.ctx.rt_ptr();
        defer!(unsafe {
            JS_FreeAtomRT(rt_ptr.as_ptr(), *local_name_ptr);
            JS_FreeAtomRT(rt_ptr.as_ptr(), *export_name_ptr);
        });

        match tok {
            // '{'
            0x7b => {
                // export {...}
                let first_export = md.0.export_entries.borrow().len();

                while self.token.val != b'}' as i32 {
                    *local_name = parse_export_name(self)?;

                    self.next_token()?;

                    if self.token_is_pseudo_keyword(JS_ATOM_as) {
                        // <local_name> as <export_name>
                        self.next_token()?;
                        *export_name = parse_export_name(self)?;
                        self.next_token()?;
                    } else {
                        *export_name = local_name.dup(self.ctx.rt());
                    }

                    self.add_module_export(
                        Some(&md),
                        *local_name,
                        *export_name,
                        JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                            var_idx: u16::MAX, // unknown for now
                            var_ref: None,
                        },
                    )?;

                    // reset name atoms
                    JS_FreeAtom(&self.ctx, *local_name);
                    JS_FreeAtom(&self.ctx, *export_name);
                    *local_name = JS_ATOM_NULL;
                    *export_name = JS_ATOM_NULL;

                    if self.token.val != CHAR_COMMA {
                        break;
                    }

                    self.next_token()?;
                }

                self.parse_expect('}' as i32)?;

                // Handle re-exports
                if self.token_is_pseudo_keyword(JS_ATOM_from) {
                    let module_name = self.parse_from_clause()?;
                    let idx = md.add_dependency(&self.ctx, module_name);
                    JS_FreeAtom(&self.ctx, module_name);

                    let idx = idx?;

                    for me in &mut md.0.export_entries.borrow_mut()[first_export..] {
                        me.export_type = JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(idx as _);
                    }
                }
            }

            // *
            0x2a => {
                if self.token_is_pseudo_keyword(JS_ATOM_as) {
                    // export ns from
                    self.next_token()?;
                    *export_name = parse_export_name(self)?;
                    self.next_token()?;

                    let module_name = self.parse_from_clause()?;
                    let idx = md.add_dependency(&self.ctx, module_name);
                    JS_FreeAtom(&self.ctx, module_name);

                    let idx = idx?;
                    self.add_module_export(
                        Some(&md),
                        JS_ATOM__star_,
                        *export_name,
                        JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(idx as _),
                    )?;
                } else {
                    let module_name = self.parse_from_clause()?;
                    let dep_idx = md.add_dependency(&self.ctx, module_name);
                    JS_FreeAtom(&self.ctx, module_name);

                    let idx = dep_idx?;
                    add_star_export_entry(&self.ctx, &md, idx)?;
                }
            }

            TOK_DEFAULT => {
                if self.token.val == TOK_CLASS {
                    return self.parse_class(false, JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT);
                } else if self.token.val == TOK_FUNCTION
                    || (self.token_is_pseudo_keyword(JS_ATOM_async)
                        && self.peek_token(true) == TOK_FUNCTION)
                {
                    return unsafe {
                        self.parse_function_decl2(
                            JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                            JSFunctionKindEnum::JS_FUNC_NORMAL,
                            JS_ATOM_NULL,
                            self.token.ptr,
                            self.token.line_num,
                            JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT,
                            std::ptr::null_mut(),
                        )
                    };
                } else {
                    self.parse_assign_expr()?;
                }

                // set the name of anonymous functions
                self.set_object_name(JS_ATOM_default);

                // store the value in the _default_ global variable and export it
                let loc = JS_ATOM__default_;
                self.define_var_cur_func(loc, JSVarDefEnum::JS_VAR_DEF_LET)?;
                self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                self.emit_atom_dup(loc);
                self.emit_u16(0);

                self.add_module_export(
                    Some(&md),
                    loc,
                    JS_ATOM_default,
                    JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                        var_idx: u16::MAX, // unknown for now
                        var_ref: None,
                    },
                )?;
            }

            TOK_VAR | TOK_LET | TOK_CONST => {
                // export var, export let, export const
                return unsafe { self.parse_var(1, tok, true) };
            }

            _ => {
                self.throw_syntax_error("invalid export syntax");
                return Err(-1);
            }
        }

        self.parse_expect_semi()
    }

    /// add export name for specified module or current parsing module.
    /// `local_name`, `export_name` are not consumed.
    pub(super) fn add_module_export(
        &self,
        md: Option<&ModuleDef>,
        local_name: JSAtom,
        export_name: JSAtom,
        export_type: JSExportTypeEnum,
    ) -> Result<(), i32> {
        let md = md.unwrap_or_else(|| {
            // current function def must be module.
            self.cur_func().module.as_ref().unwrap()
        });

        if md.find_export(export_name).is_some() {
            self.throw_syntax_error(&format!(
                "duplicate exported name '{}'",
                export_name.to_utf8(self.ctx.rt()),
            ));
            return Err(-1);
        }

        if md.0.export_entries.borrow().len() == md.0.export_entries.borrow().capacity() {
            md.0.export_entries
                .borrow_mut()
                .try_reserve(4)
                .map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
        }

        md.0.export_entries
            .borrow_mut()
            .push(Box::new(JSExportEntry {
                export_type,
                local_name: local_name.dup(self.ctx.rt()),
                export_name: export_name.dup(self.ctx.rt()),
            }));

        Ok(())
    }

    /// add import for current parsing module.
    /// `local_name`, `import_name` are not consumed.
    pub(super) fn add_module_import(
        &self,
        md: &ModuleDef,
        local_name: JSAtom,
        import_name: JSAtom,
    ) -> Result<(), i32> {
        if local_name == JS_ATOM_arguments || local_name == JS_ATOM_eval {
            self.throw_syntax_error("invalid import binding");
            return Err(-1);
        }

        if local_name != JS_ATOM_default {
            if self
                .cur_func()
                .closure_vars
                .as_ref()
                .is_some_and(|lst| lst.iter().any(|cv| cv.var_name == local_name))
            {
                self.throw_syntax_error("duplicate import binding");
                return Err(-1);
            }
        }

        let ctx = self.ctx.clone();
        let is_local = import_name == JS_ATOM__star_;
        let var_idx = self.cur_func_mut().add_closure_var(
            &ctx,
            is_local,
            false,
            md.0.import_entries.borrow().len() as _,
            local_name,
            true,
            true,
            JSVarKindEnum::JS_VAR_NORMAL,
        )?;

        if md.0.import_entries.borrow().len() == md.0.import_entries.borrow().capacity() {
            md.0.import_entries
                .borrow_mut()
                .try_reserve(2)
                .map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
        }
        md.0.import_entries.borrow_mut().push(JSImportEntry {
            var_idx,
            import_name: import_name.dup(&self.ctx.rt()),
            req_module_idx: 0,
        });

        Ok(())
    }
}

/// Parse an identifier and return its atom.
/// returned atom is duplicated. caller should free it after use.
fn parse_export_name(s: &JSParseState) -> Result<JSAtom, i32> {
    if !JSParseState::token_maybe_ident(s.token.val) {
        s.throw_syntax_error("identifier expected");
        Err(-1)
    } else {
        s.token.atom().ok_or(-1).map(|a| a.dup(s.ctx.rt()))
    }
}

/// return true if 'input' contains the source of a module
/// (heuristic). 'input' must be a zero terminated.
//
/// Heuristic: skip comments and expect 'import' keyword not followed by '(' or '.' or export keyword.
pub unsafe fn JS_DetectModule(input: &[u8]) -> bool {
    let buf_p = input.as_ptr();
    let mut p = skip_shebang(buf_p, buf_p.add(input.len()));

    match simple_next_token(&mut p, false) {
        TOK_IMPORT => {
            let tok = simple_next_token(&mut p, false);
            !matches!(tok, CHAR_BRACKET_L | CHAR_DOT)
        }
        TOK_EXPORT => true,
        _ => false,
    }
}
