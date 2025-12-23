use crate::{
    atom::{Atom, AtomKind},
    bytecode::OpCodeTemp,
    error::{ErrorKind, JS_BACKTRACE_FLAG_SINGLE_LEVEL},
    function::{JSFunctionKindEnum, JSVarDefEnum, JSVarKindEnum},
    module::{ExportType, ImportEntry, ModuleDef},
    string::Utf8Stream,
};

use super::{
    function::JSParseFunctionEnum, parse::skip_shebang, state::JSParseState, tokenize::TokenKind,
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
        if !self.token_is_pseudo_keyword(Atom::CONST_from) {
            self.throw_syntax_error("from clause expected");
            return Err(-1);
        }

        self.next_token()?;

        if self.token.kind != TokenKind::STRING {
            self.throw_syntax_error("string expected");
            return Err(-1);
        }

        let module_name = self
            .ctx
            .create_string_with_buf(self.token.data.take_string().unwrap())
            .and_then(|s| s.to_atom(self.ctx.rt_ptr()))?;

        self.next_token()?;
        Ok(module_name)
    }

    /// parse import statement
    #[inline(never)]
    pub fn parse_import(&mut self) -> Result<(), i32> {
        let mut md = self.cur_func().module.unwrap();

        self.next_token()?;

        let first_import = md.import_entries.len();
        let module_name: Atom;

        if self.token.kind == TokenKind::STRING {
            // import "module-name"
            module_name = Atom::from_str(
                self.ctx.rt_ptr(),
                &self.token.data.string().unwrap().to_utf8(),
                AtomKind::String,
            )?;

            self.next_token()?;
        } else {
            let done = if self.token.kind == TokenKind::IDENT {
                // import <default>
                if self.token.data.is_reserved() {
                    self.throw_reserved_identifier_error();
                    return Err(-1);
                }

                let local_name = self.token.atom().unwrap();
                self.next_token()?;
                self.add_module_import(md, local_name, Atom::CONST_default)?;

                if self.token.kind == TokenKind::Char(',') {
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
                match self.token.kind {
                    //'*'
                    TokenKind::Char('*') => {
                        // namespace
                        self.next_token()?;

                        if !self.token_is_pseudo_keyword(Atom::CONST_as) {
                            self.throw_syntax_error("expecting 'as'");
                            return Err(-1);
                        }

                        self.next_token()?;

                        if !self.token.kind.is_ident() {
                            self.throw_syntax_error("identifier expected");
                            return Err(-1);
                        }

                        let local_name = self.token.atom().unwrap();
                        self.next_token()?;
                        self.add_module_import(md, local_name, Atom::CONST__star_)?;
                    }
                    // '{'
                    TokenKind::Char('{') => {
                        // name dict
                        self.next_token()?;

                        while self.token.kind != TokenKind::Char('}') {
                            let import_name = match self.token.kind {
                                TokenKind::IDENT => self.token.atom().unwrap(),
                                TokenKind::STRING => Atom::from_str(
                                    self.ctx.rt_ptr(),
                                    &self.token.data.string().unwrap().to_utf8(),
                                    AtomKind::String,
                                )?,
                                t if t.is_keyword() => t.keyword_atom().unwrap(),
                                _ => {
                                    self.throw_syntax_error("identifier or string expected");
                                    return Err(-1);
                                }
                            };

                            self.next_token()?;

                            let local_name = if self.token_is_pseudo_keyword(Atom::CONST_as) {
                                self.next_token()?;

                                if !self.token.kind.is_ident() {
                                    self.throw_syntax_error("identifier expected");
                                    return Err(-1);
                                }

                                let ln = self.token.atom().unwrap();
                                self.next_token()?;
                                ln
                            } else {
                                import_name
                            };

                            self.add_module_import(md, local_name, import_name)?;

                            if self.token.kind == TokenKind::Char(',') {
                                self.next_token()?;
                            } else {
                                break;
                            }
                        }

                        self.parse_expect(TokenKind::Char('}'))?;
                    }
                    _ => {}
                }
            }

            module_name = self.parse_from_clause()?;
        }

        let idx = md.add_dependency(&self.ctx, module_name)?;
        for imp in &mut md.import_entries[first_import..] {
            imp.req_module_idx = idx as _;
        }

        self.parse_expect_semi()?;
        Ok(())
    }

    pub fn parse_export(&mut self) -> Result<(), i32> {
        let mut md = self.cur_func().module.unwrap();

        let tok = self.next_token()?;
        match tok {
            TokenKind::CLASS => {
                // export class ...
                return self.parse_class(false, JSParseExportEnum::JS_PARSE_EXPORT_NAMED);
            }
            TokenKind::FUNCTION => {
                // export function ...
                return {
                    self.parse_function_decl_ex(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        self.token.ptr,
                        self.token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
                        std::ptr::null_mut(),
                    )
                };
            }
            TokenKind::IDENT
                if (self.token_is_pseudo_keyword(Atom::CONST_async)
                    && self.peek_token(true) == TokenKind::FUNCTION) =>
            {
                // export async function ...
                return {
                    self.parse_function_decl_ex(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        self.token.ptr,
                        self.token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
                        std::ptr::null_mut(),
                    )
                };
            }
            TokenKind::VAR | TokenKind::LET | TokenKind::CONST => {
                // export var, export let, export const
                self.next_token()?;
                return self.parse_var(1, tok, true);
            }
            // '{'
            TokenKind::Char('{') => {
                // export {...}
                self.next_token()?;

                let first_export = md.export_entries.len();

                while self.token.kind != TokenKind::Char('}') {
                    let local_name = match self.token.kind {
                        TokenKind::IDENT => self.token.atom().unwrap(),
                        TokenKind::STRING => {
                            let s = self.token.data.string().unwrap();
                            Atom::from_str(self.ctx.rt_ptr(), &s.to_utf8(), AtomKind::String)?
                        }
                        t if t.is_keyword() => t.keyword_atom().unwrap(),
                        _ => {
                            self.throw_syntax_error("identifier or string expected");
                            return Err(-1);
                        }
                    };

                    self.next_token()?;

                    let export_name = if self.token_is_pseudo_keyword(Atom::CONST_as) {
                        // <local_name> as <export_name>
                        self.next_token()?;

                        let n = match self.token.kind {
                            TokenKind::IDENT => self.token.atom().unwrap(),
                            TokenKind::STRING => {
                                let s = self.token.data.string().unwrap();
                                Atom::from_str(self.ctx.rt_ptr(), &s.to_utf8(), AtomKind::String)?
                            }
                            t if t.is_keyword() => t.keyword_atom().unwrap(),
                            _ => {
                                self.throw_syntax_error("identifier or string expected");
                                return Err(-1);
                            }
                        };

                        self.next_token()?;
                        n
                    } else {
                        local_name
                    };

                    self.add_module_export(
                        Some(md),
                        local_name,
                        export_name,
                        ExportType::JS_EXPORT_TYPE_LOCAL {
                            var_idx: u16::MAX, // unknown for now
                            var_ref: None,
                        },
                    )?;

                    if self.token.kind != TokenKind::Char(',') {
                        break;
                    }

                    self.next_token()?;
                }

                self.parse_expect(TokenKind::Char('}'))?;

                // Handle re-exports
                if self.token_is_pseudo_keyword(Atom::CONST_from) {
                    let module_name = self.parse_from_clause()?;
                    let idx = md.add_dependency(&self.ctx, module_name)?;
                    for me in &mut md.export_entries[first_export..] {
                        me.export_type = ExportType::JS_EXPORT_TYPE_INDIRECT(idx as _);
                    }
                }
            }

            TokenKind::Char('*') => {
                self.next_token()?;

                if self.token_is_pseudo_keyword(Atom::CONST_as) {
                    // export ns from
                    self.next_token()?;

                    let export_name = match self.token.kind {
                        TokenKind::IDENT => {
                            // Accept ident, keywords and reserved words as property names
                            self.token.atom().unwrap()
                        }
                        TokenKind::STRING => {
                            let s = self.token.data.string().unwrap();
                            Atom::from_str(self.ctx.rt_ptr(), &s.to_utf8(), AtomKind::String)?
                        }
                        t if t.is_keyword() => t.keyword_atom().unwrap(),
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
                        Some(md),
                        Atom::CONST__star_,
                        export_name,
                        ExportType::JS_EXPORT_TYPE_INDIRECT(idx as _),
                    )?;
                } else {
                    let module_name = self.parse_from_clause()?;
                    let dep_idx = md.add_dependency(&self.ctx, module_name);

                    md.add_star_export_entry(&self.ctx, dep_idx?)?;
                }
            }

            TokenKind::DEFAULT => {
                self.next_token()?;

                if self.token.kind == TokenKind::CLASS {
                    return self.parse_class(false, JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT);
                } else if self.token.kind == TokenKind::FUNCTION
                    || (self.token_is_pseudo_keyword(Atom::CONST_async)
                        && self.peek_token(true) == TokenKind::FUNCTION)
                {
                    return self.parse_function_decl_ex(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        self.token.ptr,
                        self.token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT,
                        std::ptr::null_mut(),
                    );
                } else {
                    self.parse_assign_expr()?;
                }

                // set the name of anonymous functions
                self.set_object_name(Atom::CONST_default);

                // store the value in the _default_ global variable and export it
                let loc = Atom::CONST__default_;
                self.define_var_cur_func(loc, JSVarDefEnum::JS_VAR_DEF_LET)?;
                self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                self.emit_atom(loc);
                self.emit_u16(0);

                self.add_module_export(
                    Some(md),
                    loc,
                    Atom::CONST_default,
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
        md: Option<ModuleDef>,
        local_name: Atom,
        export_name: Atom,
        export_type: ExportType,
    ) -> Result<(), i32> {
        let mut md = md.unwrap_or_else(|| {
            // current function def must have module.
            self.cur_func().module.unwrap()
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
                        self.ctx.rt_ptr().exception(),
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
        mut md: ModuleDef,
        local_name: Atom,
        import_name: Atom,
    ) -> Result<(), i32> {
        if matches!(local_name, Atom::CONST_arguments | Atom::CONST_eval) {
            self.throw_syntax_error("invalid import binding");
            return Err(-1);
        }

        if local_name != Atom::CONST_default
            && self
                .cur_func()
                .closure_vars
                .as_ref()
                .is_some_and(|lst| lst.iter().any(|cv| cv.var_name == local_name))
        {
            self.throw_syntax_error("duplicate import binding");
            return Err(-1);
        }

        let is_local = import_name == Atom::CONST__star_;
        let var_idx = self.cur_func_mut().add_closure_var(
            &self.ctx,
            is_local,
            false,
            md.import_entries.len() as _,
            local_name,
            true,
            true,
            JSVarKindEnum::JS_VAR_NORMAL,
        )?;

        if md.import_entries.len() == md.import_entries.capacity() {
            md.import_entries.try_reserve(2).map_err(|_| {
                self.ctx.throw_out_of_memory();
                -1
            })?;
        }
        md.import_entries.push(ImportEntry {
            var_idx,
            import_name,
            req_module_idx: 0,
        });

        Ok(())
    }

    /// return true if script `buf` is module.
    pub fn detect_module(buf: &[u8]) -> bool {
        // skip comments and expect:
        // * 'import' keyword not followed by '(' or '.' char;
        // * 'export' keyword;

        let n = skip_shebang(buf);

        let mut inp = Utf8Stream::new(&buf[n..]);

        match Self::peek_token_internal(&mut inp, false) {
            TokenKind::IMPORT => !matches!(
                Self::peek_token_internal(&mut inp, false),
                TokenKind::Char('(' | '.')
            ),
            TokenKind::EXPORT => true,
            _ => false,
        }
    }
}
