use std::ffi::CStr;

use crate::{
    atom::{consts::*, JSAtom, JS_DupAtom, JS_FreeAtom, JS_ValueToAtom, JS_ATOM_NULL},
    bytecode::OpCodeTemp,
    context::JSContext,
    error::JS_ThrowSyntaxErrorAtom,
    function::{JSFunctionKindEnum, JSVarDefEnum, JSVarKindEnum},
    mem::js_resize_array,
    module::{
        add_req_module_entry, find_export_entry, JSExportEntry, JSExportTypeEnum, JSImportEntry,
        JSModuleDef,
    },
};

use super::{
    class::js_parse_class,
    emit::{emit_atom_dup, emit_op_temp, emit_u16},
    error::{js_parse_error, js_parse_error_reserved_identifier},
    function::JSParseFunctionEnum,
    obj::set_object_name,
    state::JSParseState,
    token::{
        peek_token, token_is_ident, token_is_pseudo_keyword, CHAR_COMMA, CHAR_CURLY_R, TOK_CLASS,
        TOK_CONST, TOK_DEFAULT, TOK_FUNCTION, TOK_IDENT, TOK_LET, TOK_STRING, TOK_VAR,
    },
    var::define_var,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSParseExportEnum {
    JS_PARSE_EXPORT_NONE,
    JS_PARSE_EXPORT_NAMED,
    JS_PARSE_EXPORT_DEFAULT,
}

/// Parse an identifier and return its atom
unsafe fn parse_export_name(s: *mut JSParseState) -> JSAtom {
    if !token_is_ident((*s).token.val) {
        js_parse_error(s, c"identifier expected");
        return JS_ATOM_NULL;
    }
    JS_DupAtom((*s).ctx, (*s).token.u.ident.atom)
}

/// Parse an 'as' clause and return the export name
unsafe fn parse_as_clause(s: *mut JSParseState, name: JSAtom) -> JSAtom {
    let ctx = (*s).ctx;
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
    let ctx = (*s).ctx;
    let m = (*(*s).cur_func).module;
    let mut local_name: JSAtom;
    let mut export_name: JSAtom;

    let mut me: *mut JSExportEntry;

    if (*s).next_token().is_err() {
        return -1;
    }

    let tok = (*s).token.val;
    if tok == TOK_CLASS {
        return js_parse_class(s, false, JSParseExportEnum::JS_PARSE_EXPORT_NAMED);
    } else if tok == TOK_FUNCTION
        || (token_is_pseudo_keyword(s, JS_ATOM_async) && peek_token(s, true) == TOK_FUNCTION)
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
            let first_export = (*m).export_entries_count;
            for (local_name, export_name) in exports {
                me = add_export_entry(
                    s,
                    m,
                    local_name,
                    export_name,
                    JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL,
                );
                JS_FreeAtom(ctx, local_name);
                JS_FreeAtom(ctx, export_name);
                if me.is_null() {
                    return -1;
                }
            }

            // Handle re-exports
            if token_is_pseudo_keyword(s, JS_ATOM_from) {
                let module_name = js_parse_from_clause(s);
                if module_name == JS_ATOM_NULL {
                    return -1;
                }
                let idx = add_req_module_entry(ctx, m, module_name);
                JS_FreeAtom(ctx, module_name);
                if idx < 0 {
                    return -1;
                }

                for i in first_export..(*m).export_entries_count {
                    me = &mut *(*m).export_entries.offset(i as isize);
                    (*me).export_type = JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT;
                    (*me).u.req_module_idx = idx;
                }
            }
        }

        // *
        0x2a => {
            export_name = parse_as_clause(s, JS_ATOM__star_);
            if export_name == JS_ATOM_NULL {
                return -1;
            }

            let module_name = js_parse_from_clause(s);
            if module_name == JS_ATOM_NULL {
                JS_FreeAtom(ctx, export_name);
                return -1;
            }
            let idx = add_req_module_entry(ctx, m, module_name);
            JS_FreeAtom(ctx, module_name);
            if idx < 0 {
                JS_FreeAtom(ctx, export_name);
                return -1;
            }
            me = add_export_entry(
                s,
                m,
                JS_ATOM__star_,
                export_name,
                JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT,
            );
            JS_FreeAtom(ctx, export_name);
            if me.is_null() {
                return -1;
            }
            (*me).u.req_module_idx = idx;
        }

        TOK_DEFAULT => {
            if (*s).token.val == TOK_CLASS {
                return js_parse_class(s, false, JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT);
            } else if (*s).token.val == TOK_FUNCTION
                || (token_is_pseudo_keyword(s, JS_ATOM_async)
                    && peek_token(s, true) == TOK_FUNCTION)
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
            if define_var(s, (*s).cur_func, local_name, JSVarDefEnum::JS_VAR_DEF_LET) < 0 {
                return -1;
            }
            emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
            emit_atom_dup(s, local_name);
            emit_u16(s, 0);

            if add_export_entry(
                s,
                m,
                local_name,
                JS_ATOM_default,
                JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL,
            )
            .is_null()
            {
                return -1;
            }
        }

        TOK_VAR | TOK_LET | TOK_CONST => {
            return (*s).parse_var(1, tok, true).map_or(-1, |_| 0);
        }

        _ => {
            return js_parse_error(s, c"invalid export syntax");
        }
    }

    return (*s).parse_expect_semi().map_or(-1, |_| 0);
}

