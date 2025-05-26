use std::ops::DerefMut;

use crate::{
    atom::{consts::*, JSAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{OpCode, OpCodeTemp},
    context::{JS_MODE_STRICT, JS_MODE_STRIP},
    eval::{JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MODULE},
    function::{JSFunctionDef, JSFunctionKindEnum, JSVarDefEnum, ARGUMENT_VAR_OFFSET},
    module::JSExportTypeEnum,
    parse::token::{CHAR_EQUAL, CHAR_STAR},
    value::JSValue,
};

use super::{
    destructing::js_parse_destructuring_element,
    emit::{
        emit_atom_dup, emit_class_field_init, emit_goto, emit_label, emit_op, emit_op_temp,
        emit_return, emit_u16, emit_u32, js_is_live_code,
    },
    error::js_parse_error_reserved_identifier,
    esm::{add_export_entry, JSParseExportEnum},
    obj::set_object_name,
    parse::{js_parse_skip_parens_token, reparse_ident_token},
    state::{JSParseState, SKIP_HAS_ASSIGNMENT},
    token::{
        peek_token, token_is_pseudo_keyword, CHAR_BRACKET_L, CHAR_BRACKET_R, CHAR_COMMA,
        CHAR_CURLY_L, CHAR_CURLY_R, TOK_ARROW, TOK_AWAIT, TOK_ELLIPSIS, TOK_IDENT, TOK_LET,
        TOK_VAR, TOK_YIELD,
    },
    var::define_var,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSParseFunctionEnum {
    JS_PARSE_FUNC_STATEMENT,
    JS_PARSE_FUNC_VAR,
    JS_PARSE_FUNC_EXPR,
    JS_PARSE_FUNC_ARROW,
    JS_PARSE_FUNC_GETTER,
    JS_PARSE_FUNC_SETTER,
    JS_PARSE_FUNC_METHOD,
    JS_PARSE_FUNC_CLASS_STATIC_INIT,
    JS_PARSE_FUNC_CLASS_CONSTRUCTOR,
    JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR,
}

impl<'s> JSParseState<'s> {
    /// Creates a new JSFunctionDef as a child of the current function def.
    /// This new function definition is used to parse the function body.
    /// Returns the newly created function definition.
    /// For JS_PARSE_FUNC_STATEMENT, JS_PARSE_FUNC_EXPR, JS_PARSE_FUNC_ARROW and JS_PARSE_FUNC_VAR,
    /// func_name must be JS_ATOM_NULL.
    ///
    /// Note: `func_name` is **CONSUMED** inside.
    unsafe fn parse_function_body_to_sub_fd(
        &mut self,
        func_type: JSParseFunctionEnum,
        func_kind: JSFunctionKindEnum,
        func_name: JSAtom,
        ptr: *const u8,
        function_line_num: i32,
        is_expr: bool,
    ) -> Result<Box<JSFunctionDef>, i32> {
        // save original cur_func
        let fd0 = self.cur_func_ptr;

        let mut sub_fd = JSFunctionDef::new(
            self.ctx.deref_mut(),
            fd0,
            false,
            is_expr,
            self.filename,
            function_line_num,
        )?;

        sub_fd.func_name = func_name; // <-- atom is consumed here.

        sub_fd.has_prototype = matches!(
            func_type,
            JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT
                | JSParseFunctionEnum::JS_PARSE_FUNC_VAR
                | JSParseFunctionEnum::JS_PARSE_FUNC_EXPR
        ) && func_kind == JSFunctionKindEnum::JS_FUNC_NORMAL;

        sub_fd.has_home_object = matches!(
            func_type,
            JSParseFunctionEnum::JS_PARSE_FUNC_METHOD
                | JSParseFunctionEnum::JS_PARSE_FUNC_GETTER
                | JSParseFunctionEnum::JS_PARSE_FUNC_SETTER
                | JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                | JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
        );

        sub_fd.has_arguments_binding = !matches!(
            func_type,
            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW
                | JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT
        );

        sub_fd.has_this_binding = sub_fd.has_arguments_binding;

        sub_fd.is_derived_class_constructor =
            func_type == JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR;

        match func_type {
            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW => {
                sub_fd.new_target_allowed = (*sub_fd.parent).new_target_allowed;
                sub_fd.super_call_allowed = (*sub_fd.parent).super_call_allowed;
                sub_fd.super_allowed = (*sub_fd.parent).super_allowed;
                sub_fd.arguments_allowed = (*sub_fd.parent).arguments_allowed;
            }
            JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT => {
                sub_fd.new_target_allowed = true; // although new.target === undefined
                sub_fd.super_call_allowed = false;
                sub_fd.super_allowed = true;
                sub_fd.arguments_allowed = false;
            }
            _ => {
                sub_fd.new_target_allowed = true;
                sub_fd.super_call_allowed = sub_fd.is_derived_class_constructor;
                sub_fd.super_allowed = sub_fd.has_home_object;
                sub_fd.arguments_allowed = true;
            }
        }

        /* fd->in_function_body == FALSE prevents yield/await during the parsing
        of the arguments in generator/async functions. They are parsed as
        regular identifiers for other function kinds. */
        sub_fd.func_kind = func_kind;
        sub_fd.func_type = func_type;

        // make sub_fd to be the cur_func
        let fd1 = sub_fd.deref_mut();
        self.cur_func_ptr = fd1;

        macro_rules! fail {
            () => {
                self.cur_func_ptr = fd0; // restore original cur_func
                sub_fd.dispose(self.ctx.deref_mut()); // clean up and drop new fd
                return Err(-1);
            };
        }

        //
        // generate opcodes for sub_fd
        //
        if matches!(
            func_type,
            JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                | JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
        ) {
            /* error if not invoked as a constructor */
            emit_op(self, OpCode::CHECK_CTOR);
        }

        if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR {
            emit_class_field_init(self);
        }

        /* parse arguments */
        fd1.has_simple_parameter_list = true;
        fd1.has_parameter_expressions = false;
        let mut has_opt_arg = false;

        if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_ARROW && self.token.val == TOK_IDENT {
            if self.token.u.ident.is_reserved {
                js_parse_error_reserved_identifier(self);
                fail!();
            }

            let name = self.token.u.ident.atom;
            if fd1.add_arg(self.ctx.deref_mut(), name).is_err() {
                fail!();
            };
            fd1.defined_arg_count = 1;
        } else if func_type != JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT {
            if self.token.val == CHAR_BRACKET_L {
                let mut skip_bits: i32 = 0;
                /* if there is an '=' inside the parameter list, we
                consider there is a parameter expression inside */
                js_parse_skip_parens_token(self, &mut skip_bits, false);
                if skip_bits & SKIP_HAS_ASSIGNMENT != 0 {
                    fd1.has_parameter_expressions = true;
                }
                if self.next_token().is_err() {
                    fail!();
                }
            } else {
                if self.parse_expect(CHAR_BRACKET_L).is_err() {
                    fail!();
                }
            }

            if fd1.has_parameter_expressions {
                fd1.scope_level = -1; // force no parent scope
                if self.push_scope().is_err() {
                    fail!();
                }
            }

            while self.token.val != CHAR_BRACKET_R {
                let rest = if self.token.val == TOK_ELLIPSIS {
                    fd1.has_simple_parameter_list = false;
                    if self.next_token().is_err() {
                        fail!();
                    }
                    true
                } else {
                    false
                };

                if matches!(self.token.val as u8, b'[' | b'{') {
                    fd1.has_simple_parameter_list = false;
                    if rest {
                        emit_op(self, OpCode::REST);
                        emit_u16(self, fd1.args.len() as _);
                    } else {
                        // unnamed arg for destructuring
                        let idx = fd1
                            .add_arg(self.ctx.deref_mut(), JS_ATOM_NULL)
                            .expect("TODO") as i32;
                        emit_op(self, OpCode::GET_ARG);
                        emit_u16(self, idx as _);
                    }

                    let has_initializer = js_parse_destructuring_element(
                        self,
                        if fd1.has_parameter_expressions {
                            TOK_LET
                        } else {
                            TOK_VAR
                        },
                        true,
                        true,
                        -1,
                        true,
                    );
                    if has_initializer < 0 {
                        fail!();
                    }
                    if has_initializer != 0 {
                        has_opt_arg = true;
                    }
                    if !has_opt_arg {
                        fd1.defined_arg_count += 1;
                    }
                } else if self.token.val == TOK_IDENT {
                    if self.token.u.ident.is_reserved {
                        js_parse_error_reserved_identifier(self);
                        fail!();
                    }

                    let name = self.token.u.ident.atom;
                    if name == JS_ATOM_yield
                        && fd1.func_kind == JSFunctionKindEnum::JS_FUNC_GENERATOR
                    {
                        js_parse_error_reserved_identifier(self);
                        fail!();
                    }
                    if fd1.has_parameter_expressions {
                        if define_var(self, fd1, name, JSVarDefEnum::JS_VAR_DEF_LET) < 0 {
                            fail!();
                        }
                    }

                    /* XXX: could avoid allocating an argument if rest is true */
                    let idx = if let Ok(id) = fd1.add_arg(self.ctx.deref_mut(), name) {
                        id as i32
                    } else {
                        fail!();
                    };

                    if self.next_token().is_err() {
                        fail!();
                    }

                    if rest {
                        emit_op(self, OpCode::REST);
                        emit_u16(self, idx as _);

                        if fd1.has_parameter_expressions {
                            emit_op(self, OpCode::DUP);
                            emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                            emit_atom_dup(self, name);
                            emit_u16(self, fd1.scope_level as _);
                        }
                        emit_op(self, OpCode::PUT_ARG);
                        emit_u16(self, idx as _);
                        fd1.has_simple_parameter_list = false;
                        has_opt_arg = true;
                    } else if self.token.val == CHAR_EQUAL {
                        fd1.has_simple_parameter_list = false;
                        has_opt_arg = true;

                        if self.next_token().is_err() {
                            fail!();
                        }

                        let label = if let Ok(l) = fd1.add_label() {
                            l as i32
                        } else {
                            fail!();
                        };

                        emit_op(self, OpCode::GET_ARG);
                        emit_u16(self, idx as _);
                        emit_op(self, OpCode::DUP);
                        emit_op(self, OpCode::UNDEFINED);
                        emit_op(self, OpCode::STRICT_EQ);
                        emit_goto(self, OpCode::IF_FALSE, label);
                        emit_op(self, OpCode::DROP);

                        if self.parse_assign_expr().is_err() {
                            fail!();
                        }

                        set_object_name(self, name);
                        emit_op(self, OpCode::DUP);
                        emit_op(self, OpCode::PUT_ARG);
                        emit_u16(self, idx as _);

                        emit_label(self, label);
                        emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        emit_atom_dup(self, name);
                        emit_u16(self, fd1.scope_level as _);
                    } else {
                        if !has_opt_arg {
                            fd1.defined_arg_count += 1;
                        }
                        if fd1.has_parameter_expressions {
                            /* copy the argument to the argument scope */
                            emit_op(self, OpCode::GET_ARG);
                            emit_u16(self, idx as _);
                            emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                            emit_atom_dup(self, name);
                            emit_u16(self, fd1.scope_level as _);
                        }
                    }
                } else {
                    self.throw_syntax_error(c"missing formal parameter");
                    fail!();
                }

                if rest && self.token.val != CHAR_BRACKET_R {
                    let _ = self.parse_expect(CHAR_BRACKET_R);
                    fail!();
                }

                if self.token.val == CHAR_BRACKET_R {
                    break;
                }
                if self.parse_expect(CHAR_COMMA).is_err() {
                    fail!();
                }
            }

            if (func_type == JSParseFunctionEnum::JS_PARSE_FUNC_GETTER && !fd1.args.is_empty())
                || (func_type == JSParseFunctionEnum::JS_PARSE_FUNC_SETTER && fd1.args.len() != 1)
            {
                self.throw_syntax_error(c"invalid number of arguments for getter or setter");
                fail!();
            }
        }

        if fd1.has_parameter_expressions {
            /* Copy the variables in the argument scope to the variable
            scope (see FunctionDeclarationInstantiation() in spec). The
            normal arguments are already present, so no need to copy
            them. */
            let mut idx = fd1.scopes[fd1.scope_level as usize].first;
            while idx >= 0 {
                let (scope_level, var_name, scope_next) = {
                    let vd = fd1.vars.get(idx as usize).unwrap();
                    (vd.scope_level, vd.var_name, vd.scope_next)
                };

                if scope_level != fd1.scope_level {
                    break;
                }

                if fd1.find_var(var_name).is_none() {
                    if fd1.add_var(self.ctx.deref_mut(), var_name).is_err() {
                        fail!();
                    }

                    let vd = fd1.vars.get(idx as usize).unwrap(); // fd->vars may have been reallocated
                    emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                    emit_atom_dup(self, vd.var_name);
                    emit_u16(self, fd1.scope_level as _);

                    emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR);
                    emit_atom_dup(self, vd.var_name);
                    emit_u16(self, 0);
                }

                idx = scope_next;
            }

            /* the argument scope has no parent, hence we don't use pop_scope(s) */
            emit_op_temp(self, OpCodeTemp::LEAVE_SCOPE);
            emit_u16(self, fd1.scope_level as _);

            /* set the variable scope as the current scope */
            fd1.scope_level = 0;
            fd1.scope_first = fd1.scopes[fd1.scope_level as usize].first;
        }

        if self.next_token().is_err() {
            fail!();
        }

        /* generator function: yield after the parameters are evaluated */
        if matches!(
            func_kind,
            JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            emit_op(self, OpCode::INITIAL_YIELD);
        }

        // in generators, yield expression is forbidden during the parsing of the arguments
        fd1.in_function_body = true;
        // enter body scope
        fd1.body_scope = self.push_scope().unwrap() as i32;

        if self.token.val == TOK_ARROW {
            if self.next_token().is_err() {
                fail!();
            }
            if self.token.val != CHAR_CURLY_L {
                if js_parse_function_check_names(self, fd1, func_name) != 0 {
                    fail!();
                }
                if self.parse_assign_expr().is_err() {
                    fail!();
                }

                emit_op(
                    self,
                    if func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL {
                        OpCode::RETURN_ASYNC
                    } else {
                        OpCode::RETURN
                    },
                );

                if fd1.js_mode & JS_MODE_STRIP as u8 == 0 {
                    // save the function source code
                    // the end of the function source code is after the last
                    // token of the function source stored into s->last_ptr
                    debug_assert!(self.last_ptr >= ptr);
                    debug_assert!(fd1.source.is_none());

                    let src_len = self.last_ptr.offset_from(ptr) as usize;
                    if src_len > 0 {
                        let mut src = Vec::<u8>::new();
                        if src.try_reserve_exact(src_len).is_err() {
                            self.ctx.JS_ThrowOutOfMemory();
                            fail!();
                        }
                        src.extend_from_slice(std::slice::from_raw_parts(ptr, src_len));
                        fd1.source.replace(String::from_utf8_unchecked(src));
                    }
                }

                // function declareation done, returns new fd.
                self.cur_func_ptr = fd0; // restore cur_func to previoud fd
                return Ok(sub_fd);
            }
        }

        if func_type != JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT {
            if self.parse_expect(CHAR_CURLY_L).is_err() {
                fail!();
            }
        }

        if self.parse_directives().is_err() {
            fail!();
        }

        /* in strict_mode, check function and argument names */
        if js_parse_function_check_names(self, fd1, func_name) != 0 {
            fail!();
        }

        while self.token.val != CHAR_CURLY_R {
            if self.parse_source_element().is_err() {
                fail!();
            }
        }

        if fd1.js_mode & JS_MODE_STRIP as u8 == 0 {
            // save the function source code
            debug_assert!(self.buf_ptr >= ptr);
            debug_assert!(fd1.source.is_none());

            let src_len = self.buf_ptr.offset_from(ptr) as usize;
            if src_len > 0 {
                let mut src = Vec::<u8>::new();
                if src.try_reserve_exact(src_len).is_err() {
                    self.ctx.JS_ThrowOutOfMemory();
                    fail!();
                }
                src.extend_from_slice(std::slice::from_raw_parts(ptr, src_len));
                let s = String::from_utf8_unchecked(src);
                fd1.source.replace(s);
            }
        }

        // consume the '}'
        if self.next_token().is_err() {
            fail!();
        }

        /* in case there is no return, add one */
        if js_is_live_code(self) {
            emit_return(self, false);
        }

        self.cur_func_ptr = fd0; // restore cur_func to previoud fd
        Ok(sub_fd)
    }

    /// Parse function declaration for function statements, expressions, arrow functions and var declarations.
    /// Returns new created FunctionDef raw pointer on success, or Err on failure. You don't need to free it manually.
    ///
    /// `func_name` must be JS_ATOM_NULL for JS_PARSE_FUNC_STATEMENT and
    /// JS_PARSE_FUNC_EXPR, JS_PARSE_FUNC_ARROW and JS_PARSE_FUNC_VAR.
    /// `func_name` is **NOT CONSUMED** inside.
    pub(super) unsafe fn parse_function_decl2(
        &mut self,
        func_type: JSParseFunctionEnum,
        mut func_kind: JSFunctionKindEnum,
        mut func_name: JSAtom,
        ptr: *const u8,
        function_line_num: i32,
        export_flag: JSParseExportEnum,
        pfd: *mut *mut JSFunctionDef,
    ) -> Result<(), i32> {
        let mut ctx = self.ctx;
        let is_expr = !matches!(
            func_type,
            JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT | JSParseFunctionEnum::JS_PARSE_FUNC_VAR
        );

        // save original cur_func
        let fd0 = self.cur_func_ptr.as_mut().unwrap();

        let mut lexical_func_idx: i32 = -1;
        let mut create_func_var = false;

        match func_type {
            JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT
            | JSParseFunctionEnum::JS_PARSE_FUNC_VAR
            | JSParseFunctionEnum::JS_PARSE_FUNC_EXPR => {
                if func_kind == JSFunctionKindEnum::JS_FUNC_NORMAL
                    && token_is_pseudo_keyword(self, JS_ATOM_async)
                    && peek_token(self, true) != b'\n' as i32
                {
                    self.next_token()?;
                    func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
                }

                self.next_token()?;

                if self.token.val == CHAR_STAR {
                    self.next_token()?;
                    func_kind = std::mem::transmute::<u8, JSFunctionKindEnum>(
                        func_kind as u8 | JSFunctionKindEnum::JS_FUNC_GENERATOR as u8,
                    )
                }

                if self.token.val == TOK_IDENT {
                    if self.token.u.ident.is_reserved
                        || (self.token.u.ident.atom == JS_ATOM_yield
                            && func_type == JSParseFunctionEnum::JS_PARSE_FUNC_EXPR
                            && func_kind as u8 & JSFunctionKindEnum::JS_FUNC_GENERATOR as u8 != 0)
                        || (self.token.u.ident.atom == JS_ATOM_await
                            && ((func_type == JSParseFunctionEnum::JS_PARSE_FUNC_EXPR
                                && (func_kind as u8 & JSFunctionKindEnum::JS_FUNC_ASYNC as u8)
                                    != 0)
                                || func_type
                                    == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT))
                    {
                        js_parse_error_reserved_identifier(self);
                        return Err(-1);
                    }
                }

                if self.token.val == TOK_IDENT
                    || ((self.token.val == TOK_YIELD && fd0.js_mode & JS_MODE_STRICT as u8 == 0
                        || (self.token.val == TOK_AWAIT && !self.is_module))
                        && func_type == JSParseFunctionEnum::JS_PARSE_FUNC_EXPR)
                {
                    func_name = self.token.u.ident.atom.dup(ctx.rt());
                    if self.next_token().is_err() {
                        JS_FreeAtom(ctx.as_ptr(), func_name);
                        return Err(-1);
                    }
                } else if func_type != JSParseFunctionEnum::JS_PARSE_FUNC_EXPR
                    && export_flag != JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT
                {
                    self.throw_syntax_error(c"function name expected");
                    return Err(-1);
                }
            }
            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW => {}
            _ => {
                func_name = func_name.dup(ctx.rt());
            }
        }

        if fd0.is_eval
            && fd0.eval_type == JS_EVAL_TYPE_MODULE
            && matches!(
                func_type,
                JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT
                    | JSParseFunctionEnum::JS_PARSE_FUNC_VAR
            )
        {
            if let Some(v) = fd0.find_global_var(func_name) {
                if v.scope_level == fd0.scope_level {
                    self.throw_syntax_error(
                        c"invalid redefinition of global identifier in module code",
                    );
                    JS_FreeAtom(ctx.as_ptr(), func_name);
                    return Err(-1);
                }
            }
        }

        if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_VAR {
            let func_idx = fd0.find_var(func_name).map_or(-1, |x| x as i32);
            if fd0.js_mode & JS_MODE_STRICT as u8 == 0
                && func_kind == JSFunctionKindEnum::JS_FUNC_NORMAL
                && fd0
                    .find_lexical_decl(func_name, fd0.scope_first, false)
                    .is_none()
                && !(func_idx >= 0 && func_idx & ARGUMENT_VAR_OFFSET != 0)
                && !(func_name == JS_ATOM_arguments && fd0.has_arguments_binding)
            {
                create_func_var = true;
            }

            // Create the lexical name here so that the function closure contains it
            if fd0.is_eval
                && matches!(fd0.eval_type, JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE)
                && fd0.scope_level == fd0.body_scope
            {
                // avoid creating a lexical variable in the global scope. XXX: check annex B
                if let Some(hf) = fd0.find_global_var(func_name) {
                    /* XXX: should check scope chain */
                    if hf.scope_level == fd0.scope_level {
                        self.throw_syntax_error(c"invalid redefinition of global identifier");
                        JS_FreeAtom(ctx.as_ptr(), func_name);
                        return Err(-1);
                    }
                }
            } else {
                /* Always create a lexical name, fail if at the same scope as existing name */
                /* Lexical variable will be initialized upon entering scope */
                lexical_func_idx = define_var(
                    self,
                    fd0,
                    func_name,
                    if func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL {
                        JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL
                    } else {
                        JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
                    },
                );
                if lexical_func_idx < 0 {
                    JS_FreeAtom(ctx.as_ptr(), func_name);
                    return Err(-1);
                }
            }
        }

        // create a sub FunctionDef to parse function body, returns new sub fd.
        let mut fd1 = self.parse_function_body_to_sub_fd(
            func_type,
            func_kind,
            func_name, // this will be consumed
            ptr,
            function_line_num,
            is_expr,
        )?;
        debug_assert_eq!(fd1.parent, fd0 as *mut JSFunctionDef);
        debug_assert_eq!(
            self.cur_func_mut() as *mut JSFunctionDef,
            fd0 as *mut JSFunctionDef
        );

        reparse_ident_token(self);

        // create function object
        {
            func_name = fd1.func_name;

            // the real object will be set at the end of the compilation
            let cp_idx = self
                .cur_func_mut()
                .cpool_add(ctx, JSValue::NULL)
                .map_or(-1, |x| x as i32);

            fd1.parent_cpool_idx = cp_idx;

            if is_expr {
                // for constructors, no code needs to be generated here
                if !matches!(
                    func_type,
                    JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                        | JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
                ) {
                    // OP_fclosure creates the function object from the bytecode and adds the scope information
                    emit_op(self, OpCode::FCLOSURE);
                    emit_u32(self, cp_idx as _);

                    if func_name == JS_ATOM_NULL {
                        emit_op(self, OpCode::SET_NAME);
                        emit_u32(self, JS_ATOM_NULL.0);
                    }
                }
            } else if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_VAR {
                emit_op(self, OpCode::FCLOSURE);
                emit_u32(self, cp_idx as _);

                if create_func_var {
                    if self.cur_func().is_global_var {
                        // the global variable must be defined at the start of the function
                        let js_mode = self.cur_func().js_mode;
                        let hf = self
                            .cur_func_mut()
                            .add_global_var(ctx.deref_mut(), func_name)?;

                        /* it is considered as defined at the top level
                        (needed for annex B.3.3.4 and B.3.3.5
                        checks) */
                        hf.scope_level = 0;
                        hf.set_force_init(js_mode & JS_MODE_STRICT as u8 != 0);

                        /* store directly into global var, bypass lexical scope */
                        emit_op(self, OpCode::DUP);
                        emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR);
                        emit_atom_dup(self, func_name);
                        emit_u16(self, 0);
                    } else {
                        // do not call define_var to bypass lexical scope check
                        if self.cur_func().find_var(func_name).is_none() {
                            self.cur_func_mut().add_var(ctx.deref_mut(), func_name)?;
                        };

                        /* store directly into local var, bypass lexical catch scope */
                        emit_op(self, OpCode::DUP);
                        emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR);
                        emit_atom_dup(self, func_name);
                        emit_u16(self, 0);
                    }
                }

                if lexical_func_idx >= 0 {
                    /* lexical variable will be initialized upon entering scope */
                    self.cur_func_mut()
                        .vars
                        .get_mut(lexical_func_idx as usize)
                        .unwrap()
                        .func_pool_idx = cp_idx;
                    emit_op(self, OpCode::DROP);
                } else {
                    /* store function object into its lexical name */
                    /* XXX: could use OP_put_loc directly */
                    emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                    emit_atom_dup(self, func_name);
                    emit_u16(self, self.cur_func().scope_level as _);
                }
            } else {
                if !self.cur_func().is_global_var {
                    let var_idx = define_var(
                        self,
                        self.cur_func_mut(),
                        func_name,
                        JSVarDefEnum::JS_VAR_DEF_VAR,
                    );
                    if var_idx < 0 {
                        return Err(-1);
                    }

                    // the variable will be assigned at the top of the function
                    if var_idx & ARGUMENT_VAR_OFFSET != 0 {
                        self.cur_func_mut().args[(var_idx - ARGUMENT_VAR_OFFSET) as usize]
                            .func_pool_idx = cp_idx;
                    } else {
                        self.cur_func_mut().vars[var_idx as usize].func_pool_idx = cp_idx;
                    }
                } else {
                    let func_var_name = if func_name == JS_ATOM_NULL {
                        JS_ATOM__default_ // export default
                    } else {
                        func_name
                    };

                    /* the variable will be assigned at the top of the function */
                    let hf = self
                        .cur_func_mut()
                        .add_global_var(ctx.deref_mut(), func_var_name)?;
                    hf.cpool_idx = cp_idx;

                    if export_flag != JSParseExportEnum::JS_PARSE_EXPORT_NONE {
                        if add_export_entry(
                            self,
                            self.cur_func().module,
                            func_var_name,
                            if export_flag == JSParseExportEnum::JS_PARSE_EXPORT_NAMED {
                                func_var_name
                            } else {
                                JS_ATOM_default
                            },
                            JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                                var_idx: u16::MAX,
                                var_ref: None,
                            },
                        )
                        .is_null()
                        {
                            return Err(-1);
                        }
                    }
                }
            }
        }

        // fd1 is added to children list of fd0,
        // rust should give up its ownership.
        let fd_ptr = Box::into_raw(fd1);
        if !pfd.is_null() {
            *pfd = fd_ptr;
        }

        Ok(())
    }

    pub(super) fn parse_function_decl(
        &mut self,
        func_type: JSParseFunctionEnum,
        func_kind: JSFunctionKindEnum,
        func_name: JSAtom,
        ptr: *const u8,
        function_line_num: i32,
    ) -> Result<(), i32> {
        unsafe {
            self.parse_function_decl2(
                func_type,
                func_kind,
                func_name,
                ptr,
                function_line_num,
                JSParseExportEnum::JS_PARSE_EXPORT_NONE,
                std::ptr::null_mut(),
            )
        }
    }
}

