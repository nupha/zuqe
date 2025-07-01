use std::ffi::CStr;

use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_ValueToAtom, JSAtom, consts::*},
    bytecode::OpCodeTemp,
    context::JSContext,
    error::JS_ThrowSyntaxError,
    function::{JSFunctionKindEnum, JSVarDefEnum, JSVarKindEnum},
    mem::js_resize_array,
    module::{
        JSExportEntry, JSExportTypeEnum, JSImportEntry, JSModuleDef, add_req_module_entry,
        find_export_entry,
    },
};

use super::{
    class::js_parse_class,
    emit::{emit_atom_dup, emit_op_temp, emit_u16},
    function::JSParseFunctionEnum,
    obj::set_object_name,
    parse::skip_shebang,
    state::JSParseState,
    token::{
        CHAR_BRACKET_L, CHAR_COMMA, CHAR_CURLY_R, CHAR_DOT, TOK_CLASS, TOK_CONST, TOK_DEFAULT,
        TOK_EXPORT, TOK_FUNCTION, TOK_IDENT, TOK_IMPORT, TOK_LET, TOK_STRING, TOK_VAR,
        simple_next_token, token_is_ident, token_is_pseudo_keyword,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSParseExportEnum {
    JS_PARSE_EXPORT_NONE,
    JS_PARSE_EXPORT_NAMED,
    JS_PARSE_EXPORT_DEFAULT,
}

impl<'s> JSParseState<'s> {
    unsafe fn parse_from_clause(&mut self) -> Result<JSAtom, i32> {
        if !token_is_pseudo_keyword(self, JS_ATOM_from) {
            self.throw_syntax_error("from clause expected");
            return Err(-1);
        }

        self.next_token()?;

        if self.token.val != TOK_STRING {
            self.throw_syntax_error("string expected");
            return Err(-1);
        }

        let module_name = JS_ValueToAtom(&self.ctx, &self.token.data.string().unwrap());
        if module_name == JS_ATOM_NULL {
            return Err(-1);
        }

        if self.next_token().is_err() {
            JS_FreeAtom(&self.ctx, module_name);
            return Err(-1);
        }

        Ok(module_name)
    }

    /// parse import statement
    pub unsafe fn parse_import(&mut self) -> Result<(), i32> {
        let m = self.cur_func().module.clone().unwrap();

        self.next_token()?;

        let first_import = m.0.borrow().import_entries_count;
        let module_name: JSAtom;

        if self.token.val == TOK_STRING {
            module_name = JS_ValueToAtom(&self.ctx, &self.token.data.string().unwrap());
            if module_name == JS_ATOM_NULL {
                return Err(-1);
            }
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
                        .data
                        .ident_atom()
                        .unwrap()
                        .dup(&self.ctx.rt())
                        .into_rc(&self.ctx.rt());

                    self.next_token()?;
                    if add_import(self, &m, *local_name, JS_ATOM_default) != 0 {
                        return Err(-1);
                    }

                    if self.token.val != ',' as i32 {
                        break 'outer;
                    } else {
                        self.next_token()?;
                    }
                }

                if self.token.val == '*' as i32 {
                    // name space import
                    self.next_token()?;

                    if !token_is_pseudo_keyword(self, JS_ATOM_as) {
                        self.throw_syntax_error("expecting 'as'");
                        return Err(-1);
                    }

                    self.next_token()?;

                    if !token_is_ident(self.token.val) {
                        self.throw_syntax_error("identifier expected");
                        return Err(-1);
                    }

                    let local_name = self
                        .token
                        .data
                        .ident_atom()
                        .unwrap()
                        .dup(&self.ctx.rt())
                        .into_rc(&self.ctx.rt());

                    self.next_token()?;
                    if add_import(self, &m, *local_name, JS_ATOM__star_) != 0 {
                        return Err(-1);
                    }
                } else if self.token.val == '{' as i32 {
                    self.next_token()?;

                    while self.token.val != '}' as i32 {
                        if !token_is_ident(self.token.val) {
                            self.throw_syntax_error("identifier expected");
                            return Err(-1);
                        }

                        let import_name = self
                            .token
                            .data
                            .ident_atom()
                            .unwrap()
                            .dup(&self.ctx.rt())
                            .into_rc(&self.ctx.rt());

                        self.next_token()?;

                        let local_name = if token_is_pseudo_keyword(self, JS_ATOM_as) {
                            self.next_token()?;

                            if !token_is_ident(self.token.val) {
                                self.throw_syntax_error("identifier expected");
                                return Err(-1);
                            }

                            let ln = self.token.data.ident_atom().unwrap().dup(&self.ctx.rt());
                            self.next_token()?;
                            ln
                        } else {
                            import_name.dup(&self.ctx.rt())
                        }
                        .into_rc(&self.ctx.rt());

                        if add_import(self, &m, *local_name, *import_name) != 0 {
                            return Err(-1);
                        }

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

        let idx = add_req_module_entry(&self.ctx, &m, module_name);
        JS_FreeAtom(&self.ctx, module_name);
        if idx < 0 {
            return Err(-1);
        }
        for i in first_import..m.0.borrow().import_entries_count {
            (*m.0.borrow_mut().import_entries.offset(i as isize)).req_module_idx = idx;
        }

        self.parse_expect_semi()
    }
}

/// Parse an identifier and return its atom
unsafe fn parse_export_name(s: *mut JSParseState) -> JSAtom {
    if !token_is_ident((*s).token.val) {
        (*s).throw_syntax_error("identifier expected");
        return JS_ATOM_NULL;
    }
    (*s).token.data.ident_atom().unwrap().dup(&(*s).ctx.rt())
}

/// Parse an 'as' clause and return the export name
unsafe fn parse_as_clause(s: *mut JSParseState, name: JSAtom) -> JSAtom {
    let ctx = &(*s).ctx;
    let mut export_name = name;

    if token_is_pseudo_keyword(s, JS_ATOM_as) {
        if (*s).next_token().is_err() {
            JS_FreeAtom(ctx, export_name);
            return JS_ATOM_NULL;
        }
        export_name = parse_export_name(s);
        if export_name == JS_ATOM_NULL {
            return JS_ATOM_NULL;
        }
        if (*s).next_token().is_err() {
            JS_FreeAtom(ctx, export_name);
            return JS_ATOM_NULL;
        }
    }

    export_name
}

pub unsafe fn js_parse_export(s: *mut JSParseState) -> i32 {
    let ctx = &(*s).ctx;
    let m: &JSModuleDef = (*s).cur_func().module.as_ref().unwrap();

    let mut local_name: JSAtom;
    let mut export_name: JSAtom;

    if (*s).next_token().is_err() {
        return -1;
    }

    let tok = (*s).token.val;
    if tok == TOK_CLASS {
        return js_parse_class(
            s.as_mut().unwrap(),
            false,
            JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
        );
    } else if tok == TOK_FUNCTION
        || (token_is_pseudo_keyword(s, JS_ATOM_async) && (*s).peek_token(true) == TOK_FUNCTION)
    {
        return (*s)
            .parse_function_decl2(
                JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                JSFunctionKindEnum::JS_FUNC_NORMAL,
                JS_ATOM_NULL,
                (*s).token.ptr,
                (*s).token.line_num,
                JSParseExportEnum::JS_PARSE_EXPORT_NAMED,
                std::ptr::null_mut(),
            )
            .map_or(-1, |_| 0);
    }

    if (*s).next_token().is_err() {
        return -1;
    }

    match tok {
        // '{'
        0x7b => {
            // Get all named exports first
            let mut exports = Vec::new();
            while (*s).token.val != b'}' as i32 {
                local_name = parse_export_name(s);
                if local_name == JS_ATOM_NULL {
                    return -1;
                }
                export_name = JS_ATOM_NULL;
                if (*s).next_token().is_err() {
                    JS_FreeAtom(ctx, local_name);
                    return -1;
                }
                export_name = parse_as_clause(s, local_name);
                if export_name == JS_ATOM_NULL {
                    JS_FreeAtom(ctx, local_name);
                    return -1;
                }
                exports.push((local_name, export_name));

                if (*s).token.val != CHAR_COMMA {
                    break;
                }
                if (*s).next_token().is_err() {
                    return -1;
                }
            }

            if (*s).parse_expect(CHAR_CURLY_R).is_err() {
                return -1;
            }

            // Process named exports
            let first_export = m.0.borrow().export_entries.len();
            for (local_name, export_name) in exports {
                match add_export_entry(
                    s.as_mut().unwrap(),
                    m,
                    local_name,
                    export_name,
                    JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                        var_idx: u16::MAX, // unknown for now
                        var_ref: None,
                    },
                ) {
                    Ok(_) => {
                        JS_FreeAtom(ctx, local_name);
                        JS_FreeAtom(ctx, export_name);
                    }
                    Err(e) => {
                        return -1;
                    }
                }

                // me = add_export_entry(
                //     s,
                //     m,
                //     local_name,
                //     export_name,
                //     JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                //         var_idx: u16::MAX, // unknown for now
                //         var_ref: None,
                //     },
                // );
                // JS_FreeAtom(ctx, local_name);
                // JS_FreeAtom(ctx, export_name);
                // if me.is_null() {
                //     return -1;
                // }
            }

            // Handle re-exports
            if token_is_pseudo_keyword(s, JS_ATOM_from) {
                let module_name = if let Ok(n) = (*s).parse_from_clause() {
                    n
                } else {
                    return -1;
                };
                let idx = add_req_module_entry(ctx, m, module_name);
                JS_FreeAtom(ctx, module_name);
                if idx < 0 {
                    return -1;
                }

                for me in &mut m.0.borrow_mut().export_entries[first_export..] {
                    me.export_type = JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(idx);
                }
            }
        }

        // *
        0x2a => {
            export_name = parse_as_clause(s, JS_ATOM__star_);
            if export_name == JS_ATOM_NULL {
                return -1;
            }

            let module_name = if let Ok(n) = (*s).parse_from_clause() {
                n
            } else {
                JS_FreeAtom(ctx, export_name);
                return -1;
            };
            let idx = add_req_module_entry(ctx, m, module_name);
            JS_FreeAtom(ctx, module_name);
            if idx < 0 {
                JS_FreeAtom(ctx, export_name);
                return -1;
            }

            let me = add_export_entry(
                s.as_mut().unwrap(),
                m,
                JS_ATOM__star_,
                export_name,
                JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(idx),
            );
            JS_FreeAtom(ctx, export_name);
            if me.is_err() {
                return -1;
            }
        }

        TOK_DEFAULT => {
            if (*s).token.val == TOK_CLASS {
                return js_parse_class(
                    s.as_mut().unwrap(),
                    false,
                    JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT,
                );
            } else if (*s).token.val == TOK_FUNCTION
                || (token_is_pseudo_keyword(s, JS_ATOM_async)
                    && (*s).peek_token(true) == TOK_FUNCTION)
            {
                return (*s)
                    .parse_function_decl2(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        JS_ATOM_NULL,
                        (*s).token.ptr,
                        (*s).token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT,
                        std::ptr::null_mut(),
                    )
                    .map_or(-1, |_| 0);
            } else {
                if (*s).parse_assign_expr().is_err() {
                    return -1;
                }
            }
            set_object_name(s, JS_ATOM_default);

            local_name = JS_ATOM__default_;
            if (*s)
                .define_var_cur_func(local_name, JSVarDefEnum::JS_VAR_DEF_LET)
                .is_err()
            {
                return -1;
            }
            emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
            emit_atom_dup(s, local_name);
            emit_u16(s, 0);

            if add_export_entry(
                s.as_mut().unwrap(),
                m,
                local_name,
                JS_ATOM_default,
                JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                    var_idx: u16::MAX, // unknown for now
                    var_ref: None,
                },
            )
            .is_err()
            {
                return -1;
            }
        }

        TOK_VAR | TOK_LET | TOK_CONST => {
            return (*s).parse_var(1, tok, true).map_or(-1, |_| 0);
        }

        _ => {
            (*s).throw_syntax_error("invalid export syntax");
            return -1;
        }
    }

    (*s).parse_expect_semi().map_or(-1, |_| 0)
}

unsafe fn add_export_entry2(
    ctx: &JSContext,
    s: *mut JSParseState,
    m: &JSModuleDef,
    local_name: JSAtom,
    export_name: JSAtom,
    export_type: JSExportTypeEnum,
) -> Result<(), i32> {
    if find_export_entry(m, export_name).is_some() {
        if !s.is_null() {
            (*s).throw_syntax_error(&format!(
                "duplicate exported name '{}'",
                export_name.to_str(ctx.rt()),
            ));
        } else {
            JS_ThrowSyntaxError(
                ctx,
                &format!("duplicate exported name '{}'", export_name.to_str(ctx.rt())),
            );
        }

        return Err(-1);
    }

    m.with_mut(|m| {
        if m.export_entries.len() == m.export_entries.capacity() {
            if m.export_entries.try_reserve_exact(4).is_err() {
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }
        Ok(())
    })?;

    let exp = Box::new(JSExportEntry {
        export_type,
        local_name: local_name.dup(&ctx.rt()),
        export_name: export_name.dup(&ctx.rt()),
    });
    m.0.borrow_mut().export_entries.push(exp);

    Ok(())
}

pub fn add_export_entry<'a>(
    s: &mut JSParseState,
    m: &JSModuleDef,
    local_name: JSAtom,
    export_name: JSAtom,
    export_type: JSExportTypeEnum,
) -> Result<(), i32> {
    let ctx = s.ctx.clone();
    unsafe { add_export_entry2(&ctx, s, m, local_name, export_name, export_type) }
}

pub unsafe fn add_import(
    s: *mut JSParseState,
    m: &JSModuleDef,
    local_name: JSAtom,
    import_name: JSAtom,
) -> i32 {
    if local_name == JS_ATOM_arguments || local_name == JS_ATOM_eval {
        (*s).throw_syntax_error("invalid import binding");
        return -1;
    }

    if local_name != JS_ATOM_default {
        if (*s)
            .cur_func()
            .closure_vars
            .as_ref()
            .is_some_and(|lst| lst.iter().any(|cv| cv.var_name == local_name))
        {
            (*s).throw_syntax_error("duplicate import binding");
            return -1;
        }
    }

    let is_local = import_name == JS_ATOM__star_;
    let var_idx = if let Ok(vi) = (*s).cur_func_mut().add_closure_var(
        &(*s).ctx,
        is_local,
        false,
        m.0.borrow().import_entries_count,
        local_name,
        true,
        true,
        JSVarKindEnum::JS_VAR_NORMAL,
    ) {
        vi
    } else {
        return -1;
    };

    {
        let mut md = m.0.borrow_mut();

        if js_resize_array(
            &(*s).ctx,
            std::ptr::addr_of_mut!(md.import_entries).cast(),
            std::mem::size_of::<JSImportEntry>() as _,
            std::ptr::addr_of_mut!(m.0.borrow_mut().import_entries_size),
            md.import_entries_count as i32 + 1,
        ) != 0
        {
            return -1;
        }

        let mi = md.import_entries.offset(md.import_entries_count as isize);
        md.import_entries_count += 1;

        (*mi).import_name = import_name.dup(&(*s).ctx.rt());
        (*mi).var_idx = var_idx;
    }

    0
}

/// return true if 'input' contains the source of a module
/// (heuristic). 'input' must be a zero terminated.
//
/// Heuristic: skip comments and expect 'import' keyword not followed by '(' or '.' or export keyword.
pub unsafe fn JS_DetectModule(input: &CStr) -> bool {
    let mut p = input.as_ptr().cast::<u8>();

    skip_shebang(&mut p, p.add(input.count_bytes()));
    match simple_next_token(&mut p, false) {
        TOK_IMPORT => {
            let tok = simple_next_token(&mut p, false);
            !matches!(tok, CHAR_BRACKET_L | CHAR_DOT)
        }
        TOK_EXPORT => true,
        _ => false,
    }
}