pub unsafe fn js_parse_from_clause(s: *mut JSParseState) -> JSAtom {
    let module_name: JSAtom;

    if !token_is_pseudo_keyword(s, JS_ATOM_from) {
        js_parse_error(s, c"from clause expected");
        return JS_ATOM_NULL;
    }

    if (*s).next_token().is_err() {
        return JS_ATOM_NULL;
    }

    if unsafe { (*s).token.val } != TOK_STRING {
        js_parse_error(s, c"string expected");
        return JS_ATOM_NULL;
    }

    module_name = JS_ValueToAtom((*s).ctx, (*s).token.u.str.str);
    if module_name == JS_ATOM_NULL {
        return JS_ATOM_NULL;
    }

    if (*s).next_token().is_err() {
        JS_FreeAtom((*s).ctx, module_name);
        return JS_ATOM_NULL;
    }

    return module_name;
}

pub unsafe fn js_parse_import(s: *mut JSParseState) -> i32 {
    let ctx = (*s).ctx;
    let m = (*(*s).cur_func).module;

    let mut local_name: JSAtom;
    let mut import_name: JSAtom;
    let mut module_name: JSAtom;

    macro_rules! fail {
        () => {
            JS_FreeAtom(ctx, local_name);
            JS_FreeAtom(ctx, import_name);
            return -1;
        };
    }

    if (*s).next_token().is_err() {
        return -1;
    }

    let first_import = (*m).import_entries_count;
    if (*s).token.val == TOK_STRING {
        module_name = JS_ValueToAtom(ctx, (*s).token.u.str.str);
        if module_name == JS_ATOM_NULL {
            return -1;
        }
        if (*s).next_token().is_err() {
            JS_FreeAtom(ctx, module_name);
            return -1;
        }
    } else {
        if (*s).token.val == TOK_IDENT {
            if (*s).token.u.ident.is_reserved {
                return js_parse_error_reserved_identifier(s);
            }
            // "default" import
            local_name = JS_DupAtom(ctx, (*s).token.u.ident.atom);
            import_name = JS_ATOM_default;
            if (*s).next_token().is_err() {
                fail!();
            }
            if add_import(s, m, local_name, import_name) != 0 {
                fail!();
            }
            JS_FreeAtom(ctx, local_name);

            if (*s).token.val != b',' as i32 {
                module_name = js_parse_from_clause(s);
                if module_name == JS_ATOM_NULL {
                    return -1;
                }
            }
            if (*s).next_token().is_err() {
                return -1;
            }
        }

        if (*s).token.val == b'*' as i32 {
            // name space import
            if (*s).next_token().is_err() {
                return -1;
            }
            if !token_is_pseudo_keyword(s, JS_ATOM_as) {
                return js_parse_error(s, c"expecting 'as'");
            }
            if (*s).next_token().is_err() {
                return -1;
            }
            if !token_is_ident((*s).token.val) {
                js_parse_error(s, c"identifier expected");
                return -1;
            }
            local_name = JS_DupAtom(ctx, (*s).token.u.ident.atom);
            import_name = JS_ATOM__star_;

            if (*s).next_token().is_err() {
                fail!();
            }
            if add_import(s, m, local_name, import_name) != 0 {
                fail!();
            }
            JS_FreeAtom(ctx, local_name);
        } else if (*s).token.val == b'{' as i32 {
            if (*s).next_token().is_err() {
                return -1;
            }

            while (*s).token.val != b'}' as i32 {
                if !token_is_ident((*s).token.val) {
                    js_parse_error(s, c"identifier expected");
                    return -1;
                }
                import_name = JS_DupAtom(ctx, (*s).token.u.ident.atom);
                local_name = JS_ATOM_NULL;
                if (*s).next_token().is_err() {
                    fail!();
                }
                if token_is_pseudo_keyword(s, JS_ATOM_as) {
                    if (*s).next_token().is_err() {
                        fail!();
                    }
                    if !token_is_ident((*s).token.val) {
                        js_parse_error(s, c"identifier expected");
                        fail!();
                    }
                    local_name = JS_DupAtom(ctx, (*s).token.u.ident.atom);
                    if (*s).next_token().is_err() {
                        JS_FreeAtom(ctx, local_name);
                        JS_FreeAtom(ctx, import_name);
                        return -1;
                    }
                } else {
                    local_name = JS_DupAtom(ctx, import_name);
                }
                if add_import(s, m, local_name, import_name) != 0 {
                    fail!();
                }
                JS_FreeAtom(ctx, local_name);
                JS_FreeAtom(ctx, import_name);
                if (*s).token.val != b',' as i32 {
                    break;
                }
                if (*s).next_token().is_err() {
                    return -1;
                }
            }

            if (*s).parse_expect(CHAR_CURLY_R).is_err() {
                return -1;
            }
        }

        module_name = js_parse_from_clause(s);
        if module_name == JS_ATOM_NULL {
            return -1;
        }
    }

    let idx = add_req_module_entry(ctx, m, module_name);
    JS_FreeAtom(ctx, module_name);
    if idx < 0 {
        return -1;
    }
    for i in first_import..(*m).import_entries_count {
        (*(*m).import_entries.offset(i as isize)).req_module_idx = idx;
    }

    return (*s).parse_expect_semi().map_or(-1, |_| 0);
}

