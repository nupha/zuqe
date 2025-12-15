use std::ops::DerefMut;

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp},
    eval::EvalType,
    function::{ARGUMENT_VAR_OFFSET, JSFunctionDef, JSFunctionKindEnum, JSVarDefEnum},
    module::ExportType,
    stack_frame::ExecMode,
    utils::unlikely,
    value::JSValue,
};

use super::{
    emit::js_is_live_code,
    esm::JSParseExportEnum,
    state::{JSParseState, SKIP_HAS_ASSIGNMENT},
    token::{
        CHAR_BRACKET_R, CHAR_COMMA, CHAR_CURLY_L, CHAR_CURLY_R, CHAR_EQUAL, CHAR_STAR, TOK_ARROW,
        TOK_AWAIT, TOK_ELLIPSIS, TOK_FIRST_KEYWORD, TOK_IDENT, TOK_LAST_KEYWORD, TOK_LET, TOK_VAR,
        TOK_YIELD,
    },
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
    /// Creates a sub function definition (as child of the current function def).
    /// Returns the newly created function definition.
    /// For JS_PARSE_FUNC_STATEMENT, JS_PARSE_FUNC_EXPR, JS_PARSE_FUNC_ARROW and JS_PARSE_FUNC_VAR,
    /// func_name must be JS_ATOM_NULL.
    ///
    /// Note: `func_name` is consumed inside.
    fn parse_function_sub(
        &mut self,
        func_type: JSParseFunctionEnum,
        func_kind: JSFunctionKindEnum,
        func_name: Atom,
        ptr: *const u8,
        function_line_num: i32,
        is_expr: bool,
    ) -> Result<Box<JSFunctionDef>, i32> {
        // save original cur_func
        let parent_fd = self.cur_func_def;

        let mut sub_fd = JSFunctionDef::new(
            &self.ctx,
            unsafe { parent_fd.as_mut() },
            false,
            is_expr,
            self.filename,
            function_line_num,
        )?;

        sub_fd.func_name = func_name;
        sub_fd.attrs.has_prototype = matches!(
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

        sub_fd.attrs.is_derived_class_constructor =
            func_type == JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR;

        /* fd->in_function_body == FALSE prevents yield/await during the parsing
        of the arguments in generator/async functions. They are parsed as
        regular identifiers for other function kinds. */
        sub_fd.attrs.func_kind = func_kind;

        match func_type {
            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW => {
                let parent_attrs = unsafe { &(&*sub_fd.parent).attrs };

                sub_fd.attrs.new_target_allowed = parent_attrs.new_target_allowed;
                sub_fd.attrs.super_call_allowed = parent_attrs.super_call_allowed;
                sub_fd.attrs.super_allowed = parent_attrs.super_allowed;
                sub_fd.attrs.arguments_allowed = parent_attrs.arguments_allowed;
            }
            JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT => {
                sub_fd.attrs.new_target_allowed = true; // although new.target === undefined
                sub_fd.attrs.super_call_allowed = false;
                sub_fd.attrs.super_allowed = true;
                sub_fd.attrs.arguments_allowed = false;
            }
            _ => {
                sub_fd.attrs.new_target_allowed = true;
                sub_fd.attrs.super_call_allowed = sub_fd.attrs.is_derived_class_constructor;
                sub_fd.attrs.super_allowed = sub_fd.has_home_object;
                sub_fd.attrs.arguments_allowed = true;
            }
        }

        sub_fd.func_type = func_type;

        // set sub_fd to be the cur_func
        let fd1 = sub_fd.deref_mut();
        self.cur_func_def = fd1;

        // IMPORTANT: must restore original cur_func on exit.
        let mut self_ptr = unsafe { std::ptr::NonNull::new_unchecked(self) };
        crate::defer!(unsafe {
            self_ptr.as_mut().cur_func_def = parent_fd;
        });

        //
        // generate opcodes for sub_fd
        //
        if matches!(
            func_type,
            JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                | JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
        ) {
            // error if not invoked as a constructor
            fd1.emit_op(OpCode::CHECK_CTOR, self.last_line_num);
        }

        if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR {
            fd1.emit_class_field_init(self.last_line_num);
        }

        // parse arguments
        fd1.attrs.has_simple_parameter_list = true;
        fd1.has_parameter_expressions = false;
        let mut has_opt_arg = false;

        if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_ARROW && self.token.val == TOK_IDENT {
            if self.token.data.is_reserved() {
                self.throw_reserved_identifier_error();
                return Err(-1);
            }

            fd1.add_arg(&self.ctx, self.token.atom().unwrap())?;
            fd1.attrs.defined_arg_count = 1;
        } else if func_type != JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT {
            if self.token.val == '(' as i32 {
                let mut skip_bits: i32 = 0;
                /* if there is an '=' inside the parameter list, we
                consider there is a parameter expression inside */
                self.parse_skip_parens_token(&mut skip_bits, false)?;
                if skip_bits & SKIP_HAS_ASSIGNMENT != 0 {
                    fd1.has_parameter_expressions = true;
                }
                self.next_token()?;
            } else {
                self.parse_expect('(' as i32)?;
            }

            if fd1.has_parameter_expressions {
                fd1.scope_level = -1; // force no parent scope
                self.push_scope()?;
            }

            while self.token.val != ')' as i32 {
                let rest = if self.token.val == TOK_ELLIPSIS {
                    fd1.attrs.has_simple_parameter_list = false;
                    self.next_token()?;
                    true
                } else {
                    false
                };

                if matches!(self.token.val as u8, b'[' | b'{') {
                    fd1.attrs.has_simple_parameter_list = false;
                    if rest {
                        fd1.emit_op(OpCode::REST, self.last_line_num);
                        fd1.emit_u16(fd1.args.len() as _);
                    } else {
                        // unnamed arg for destructuring
                        let idx = fd1.add_arg(&self.ctx, Atom::NULL)? as i32;
                        fd1.emit_op(OpCode::GET_ARG, self.last_line_num);
                        fd1.emit_u16(idx as _);
                    }

                    let has_initializer = self.parse_destructuring_element(
                        if fd1.has_parameter_expressions {
                            TOK_LET
                        } else {
                            TOK_VAR
                        },
                        true,
                        true,
                        -1,
                        true,
                        false,
                    )?;
                    if has_initializer {
                        has_opt_arg = true;
                    }
                    if !has_opt_arg {
                        fd1.attrs.defined_arg_count += 1;
                    }
                } else if self.token.val == TOK_IDENT {
                    if self.token.data.is_reserved() {
                        self.throw_reserved_identifier_error();
                        return Err(-1);
                    }

                    let name = self.token.atom().unwrap();

                    if name == Atom::CONST_yield
                        && fd1.attrs.func_kind == JSFunctionKindEnum::JS_FUNC_GENERATOR
                    {
                        self.throw_reserved_identifier_error();
                        return Err(-1);
                    }
                    if fd1.has_parameter_expressions {
                        // fd1 is the cur_func
                        self.define_var_cur_func(name, JSVarDefEnum::JS_VAR_DEF_LET)?;
                    }

                    /* XXX: could avoid allocating an argument if rest is true */
                    let idx = fd1.add_arg(&self.ctx, name)? as i32;
                    self.next_token()?;

                    if rest {
                        fd1.emit_op(OpCode::REST, self.last_line_num);
                        fd1.emit_u16(idx as _);

                        if fd1.has_parameter_expressions {
                            fd1.emit_op(OpCode::DUP, self.last_line_num);
                            fd1.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT, self.last_line_num);
                            fd1.emit_atom(name);
                            fd1.emit_u16(fd1.scope_level as _);
                        }
                        fd1.emit_op(OpCode::PUT_ARG, self.last_line_num);
                        fd1.emit_u16(idx as _);

                        fd1.attrs.has_simple_parameter_list = false;
                        has_opt_arg = true;
                    } else if self.token.val == CHAR_EQUAL {
                        fd1.attrs.has_simple_parameter_list = false;
                        has_opt_arg = true;

                        self.next_token()?;

                        let label = fd1.add_label()? as i32;

                        fd1.emit_op(OpCode::GET_ARG, self.last_line_num);
                        fd1.emit_u16(idx as _);
                        fd1.emit_op(OpCode::DUP, self.last_line_num);
                        fd1.emit_op(OpCode::UNDEFINED, self.last_line_num);
                        fd1.emit_op(OpCode::STRICT_EQ, self.last_line_num);
                        fd1.emit_goto(OpCode::IF_FALSE, label, self.last_line_num);
                        fd1.emit_op(OpCode::DROP, self.last_line_num);

                        self.parse_assign_expr()?;

                        self.set_object_name(name);

                        fd1.emit_op(OpCode::DUP, self.last_line_num);
                        fd1.emit_op(OpCode::PUT_ARG, self.last_line_num);
                        fd1.emit_u16(idx as _);

                        fd1.emit_label(label, self.last_line_num);
                        fd1.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT, self.last_line_num);
                        fd1.emit_atom(name);
                        fd1.emit_u16(fd1.scope_level as _);
                    } else {
                        if !has_opt_arg {
                            fd1.attrs.defined_arg_count += 1;
                        }
                        if fd1.has_parameter_expressions {
                            // copy the argument to the argument scope
                            fd1.emit_op(OpCode::GET_ARG, self.last_line_num);
                            fd1.emit_u16(idx as _);
                            fd1.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT, self.last_line_num);
                            fd1.emit_atom(name);
                            fd1.emit_u16(fd1.scope_level as _);
                        }
                    }
                } else {
                    self.throw_syntax_error("missing formal parameter");
                    return Err(-1);
                }

                if rest && self.token.val != CHAR_BRACKET_R {
                    let _ = self.parse_expect(CHAR_BRACKET_R);
                    return Err(-1);
                }

                if self.token.val == CHAR_BRACKET_R {
                    break;
                }
                self.parse_expect(CHAR_COMMA)?;
            }

            if (func_type == JSParseFunctionEnum::JS_PARSE_FUNC_GETTER && !fd1.args.is_empty())
                || (func_type == JSParseFunctionEnum::JS_PARSE_FUNC_SETTER && fd1.args.len() != 1)
            {
                self.throw_syntax_error("invalid number of arguments for getter or setter");
                return Err(-1);
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
                    fd1.add_var(&self.ctx, var_name)?;

                    let vname = fd1.vars.get(idx as usize).unwrap().var_name;

                    fd1.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR, self.last_line_num);
                    fd1.emit_atom(vname);
                    fd1.emit_u16(fd1.scope_level as _);

                    fd1.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR, self.last_line_num);
                    fd1.emit_atom(vname);
                    fd1.emit_u16(0);
                }

                idx = scope_next;
            }

            /* the argument scope has no parent, hence we don't use pop_scope(s) */
            fd1.emit_op_temp(OpCodeTemp::LEAVE_SCOPE, self.last_line_num);
            fd1.emit_u16(fd1.scope_level as _);

            /* set the variable scope as the current scope */
            fd1.scope_level = 0;
            fd1.scope_first = fd1.scopes[fd1.scope_level as usize].first;
        }

        self.next_token()?;

        /* generator function: yield after the parameters are evaluated */
        if matches!(
            func_kind,
            JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            fd1.emit_op(OpCode::INITIAL_YIELD, self.last_line_num);
        }

        // in generators, yield expression is forbidden during the parsing of the arguments
        fd1.in_function_body = true;
        // enter body scope
        fd1.body_scope = self.push_scope().unwrap() as i32;

        if self.token.val == TOK_ARROW {
            self.next_token()?;
            if self.token.val != CHAR_CURLY_L {
                self.parse_function_check_names(fd1, func_name)?;
                self.parse_assign_expr()?;

                fd1.emit_op(
                    if func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL {
                        OpCode::RETURN_ASYNC
                    } else {
                        OpCode::RETURN
                    },
                    self.last_line_num,
                );

                if !fd1.attrs.exec_mode.contains(ExecMode::STRIP) {
                    // save the function source code
                    // the end of the function source code is after the last
                    // token of the function source stored into s->last_ptr
                    debug_assert!(self.last_ptr >= ptr);
                    debug_assert!(fd1.source.is_none());

                    let src_len = unsafe { self.last_ptr.offset_from(ptr) as usize };
                    if src_len > 0 {
                        let mut src = Vec::<u8>::new();
                        if src.try_reserve_exact(src_len).is_err() {
                            self.ctx.throw_out_of_memory();
                            return Err(-1);
                        }
                        src.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, src_len) });
                        fd1.source
                            .replace(unsafe { String::from_utf8_unchecked(src) });
                    }
                }

                // function declareation done, returns new fd.
                self.cur_func_def = parent_fd; // restore cur_func to previoud fd
                return Ok(sub_fd);
            }
        }

        if func_type != JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT {
            self.parse_expect(CHAR_CURLY_L)?;
        }

        self.parse_directives()?;

        // in strict_mode, check function and argument names
        self.parse_function_check_names(fd1, func_name)?;

        while self.token.val != CHAR_CURLY_R {
            self.parse_source_element()?;
        }

        if !fd1.attrs.exec_mode.contains(ExecMode::STRIP) {
            // save the function source code
            debug_assert!(self.buf_ptr >= ptr);
            debug_assert!(fd1.source.is_none());

            let src_len = unsafe { self.buf_ptr.offset_from(ptr) as usize };
            if src_len > 0 {
                let mut src = Vec::<u8>::new();
                if src.try_reserve_exact(src_len).is_err() {
                    self.ctx.throw_out_of_memory();
                    return Err(-1);
                }
                src.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, src_len) });
                let s = unsafe { String::from_utf8_unchecked(src) };
                fd1.source.replace(s);
            }
        }

        // consume the '}'
        self.next_token()?;

        /* in case there is no return, add one */
        if js_is_live_code(self) {
            fd1.emit_return(false, self.last_line_num);
        }

        // restore cur_func: this is done in defer!()

        Ok(sub_fd)
    }

    /// Parse function declaration for function statements, expressions, arrow functions and var declarations.
    /// Returns new created FunctionDef raw pointer on success, or Err on failure. You don't need to free it manually.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn parse_function_decl_ex(
        &mut self,
        func_type: JSParseFunctionEnum,
        mut func_kind: JSFunctionKindEnum,
        ptr: *const u8,
        function_line_num: i32,
        export_flag: JSParseExportEnum,
        pfd: *mut *mut JSFunctionDef,
    ) -> Result<(), i32> {
        let is_expr = !matches!(
            func_type,
            JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT | JSParseFunctionEnum::JS_PARSE_FUNC_VAR
        );

        // save current function def
        let cur_fd = unsafe { self.cur_func_def.as_mut().unwrap() };

        let mut lexical_func_idx: i32 = -1;
        let mut create_func_var = false;
        let mut func_name = Atom::NULL;

        match func_type {
            JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT
            | JSParseFunctionEnum::JS_PARSE_FUNC_VAR
            | JSParseFunctionEnum::JS_PARSE_FUNC_EXPR => {
                if func_kind == JSFunctionKindEnum::JS_FUNC_NORMAL
                    && self.token_is_pseudo_keyword(Atom::CONST_async)
                    && self.peek_token(true) != b'\n' as i32
                {
                    self.next_token()?;
                    func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
                }

                self.next_token()?;

                if self.token.val == CHAR_STAR {
                    self.next_token()?;
                    func_kind = unsafe {
                        std::mem::transmute::<u8, JSFunctionKindEnum>(
                            func_kind as u8 | JSFunctionKindEnum::JS_FUNC_GENERATOR as u8,
                        )
                    }
                }

                if self.token.val == TOK_IDENT {
                    let atom = self.token.atom().unwrap();
                    if self.token.data.is_reserved()
                        || (atom == Atom::CONST_yield
                            && func_type == JSParseFunctionEnum::JS_PARSE_FUNC_EXPR
                            && func_kind as u8 & JSFunctionKindEnum::JS_FUNC_GENERATOR as u8 != 0)
                        || (atom == Atom::CONST_await
                            && ((func_type == JSParseFunctionEnum::JS_PARSE_FUNC_EXPR
                                && (func_kind as u8 & JSFunctionKindEnum::JS_FUNC_ASYNC as u8)
                                    != 0)
                                || func_type
                                    == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT))
                    {
                        self.throw_reserved_identifier_error();
                        return Err(-1);
                    }
                }

                if self.token.val == TOK_IDENT
                    || ((self.token.val == TOK_YIELD
                        && !cur_fd.attrs.exec_mode.contains(ExecMode::STRICT)
                        || (self.token.val == TOK_AWAIT && !self.is_module))
                        && func_type == JSParseFunctionEnum::JS_PARSE_FUNC_EXPR)
                {
                    func_name = self.token.atom().unwrap();
                    self.next_token()?;
                } else if unlikely(
                    func_type != JSParseFunctionEnum::JS_PARSE_FUNC_EXPR
                        && export_flag != JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT,
                ) {
                    self.throw_syntax_error("function name expected");
                    return Err(-1);
                }
            }
            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW => {}
            _ => {}
        }

        if cur_fd.is_eval
            && cur_fd.eval_type == EvalType::Module
            && matches!(
                func_type,
                JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT
                    | JSParseFunctionEnum::JS_PARSE_FUNC_VAR
            )
            && let Some(v) = cur_fd.find_global_var(func_name)
            && v.scope_level == cur_fd.scope_level
        {
            self.throw_syntax_error("invalid redefinition of global identifier in module code");
            return Err(-1);
        }

        if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_VAR {
            let func_idx = cur_fd.find_var(func_name).map_or(-1, |x| x as i32);

            if !cur_fd.attrs.exec_mode.contains(ExecMode::STRICT)
                && func_kind == JSFunctionKindEnum::JS_FUNC_NORMAL
                && cur_fd
                    .find_lexical_decl(func_name, cur_fd.scope_first, false)
                    .is_none()
                && !(func_idx >= 0 && func_idx & ARGUMENT_VAR_OFFSET != 0)
                && !(func_name == Atom::CONST_arguments && cur_fd.has_arguments_binding)
            {
                create_func_var = true;
            }

            // Create the lexical name here so that the function closure contains it
            if cur_fd.is_eval
                && matches!(cur_fd.eval_type, EvalType::Global | EvalType::Module)
                && cur_fd.scope_level == cur_fd.body_scope
            {
                // avoid creating a lexical variable in the global scope. XXX: check annex B
                if let Some(hf) = cur_fd.find_global_var(func_name) {
                    /* XXX: should check scope chain */
                    if hf.scope_level == cur_fd.scope_level {
                        self.throw_syntax_error("invalid redefinition of global identifier");
                        return Err(-1);
                    }
                }
            } else {
                /* Always create a lexical name, fail if at the same scope as existing name */
                /* Lexical variable will be initialized upon entering scope */
                // now fd0 is still the cur_func
                debug_assert_eq!(cur_fd as *mut JSFunctionDef, self.cur_func_def);
                lexical_func_idx = self.define_var_cur_func(
                    func_name,
                    if func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL {
                        JSVarDefEnum::JS_VAR_DEF_NEW_FUNCTION_DECL
                    } else {
                        JSVarDefEnum::JS_VAR_DEF_FUNCTION_DECL
                    },
                )? as i32;
            }
        }

        // parse sub FunctionDef
        let mut sub_fd = self.parse_function_sub(
            func_type,
            func_kind,
            func_name,
            ptr,
            function_line_num,
            is_expr,
        )?;

        debug_assert_eq!(sub_fd.parent, cur_fd as *mut JSFunctionDef);
        debug_assert_eq!(
            self.cur_func_mut() as *mut JSFunctionDef,
            cur_fd as *mut JSFunctionDef
        );

        // reparse_ident_token:
        // if the current token is an identifier or keyword, reparse it
        // according to the current function type
        if matches!(
            self.token.val,
            TOK_IDENT | TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD
        ) {
            self.update_token_ident();
        }

        // create function object from fd1
        {
            // reload the real sub func name
            let sub_func_name = sub_fd.func_name;

            // the real object will be set at the end of the compilation
            let cp_idx = self
                .cur_func_mut()
                .cpool_add(JSValue::NULL)
                .map_or(-1, |x| x as i32);

            sub_fd.parent_cpool_idx = cp_idx;

            if is_expr {
                // for constructors, no code needs to be generated here
                if !matches!(
                    func_type,
                    JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                        | JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
                ) {
                    // OP_fclosure creates the function object from the bytecode and adds the scope information
                    self.emit_op(OpCode::FCLOSURE);
                    self.emit_u32(cp_idx as _);

                    if sub_func_name.is_null() {
                        self.emit_op(OpCode::SET_NAME);
                        self.emit_atom(Atom::NULL);
                    }
                }
            } else if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_VAR {
                self.emit_op(OpCode::FCLOSURE);
                self.emit_u32(cp_idx as _);

                if create_func_var {
                    if self.cur_func().is_global_var {
                        // the global variable must be defined at the start of the function
                        let js_mode = self.cur_func().attrs.exec_mode;

                        let hf = self
                            .cur_func_mut()
                            .add_global_var(&self.ctx, sub_func_name)?;

                        /* it is considered as defined at the top level
                        (needed for annex B.3.3.4 and B.3.3.5
                        checks) */
                        hf.scope_level = 0;
                        hf.set_force_init(js_mode.contains(ExecMode::STRICT));

                        /* store directly into global var, bypass lexical scope */
                        self.emit_op(OpCode::DUP);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR);
                        self.emit_atom(sub_func_name);
                        self.emit_u16(0);
                    } else {
                        // do not call define_var to bypass lexical scope check
                        if self.cur_func().find_var(sub_func_name).is_none() {
                            self.cur_func_mut().add_var(&self.ctx, sub_func_name)?;
                        };

                        /* store directly into local var, bypass lexical catch scope */
                        self.emit_op(OpCode::DUP);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR);
                        self.emit_atom(sub_func_name);
                        self.emit_u16(0);
                    }
                }

                if lexical_func_idx >= 0 {
                    /* lexical variable will be initialized upon entering scope */
                    self.cur_func_mut()
                        .vars
                        .get_mut(lexical_func_idx as usize)
                        .unwrap()
                        .func_pool_idx = cp_idx;
                    self.emit_op(OpCode::DROP);
                } else {
                    /* store function object into its lexical name */
                    /* XXX: could use OP_put_loc directly */
                    self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                    self.emit_atom(sub_func_name);
                    self.emit_u16(self.cur_func().scope_level as _);
                }
            } else if !self.cur_func().is_global_var {
                let var_idx =
                    self.define_var_cur_func(sub_func_name, JSVarDefEnum::JS_VAR_DEF_VAR)? as i32;

                // the variable will be assigned at the top of the function
                if var_idx & ARGUMENT_VAR_OFFSET != 0 {
                    self.cur_func_mut().args[(var_idx - ARGUMENT_VAR_OFFSET) as usize]
                        .func_pool_idx = cp_idx;
                } else {
                    self.cur_func_mut().vars[var_idx as usize].func_pool_idx = cp_idx;
                }
            } else {
                let func_var_name = if sub_func_name.is_null() {
                    Atom::CONST__default_ // export default
                } else {
                    sub_func_name
                };

                // the variable will be assigned at the top of the function
                self.cur_func_mut()
                    .add_global_var(&self.ctx, func_var_name)?
                    .cpool_idx = cp_idx;

                if export_flag != JSParseExportEnum::JS_PARSE_EXPORT_NONE {
                    self.add_module_export(
                        None,
                        func_var_name,
                        if export_flag == JSParseExportEnum::JS_PARSE_EXPORT_NAMED {
                            func_var_name
                        } else {
                            Atom::CONST_default
                        },
                        ExportType::JS_EXPORT_TYPE_LOCAL {
                            var_idx: u16::MAX,
                            var_ref: None,
                        },
                    )?;
                }
            }
        }

        // fd1 raw pointer was added to children list of fd0,
        // rust should give up its ownership.
        let fd_ptr = Box::into_raw(sub_fd);
        if !pfd.is_null() {
            unsafe {
                *pfd = fd_ptr;
            }
        }

        Ok(())
    }

    /// start parse function defination from given source.
    #[inline(always)]
    pub(super) fn parse_function_decl(
        &mut self,
        func_type: JSParseFunctionEnum,
        func_kind: JSFunctionKindEnum,
        ptr: *const u8,
        function_line_num: i32,
    ) -> Result<(), i32> {
        self.parse_function_decl_ex(
            func_type,
            func_kind,
            ptr,
            function_line_num,
            JSParseExportEnum::JS_PARSE_EXPORT_NONE,
            std::ptr::null_mut(),
        )
    }

    fn parse_function_check_names(&self, fd: &JSFunctionDef, func_name: Atom) -> Result<(), i32> {
        if fd.attrs.exec_mode.contains(ExecMode::STRICT) {
            if !fd.attrs.has_simple_parameter_list && fd.has_use_strict {
                self.throw_syntax_error(
                    "'use strict' not allowed in function with default or destructuring parameter",
                );
                return Err(-1);
            } else if matches!(func_name, Atom::CONST_eval | Atom::CONST_arguments) {
                self.throw_syntax_error("invalid function name in strict code");
                return Err(-1);
            } else if fd
                .args
                .iter()
                .any(|vd| matches!(vd.var_name, Atom::CONST_eval | Atom::CONST_arguments))
            {
                self.throw_syntax_error("invalid argument name in strict code");
                return Err(-1);
            }
        }

        // check async_generator case
        if fd.attrs.exec_mode.contains(ExecMode::STRICT)
            || !fd.attrs.has_simple_parameter_list
            || (fd.func_type == JSParseFunctionEnum::JS_PARSE_FUNC_METHOD
                && fd.attrs.func_kind == JSFunctionKindEnum::JS_FUNC_ASYNC)
            || matches!(
                fd.func_type,
                JSParseFunctionEnum::JS_PARSE_FUNC_ARROW
                    | JSParseFunctionEnum::JS_PARSE_FUNC_METHOD
            )
        {
            for (idx, name) in fd.args.iter().enumerate().filter_map(|(i, vd)| {
                if !vd.var_name.is_null() {
                    Some((i, vd.var_name))
                } else {
                    None
                }
            }) {
                if fd.args.iter().take(idx).any(|d| d.var_name.eq(&name)) {
                    self.throw_syntax_error("duplicate argument names not allowed in this context");
                    return Err(-1);
                }

                // Check if argument name duplicates a destructuring parameter
                // XXX: should have a flag for such variables
                if fd
                    .vars
                    .iter()
                    .any(|d| d.var_name.eq(&name) && d.scope_level == 0)
                {
                    self.throw_syntax_error("duplicate argument names not allowed in this context");
                    return Err(-1);
                }
            }
        }

        Ok(())
    }
}
