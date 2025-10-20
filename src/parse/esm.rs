use std::ops::Deref;

use crate::{
    atom::{Atom, AtomTypeHint, consts::*},
    bytecode::OpCodeTemp,
    error::{ErrorKind, JS_BACKTRACE_FLAG_SINGLE_LEVEL},
    function::{JSFunctionKindEnum, JSVarDefEnum, JSVarKindEnum},
    module::{ExportType, JSImportEntry, ModuleDef},
    parse::token::{TOK_FIRST_KEYWORD, TOK_LAST_KEYWORD},
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
    fn parse_from_clause(&mut self) -> Result<Atom, i32> {
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
            .and_then(|s| s.to_atom())?;

        self.next_token()?;
        Ok(module_name)
    }

    /// parse import statement
    pub fn parse_import(&mut self) -> Result<(), i32> {
        let md = self.cur_func().module.clone().unwrap();

        self.next_token()?;

        let first_import = md.0.import_entries.borrow().len();
        let module_name: Atom;

        if self.token.val == TOK_STRING {
            // import "module-name"
            module_name = Atom::from_str(
                self.ctx.rt_mut(),
                self.token.string().unwrap().to_utf8(false).as_ref(),
                AtomTypeHint::String,
            )?;

            self.next_token()?;
        } else {
            let done = if self.token.val == TOK_IDENT {
                // import <default>
                if self.token.data.is_reserved() {
                    self.throw_reserved_identifier_error();
                    return Err(-1);
                }

                let local_name = self.token.atom().unwrap();
                self.next_token()?;
                self.add_module_import(&md, &local_name, JS_ATOM_default.deref())?;

                if self.token.val == ',' as i32 {
                    // default import mixed with namespace or named import.
                    self.next_token()?;
                    false // <-- go on parsing namespace or named import.
                } else {
                    true
                }
            } else {
                false
            };

            if !done {
                match self.token.val {
                    //'*'
                    0x2a => {
                        // namespace
                        self.next_token()?;

                        if !self.token_is_pseudo_keyword(JS_ATOM_as) {
                            self.throw_syntax_error("expecting 'as'");
                            return Err(-1);
                        }

                        self.next_token()?;

                        if !Self::token_is_ident(self.token.val) {
                            self.throw_syntax_error("identifier expected");
                            return Err(-1);
                        }

                        let local_name = self.token.atom().unwrap();
                        self.next_token()?;
                        self.add_module_import(&md, &local_name, JS_ATOM__star_.deref())?;
                    }
                    // '{'
                    0x7b => {
                        // name dict
                        self.next_token()?;

                        while self.token.val != '}' as i32 {
                            let import_name = match self.token.val {
                                TOK_IDENT | TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD => {
                                    // Accept ident, keywords and reserved words as property names
                                    self.token.atom().unwrap()
                                }
                                TOK_STRING => Atom::from_str(
                                    self.ctx.rt_mut(),
                                    &self.token.string().unwrap().to_utf8(false),
                                    AtomTypeHint::String,
                                )?,
                                _ => {
                                    self.throw_syntax_error("identifier or string expected");
                                    return Err(-1);
                                }
                            };

                            self.next_token()?;

                            let local_name = if self.token_is_pseudo_keyword(JS_ATOM_as) {
                                self.next_token()?;

                                if !Self::token_is_ident(self.token.val) {
                                    self.throw_syntax_error("identifier expected");
                                    return Err(-1);
                                }

                                let ln = self.token.atom().unwrap();
                                self.next_token()?;
                                ln
                            } else {
                                import_name.clone()
                            };

                            self.add_module_import(&md, &local_name, &import_name)?;

                            if self.token.val == b',' as i32 {
                                self.next_token()?;
                            } else {
                                break;
                            }
                        }

                        self.parse_expect(CHAR_CURLY_R)?;
                    }
                    _ => {}
                }
            }

            module_name = self.parse_from_clause()?;
        }

        let idx = md.add_dependency(&self.ctx, module_name)?;
        for imp in &mut md.0.import_entries.borrow_mut()[first_import..] {
            imp.req_module_idx = idx as _;
        }

        self.parse_expect_semi()?;
        Ok(())
    }

    pub fn parse_export(&mut self) -> Result<(), i32> {
        let md = self.cur_func().module.as_ref().cloned().unwrap();

        let tok = self.next_token()?;
        match tok {
            TOK_CLASS => {
                // export class ...
                return self.parse_class(false, JSParseExportEnum::JS_PARSE_EXPORT_NAMED);
            }
            TOK_FUNCTION => {
                // export function ...
                return {
                    self.parse_function_decl2(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        &Atom::NULL,
                        self.token.ptr,
                        self.token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
                        std::ptr::null_mut(),
                    )
                };
            }
            TOK_IDENT
                if (self.token_is_pseudo_keyword(JS_ATOM_async)
                    && self.peek_token(true) == TOK_FUNCTION) =>
            {
                // export async function ...
                return {
                    self.parse_function_decl2(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        &Atom::NULL,
                        self.token.ptr,
                        self.token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
                        std::ptr::null_mut(),
                    )
                };
            }
            TOK_VAR | TOK_LET | TOK_CONST => {
                // export var, export let, export const
                self.next_token()?;
                return self.parse_var(1, tok, true);
            }
            // '{'
            0x7b => {
                // export {...}
                self.next_token()?;

                let first_export = md.0.export_entries.borrow().len();

                while self.token.val != b'}' as i32 {
                    let local_name = match self.token.val {
                        TOK_IDENT | TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD => {
                            // Accept ident, keywords and reserved words as property names
                            self.token.atom().unwrap()
                        }
                        TOK_STRING => {
                            let s = self.token.string().unwrap();
                            Atom::from_str(
                                self.ctx.rt_mut(),
                                &s.to_utf8(false),
                                AtomTypeHint::String,
                            )?
                        }
                        _ => {
                            self.throw_syntax_error("identifier or string expected");
                            return Err(-1);
                        }
                    };

                    self.next_token()?;

                    let export_name = if self.token_is_pseudo_keyword(JS_ATOM_as) {
                        // <local_name> as <export_name>
                        self.next_token()?;

                        let n = match self.token.val {
                            TOK_IDENT | TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD => {
                                // Accept ident, keywords and reserved words as property names
                                self.token.atom().unwrap()
                            }
                            TOK_STRING => {
                                let s = self.token.string().unwrap();
                                Atom::from_str(
                                    self.ctx.rt_mut(),
                                    &s.to_utf8(false),
                                    AtomTypeHint::String,
                                )?
                            }
                            _ => {
                                self.throw_syntax_error("identifier or string expected");
                                return Err(-1);
                            }
                        };

                        self.next_token()?;
                        n
                    } else {
                        local_name.clone()
                    };

                    self.add_module_export(
                        Some(&md),
                        &local_name,
                        &export_name,
                        ExportType::JS_EXPORT_TYPE_LOCAL {
                            var_idx: u16::MAX, // unknown for now
                            var_ref: None,
                        },
                    )?;

                    if self.token.val != CHAR_COMMA {
                        break;
                    }

                    self.next_token()?;
                }

                self.parse_expect('}' as i32)?;

                // Handle re-exports
                if self.token_is_pseudo_keyword(JS_ATOM_from) {
                    let module_name = self.parse_from_clause()?;
                    let idx = md.add_dependency(&self.ctx, module_name)?;
                    for me in &mut md.0.export_entries.borrow_mut()[first_export..] {
                        me.export_type = ExportType::JS_EXPORT_TYPE_INDIRECT(idx as _);
                    }
                }
            }

            // *
            0x2a => {
                self.next_token()?;

                if self.token_is_pseudo_keyword(JS_ATOM_as) {
                    // export ns from
                    self.next_token()?;

                    let export_name = match self.token.val {
                        TOK_IDENT | TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD => {
                            // Accept ident, keywords and reserved words as property names
                            self.token.atom().unwrap()
                        }
                        TOK_STRING => {
                            let s = self.token.string().unwrap();
                            Atom::from_str(
                                self.ctx.rt_mut(),
                                &s.to_utf8(false),
                                AtomTypeHint::String,
                            )?
                        }
                        _ => {
                            self.throw_syntax_error("identifier or string expected");
                            return Err(-1);
                        }
                    };

                    self.next_token()?;

                    let module_name = self.parse_from_clause()?;
                    let r = md.add_dependency(&self.ctx, module_name);

                    let idx = r?;
                    self.add_module_export(
                        Some(&md),
                        JS_ATOM__star_.deref(),
                        &export_name,
                        ExportType::JS_EXPORT_TYPE_INDIRECT(idx as _),
                    )?;
                } else {
                    let module_name = self.parse_from_clause()?;
                    let dep_idx = md.add_dependency(&self.ctx, module_name);

                    md.add_star_export_entry(&self.ctx, dep_idx?)?;
                }
            }

            TOK_DEFAULT => {
                self.next_token()?;

                if self.token.val == TOK_CLASS {
                    return self.parse_class(false, JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT);
                } else if self.token.val == TOK_FUNCTION
                    || (self.token_is_pseudo_keyword(JS_ATOM_async)
                        && self.peek_token(true) == TOK_FUNCTION)
                {
                    return self.parse_function_decl2(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        &Atom::NULL,
                        self.token.ptr,
                        self.token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT,
                        std::ptr::null_mut(),
                    );
                } else {
                    self.parse_assign_expr()?;
                }

                // set the name of anonymous functions
                self.set_object_name(Atom::new_const(JS_ATOM_default));

                // store the value in the _default_ global variable and export it
                let loc = Atom::new_const(JS_ATOM__default_);
                self.define_var_cur_func(&loc, JSVarDefEnum::JS_VAR_DEF_LET)?;
                self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                self.emit_atom(loc.clone());
                self.emit_u16(0);

                self.add_module_export(
                    Some(&md),
                    &loc,
                    JS_ATOM_default.deref(),
                    ExportType::JS_EXPORT_TYPE_LOCAL {
                        var_idx: u16::MAX, // unknown for now
                        var_ref: None,
                    },
                )?;
            }

            _ => {
                self.throw_syntax_error("invalid export syntax");
                return Err(-1);
            }
        }

        self.parse_expect_semi()?;
        Ok(())
    }

    /// add export name for specified module or current parsing module.
    /// `local_name`, `export_name` are not consumed.
    pub(super) fn add_module_export(
        &self,
        md: Option<&ModuleDef>,
        local_name: &Atom,
        export_name: &Atom,
        export_type: ExportType,
    ) -> Result<(), i32> {
        let md = md.unwrap_or_else(|| {
            // current function def must have module.
            self.cur_func().module.as_ref().unwrap()
        });

        match md.add_export_internal(&self.ctx, local_name, export_name, export_type) {
            Ok(_) => Ok(()),
            Err(e) => {
                if e == ErrorKind::Syntax as i32 {
                    let backtrace_flags = if !self.cur_func_def.is_null()
                        && self.cur_func().attrs.backtrace_barrier
                    {
                        JS_BACKTRACE_FLAG_SINGLE_LEVEL
                    } else {
                        0
                    };
                    self.ctx.build_backtrace_to(
                        &self.ctx.rt().cur_exception,
                        Some((self.filename, self.line_num)),
                        backtrace_flags,
                    );
                }
                Err(e)
            }
        }
    }

    /// add import for current parsing module.
    /// `local_name`, `import_name` are not consumed.
    pub(super) fn add_module_import(
        &self,
        md: &ModuleDef,
        local_name: &Atom,
        import_name: &Atom,
    ) -> Result<(), i32> {
        if matches!(local_name.to_ref(), JS_ATOM_arguments | JS_ATOM_eval) {
            self.throw_syntax_error("invalid import binding");
            return Err(-1);
        }

        if local_name.to_ref() != JS_ATOM_default {
            if self
                .cur_func()
                .closure_vars
                .as_ref()
                .is_some_and(|lst| lst.iter().any(|cv| cv.var_name.eq(local_name)))
            {
                self.throw_syntax_error("duplicate import binding");
                return Err(-1);
            }
        }

        let is_local = import_name.to_ref() == JS_ATOM__star_;
        let var_idx = self.cur_func_mut().add_closure_var(
            &self.ctx,
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
            import_name: import_name.clone(),
            req_module_idx: 0,
        });

        Ok(())
    }

    /// return true if `input` is esm source, that means skip comments and expect:
    /// * 'import' keyword not followed by '(' or '.' char;
    /// * 'export' keyword;
    pub fn detect_module(input: &[u8]) -> bool {
        let buf_p = input.as_ptr();
        let mut p = skip_shebang(buf_p, unsafe { buf_p.add(input.len()) });

        match unsafe { simple_next_token(&mut p, false) } {
            TOK_IMPORT => {
                let tok = unsafe { simple_next_token(&mut p, false) };
                !matches!(tok, CHAR_BRACKET_L | CHAR_DOT)
            }
            TOK_EXPORT => true,
            _ => false,
        }
    }
}