pub unsafe fn add_export_entry2(
    ctx: *mut JSContext,
    s: *mut JSParseState,
    m: *mut JSModuleDef,
    local_name: JSAtom,
    export_name: JSAtom,
    export_type: JSExportTypeEnum,
) -> *mut JSExportEntry {
    if !find_export_entry(ctx, m, export_name).is_null() {
        if !s.is_null() {
            js_parse_error(
                s,
                CStr::from_ptr(
                    format!(
                        "duplicate exported name '{}'\0",
                        export_name.to_str((*ctx).rt()),
                    )
                    .as_ptr()
                    .cast(),
                ),
            );
        } else {
            JS_ThrowSyntaxErrorAtom(ctx, c"duplicate exported name '%s'", export_name);
        }

        return std::ptr::null_mut();
    }

    if js_resize_array(
        ctx,
        std::ptr::addr_of_mut!((*m).export_entries).cast(),
        std::mem::size_of::<JSExportEntry>() as _,
        std::ptr::addr_of_mut!((*m).export_entries_size),
        (*m).export_entries_count as i32 + 1,
    ) != 0
    {
        return std::ptr::null_mut();
    }

    let me = (*m).export_entries.offset((*m).export_entries_count as _);
    (*m).export_entries_count += 1;
    std::ptr::write_bytes(me, 0, 1);
    (*me).local_name = JS_DupAtom(ctx, local_name);
    (*me).export_name = JS_DupAtom(ctx, export_name);
    (*me).export_type = export_type;
    me
}

pub unsafe fn add_export_entry(
    s: *mut JSParseState,
    m: *mut JSModuleDef,
    local_name: JSAtom,
    export_name: JSAtom,
    export_type: JSExportTypeEnum,
) -> *mut JSExportEntry {
    add_export_entry2((*s).ctx, s, m, local_name, export_name, export_type)
}

pub unsafe fn add_import(
    s: *mut JSParseState,
    m: *mut JSModuleDef,
    local_name: JSAtom,
    import_name: JSAtom,
) -> i32 {
    let ctx: *mut JSContext = (*s).ctx;

    if local_name == JS_ATOM_arguments || local_name == JS_ATOM_eval {
        return js_parse_error(s, c"invalid import binding");
    }

    if local_name != JS_ATOM_default {
        if (*(*s).cur_func)
            .closure_vars
            .as_ref()
            .map_or(false, |lst| lst.iter().any(|cv| cv.var_name == local_name))
        {
            return js_parse_error(s, c"duplicate import binding");
        }
    }

    let is_local = import_name == JS_ATOM__star_;
    let var_idx = if let Ok(vi) = (*(*s).cur_func).add_closure_var(
        ctx.as_mut().unwrap(),
        is_local,
        false,
        (*m).import_entries_count,
        local_name,
        true,
        true,
        JSVarKindEnum::JS_VAR_NORMAL,
    ) {
        vi
    } else {
        return -1;
    };

    if js_resize_array(
        ctx,
        std::ptr::addr_of_mut!((*m).import_entries).cast(),
        std::mem::size_of::<JSImportEntry>() as _,
        std::ptr::addr_of_mut!((*m).import_entries_size),
        (*m).import_entries_count as i32 + 1,
    ) != 0
    {
        return -1;
    }

    let mi = (*m)
        .import_entries
        .offset((*m).import_entries_count as isize);
    (*m).import_entries_count += 1;
    (*mi).import_name = JS_DupAtom(ctx, import_name);
    (*mi).var_idx = var_idx;

    return 0;
}