pub unsafe fn js_parse_check_duplicate_parameter(s: *mut JSParseState, name: JSAtom) -> i32 {
    // Check for duplicate parameter names
    if (*s).cur_func().args.iter().any(|vd| vd.var_name == name) {
        (*s).throw_syntax_error(c"duplicate parameter names not allowed in this context");
        return -1;
    }

    if (*s).cur_func().vars.iter().any(|vd| vd.var_name == name) {
        (*s).throw_syntax_error(c"duplicate parameter names not allowed in this context");
        return -1;
    }

    0
}

pub unsafe fn js_parse_function_check_names(
    s: *mut JSParseState,
    fd: *mut JSFunctionDef,
    func_name: JSAtom,
) -> i32 {
    if (*fd).js_mode & JS_MODE_STRICT as u8 != 0 {
        if !(*fd).has_simple_parameter_list && (*fd).has_use_strict {
            (*s).throw_syntax_error(
                c"\"use strict\" not allowed in function with default or destructuring parameter",
            );
            return -1;
        }
        if func_name == JS_ATOM_eval || func_name == JS_ATOM_arguments {
            (*s).throw_syntax_error(c"invalid function name in strict code");
            return -1;
        }

        if (*fd)
            .args
            .iter()
            .any(|vd| matches!(vd.var_name, JS_ATOM_eval | JS_ATOM_arguments))
        {
            (*s).throw_syntax_error(c"invalid argument name in strict code");
            return -1;
        }
    }

    // check async_generator case
    if (*fd).js_mode & JS_MODE_STRICT as u8 != 0
        || !(*fd).has_simple_parameter_list
        || ((*fd).func_type == JSParseFunctionEnum::JS_PARSE_FUNC_METHOD
            && (*fd).func_kind == JSFunctionKindEnum::JS_FUNC_ASYNC)
        || matches!(
            (*fd).func_type,
            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW | JSParseFunctionEnum::JS_PARSE_FUNC_METHOD
        )
    {
        for (idx, name) in (*fd).args.iter().enumerate().filter_map(|(i, vd)| {
            if vd.var_name != JS_ATOM_NULL {
                Some((i, vd.var_name))
            } else {
                None
            }
        }) {
            if (*fd).args.iter().take(idx).any(|d| d.var_name == name) {
                (*s).throw_syntax_error(c"duplicate argument names not allowed in this context");
                return -1;
            }

            // Check if argument name duplicates a destructuring parameter
            // XXX: should have a flag for such variables
            if (*fd)
                .vars
                .iter()
                .any(|d| d.var_name == name && d.scope_level == 0)
            {
                (*s).throw_syntax_error(c"duplicate argument names not allowed in this context");
                return -1;
            }
        }
    }

    0
}
