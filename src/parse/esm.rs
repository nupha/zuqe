// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::{Atom, AtomKind},
    bytecode::OpCodeTemp,
    error::{ErrorKind, JS_BACKTRACE_FLAG_SINGLE_LEVEL},
    function::{FunctionKind, JSVarDefEnum, JSVarKindEnum},
    module::{ExportType, ImportEntry, ModuleDef},
    object::JS_PROP_C_W_E,
    string::{StringRef, Utf8Stream},
    value::JSValue,
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
    /// return (module_name, attributes).
    /// returned atom is duplicated, caller should free it after use.
    fn parse_from_clause(&mut self) -> Result<(Atom, JSValue), i32> {
        if !self.token_is_pseudo_keyword(Atom::CONST_from) {
            self.throw_syntax_error("from clause expected");
            return Err(-1);
        }

        self.next_token()?;

        if self.token.kind != TokenKind::STRING {
            self.throw_syntax_error("string expected");
            return Err(-1);
        }

        let module_name = {
            let sbuf = self.token.data.take_string().unwrap();
            let ctx = self.ctx();
            ctx.create_string_with_buf(sbuf)
                .and_then(|s| s.to_atom(ctx, AtomKind::String))?
        };

        self.next_token()?;

        let attributes = self.parse_with_clause()?;

        Ok((module_name, attributes))
    }

    /// Parse optional `with { key: 'value', ... }` clause after `from '...'`.
    /// Returns either a JS object or JSValue::UNDEFINED if no `with` keyword.
    fn parse_with_clause(&mut self) -> Result<JSValue, i32> {
        if self.token.kind != TokenKind::WITH {
            return Ok(JSValue::UNDEFINED);
        }

        self.next_token()?; // skip 'with'
        self.parse_expect(TokenKind::Char('{'))?; // expect '{'

        let mut attributes;
        let mut keys: Vec<Atom>;
        {
            let ctx = self.ctx();
            attributes = ctx.create_base_object()?;
            keys = Vec::new();
        }

        while self.token.kind != TokenKind::Char('}') {
            // --- Parse key (extract token data before acquiring ctx) ---
            let key = if self.token.kind == TokenKind::STRING {
                let sbuf = self.token.data.take_string().unwrap();
                let ctx = self.ctx();
                let s = ctx.create_string_with_buf(sbuf)?;
                s.to_atom(ctx, AtomKind::String)?
            } else if self.token.kind == TokenKind::IDENT || self.token.kind.is_keyword() {
                self.token.atom().unwrap()
            } else {
                self.throw_syntax_error("identifier or string expected");
                return Err(-1);
            };

            self.next_token()?;

            if self.token.kind != TokenKind::Char(':') {
                self.throw_syntax_error("expected ':'");
                return Err(-1);
            }
            self.next_token()?;

            if self.token.kind != TokenKind::STRING {
                self.throw_syntax_error("string expected");
                return Err(-1);
            }

            // --- Parse value (extract token data before acquiring ctx) ---
            let sbuf = self.token.data.take_string().unwrap();
            let val = {
                let ctx = self.ctx();
                let s = ctx.create_string_with_buf(sbuf)?;
                JSValue::make_string(s)
            };

            // Check for duplicate keys
            if keys.contains(&key) {
                self.throw_syntax_error("duplicate with key");
                return Err(-1);
            }
            keys.push(key);

            // Set property (ctx acquired in a block, then dropped)
            {
                let ctx = self.ctx();
                attributes.define_property_value(ctx, key, val, JS_PROP_C_W_E)?;
            }

            self.next_token()?;

            if self.token.kind == TokenKind::Char(',') {
                self.next_token()?;
            } else {
                break;
            }
        }

        self.parse_expect(TokenKind::Char('}'))?;

        Ok(JSValue::from(attributes))
    }

    /// parse import statement
    #[inline(never)]
    pub fn parse_import(&mut self) -> Result<(), i32> {
        let mut md = self.cur_func().module.unwrap();

        self.next_token()?;

        let first_import = md.import_entries.len();

        if self.token.kind == TokenKind::STRING {
            // import "module-name"
            let sbuf = self.token.data.take_string().unwrap();
            let ctx = self.ctx();
            let s = ctx.create_string_with_buf(sbuf)?;
            let module_name = s.to_atom(ctx, AtomKind::String)?;

            self.next_token()?;

            // import "module-name" with { ... } — optional with clause
            // (parse_with_clause returns UNDEFINED if no 'with' keyword)
            let attrs = self.parse_with_clause()?;
            let idx = md.add_dependency(self.ctx(), module_name, attrs)?;
            for imp in &mut md.import_entries[first_import..] {
                imp.req_module_idx = idx as _;
            }

            self.parse_expect_semi()?;
            return Ok(());
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
                                TokenKind::STRING => {
                                    let t = self.token.data.string().unwrap().to_utf8().to_string();
                                    let s = StringRef::create_utf8(self.ctx(), t.into())?;
                                    Atom::from_string(self.ctx(), s, AtomKind::String)?
                                }
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

            let (module_name, attrs) = self.parse_from_clause()?;

            let idx = md.add_dependency(self.ctx(), module_name, attrs)?;
            for imp in &mut md.import_entries[first_import..] {
                imp.req_module_idx = idx as _;
            }

            self.parse_expect_semi()?;
            Ok(())
        }
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
                return self.parse_function_decl_ex(
                    JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                    FunctionKind::Normal,
                    self.token.ptr,
                    self.token.line_num,
                    JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
                    std::ptr::null_mut(),
                );
            }
            TokenKind::IDENT
                if (self.token_is_pseudo_keyword(Atom::CONST_async)
                    && self.peek_token(true) == TokenKind::FUNCTION) =>
            {
                // export async function ...
                return self.parse_function_decl_ex(
                    JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                    FunctionKind::Normal,
                    self.token.ptr,
                    self.token.line_num,
                    JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
                    std::ptr::null_mut(),
                );
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
                            let t = self.token.data.string().unwrap().to_utf8().to_string();
                            let s = StringRef::create_utf8(self.ctx(), t.into())?;
                            Atom::from_string(self.ctx(), s, AtomKind::String)?
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
                                let t = self.token.data.string().unwrap().to_utf8().to_string();
                                let s = StringRef::create_utf8(self.ctx(), t.into())?;
                                Atom::from_string(self.ctx(), s, AtomKind::String)?
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
                    let (module_name, attrs) = self.parse_from_clause()?;
                    let idx = md.add_dependency(self.ctx(), module_name, attrs)?;
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
                            let t = self.token.data.string().unwrap().to_utf8().to_string();
                            let s = StringRef::create_utf8(self.ctx(), t.into())?;
                            Atom::from_string(self.ctx(), s, AtomKind::String)?
                        }
                        t if t.is_keyword() => t.keyword_atom().unwrap(),
                        _ => {
                            self.throw_syntax_error("identifier or string expected");
                            return Err(-1);
                        }
                    };

                    self.next_token()?;

                    let (module_name, attrs) = self.parse_from_clause()?;
                    let r = md.add_dependency(self.ctx(), module_name, attrs);

                    let idx = r?;
                    self.add_module_export(
                        Some(md),
                        Atom::CONST__star_,
                        export_name,
                        ExportType::JS_EXPORT_TYPE_INDIRECT(idx as _),
                    )?;
                } else {
                    let (module_name, attrs) = self.parse_from_clause()?;
                    let dep_idx = md.add_dependency(self.ctx(), module_name, attrs);

                    md.add_star_export_entry(self.ctx(), dep_idx?)?;
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
                        FunctionKind::Normal,
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
        &mut self,
        md: Option<ModuleDef>,
        local_name: Atom,
        export_name: Atom,
        export_type: ExportType,
    ) -> Result<(), i32> {
        let mut md = md.unwrap_or_else(|| {
            // current function def must have module.
            self.cur_func().module.unwrap()
        });

        match md.add_export_internal(self.ctx(), local_name, export_name, export_type) {
            Ok(_) => Ok(()),
            Err(e) => {
                if e == ErrorKind::Syntax as i32 {
                    let fd = self.cur_func_def;

                    let backtrace_flags =
                        if !fd.is_null() && unsafe { (*fd).attrs.backtrace_barrier } {
                            JS_BACKTRACE_FLAG_SINGLE_LEVEL
                        } else {
                            0
                        };

                    let fln = (self.filename, self.line_num);
                    let exc = self.ctx().rt_mut().exception();
                    let err_val = exc.to_value(self.ctx());
                    self.ctx()
                        .build_backtrace_to(err_val, Some(fln), backtrace_flags);
                }
                Err(e)
            }
        }
    }

    /// add import for current parsing module.
    /// `local_name`, `import_name` are not consumed.
    pub(super) fn add_module_import(
        &mut self,
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

        let var_idx = unsafe {
            (*self.cur_func_def).add_closure_var(
                self.ctx(),
                is_local,
                false,
                md.import_entries.len() as _,
                local_name,
                true,
                true,
                JSVarKindEnum::JS_VAR_NORMAL,
            )?
        };

        if md.import_entries.len() == md.import_entries.capacity() {
            md.import_entries.try_reserve(2).map_err(|_| {
                self.ctx().throw_out_of_memory();
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
}

impl ModuleDef {
    /// return true if script `buf` is module.
    pub fn detect_module(buf: &[u8]) -> bool {
        // skip comments and expect:
        // * 'import' keyword not followed by '(' or '.' char;
        // * 'export' keyword;

        let n = skip_shebang(buf);

        let mut inp = Utf8Stream::new(&buf[n..]);

        match JSParseState::peek_token_internal(&mut inp, false) {
            TokenKind::IMPORT => !matches!(
                JSParseState::peek_token_internal(&mut inp, false),
                TokenKind::Char('(' | '.')
            ),
            TokenKind::EXPORT => true,
            _ => false,
        }
    }
}
