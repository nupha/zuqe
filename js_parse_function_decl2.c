
/* func_name must be JS_ATOM_NULL for JS_PARSE_FUNC_STATEMENT and
   JS_PARSE_FUNC_EXPR, JS_PARSE_FUNC_ARROW and JS_PARSE_FUNC_VAR */
static __exception int js_parse_function_decl2(JSParseState *s,
                                               JSParseFunctionEnum func_type,
                                               JSFunctionKindEnum func_kind,
                                               JSAtom func_name,
                                               const uint8_t *ptr,
                                               int function_line_num,
                                               JSParseExportEnum export_flag,
                                               JSFunctionDef **pfd)
{
    JSContext *ctx = s->ctx;
    JSFunctionDef *fd = s->cur_func;
    BOOL is_expr;
    int func_idx, lexical_func_idx = -1;
    BOOL has_opt_arg;
    BOOL create_func_var = FALSE;

    is_expr = (func_type != JS_PARSE_FUNC_STATEMENT &&
               func_type != JS_PARSE_FUNC_VAR);

    if (func_type == JS_PARSE_FUNC_STATEMENT ||
        func_type == JS_PARSE_FUNC_VAR ||
        func_type == JS_PARSE_FUNC_EXPR) {
        if (func_kind == JS_FUNC_NORMAL &&
            token_is_pseudo_keyword(s, JS_ATOM_async) &&
            peek_token(s, TRUE) != '\n') {
            if (next_token(s))
                return -1;
            func_kind = JS_FUNC_ASYNC;
        }
        if (next_token(s))
            return -1;
        if (s->token.val == '*') {
            if (next_token(s))
                return -1;
            func_kind |= JS_FUNC_GENERATOR;
        }

        if (s->token.val == TOK_IDENT) {
            if (s->token.u.ident.is_reserved ||
                (s->token.u.ident.atom == JS_ATOM_yield &&
                 func_type == JS_PARSE_FUNC_EXPR &&
                 (func_kind & JS_FUNC_GENERATOR)) ||
                (s->token.u.ident.atom == JS_ATOM_await &&
                 ((func_type == JS_PARSE_FUNC_EXPR &&
                   (func_kind & JS_FUNC_ASYNC)) ||
                  func_type == JS_PARSE_FUNC_CLASS_STATIC_INIT))) {
                return js_parse_error_reserved_identifier(s);
            }
        }
        if (s->token.val == TOK_IDENT ||
            (((s->token.val == TOK_YIELD && !(fd->js_mode & JS_MODE_STRICT)) ||
             (s->token.val == TOK_AWAIT && !s->is_module)) &&
             func_type == JS_PARSE_FUNC_EXPR)) {
            func_name = JS_DupAtom(ctx, s->token.u.ident.atom);
            if (next_token(s)) {
                JS_FreeAtom(ctx, func_name);
                return -1;
            }
        } else {
            if (func_type != JS_PARSE_FUNC_EXPR &&
                export_flag != JS_PARSE_EXPORT_DEFAULT) {
                return js_parse_error(s, "function name expected");
            }
        }
    } else if (func_type != JS_PARSE_FUNC_ARROW) {
        func_name = JS_DupAtom(ctx, func_name);
    }

    if (fd->is_eval && fd->eval_type == JS_EVAL_TYPE_MODULE &&
        (func_type == JS_PARSE_FUNC_STATEMENT || func_type == JS_PARSE_FUNC_VAR)) {
        JSGlobalVar *hf;
        hf = find_global_var(fd, func_name);
        /* XXX: should check scope chain */
        if (hf && hf->scope_level == fd->scope_level) {
            js_parse_error(s, "invalid redefinition of global identifier in module code");
            JS_FreeAtom(ctx, func_name);
            return -1;
        }
    }

    if (func_type == JS_PARSE_FUNC_VAR) {
        if (!(fd->js_mode & JS_MODE_STRICT)
        && func_kind == JS_FUNC_NORMAL
        &&  find_lexical_decl(ctx, fd, func_name, fd->scope_first, FALSE) < 0
        &&  !((func_idx = find_var(ctx, fd, func_name)) >= 0 && (func_idx & ARGUMENT_VAR_OFFSET))
        &&  !(func_name == JS_ATOM_arguments && fd->has_arguments_binding)) {
            create_func_var = TRUE;
        }
        /* Create the lexical name here so that the function closure
           contains it */
        if (fd->is_eval &&
            (fd->eval_type == JS_EVAL_TYPE_GLOBAL ||
             fd->eval_type == JS_EVAL_TYPE_MODULE) &&
            fd->scope_level == fd->body_scope) {
            /* avoid creating a lexical variable in the global
               scope. XXX: check annex B */
            JSGlobalVar *hf;
            hf = find_global_var(fd, func_name);
            /* XXX: should check scope chain */
            if (hf && hf->scope_level == fd->scope_level) {
                js_parse_error(s, "invalid redefinition of global identifier");
                JS_FreeAtom(ctx, func_name);
                return -1;
            }
        } else {
            /* Always create a lexical name, fail if at the same scope as
               existing name */
            /* Lexical variable will be initialized upon entering scope */
            lexical_func_idx = define_var(s, fd, func_name,
                                          func_kind != JS_FUNC_NORMAL ?
                                          JS_VAR_DEF_NEW_FUNCTION_DECL :
                                          JS_VAR_DEF_FUNCTION_DECL);
            if (lexical_func_idx < 0) {
                JS_FreeAtom(ctx, func_name);
                return -1;
            }
        }
    }

    fd = js_new_function_def(ctx, fd, FALSE, is_expr,
                             s->filename, function_line_num);
    if (!fd) {
        JS_FreeAtom(ctx, func_name);
        return -1;
    }
    if (pfd)
        *pfd = fd;
    s->cur_func = fd;
    fd->func_name = func_name;
    /* XXX: test !fd->is_generator is always false */
    fd->has_prototype = (func_type == JS_PARSE_FUNC_STATEMENT ||
                         func_type == JS_PARSE_FUNC_VAR ||
                         func_type == JS_PARSE_FUNC_EXPR) &&
                        func_kind == JS_FUNC_NORMAL;
    fd->has_home_object = (func_type == JS_PARSE_FUNC_METHOD ||
                           func_type == JS_PARSE_FUNC_GETTER ||
                           func_type == JS_PARSE_FUNC_SETTER ||
                           func_type == JS_PARSE_FUNC_CLASS_CONSTRUCTOR ||
                           func_type == JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR);
    fd->has_arguments_binding = (func_type != JS_PARSE_FUNC_ARROW &&
                                 func_type != JS_PARSE_FUNC_CLASS_STATIC_INIT);
    fd->has_this_binding = fd->has_arguments_binding;
    fd->is_derived_class_constructor = (func_type == JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR);
    if (func_type == JS_PARSE_FUNC_ARROW) {
        fd->new_target_allowed = fd->parent->new_target_allowed;
        fd->super_call_allowed = fd->parent->super_call_allowed;
        fd->super_allowed = fd->parent->super_allowed;
        fd->arguments_allowed = fd->parent->arguments_allowed;
    } else if (func_type == JS_PARSE_FUNC_CLASS_STATIC_INIT) {
        fd->new_target_allowed = TRUE; // although new.target === undefined
        fd->super_call_allowed = FALSE;
        fd->super_allowed = TRUE;
        fd->arguments_allowed = FALSE;
    } else {
        fd->new_target_allowed = TRUE;
        fd->super_call_allowed = fd->is_derived_class_constructor;
        fd->super_allowed = fd->has_home_object;
        fd->arguments_allowed = TRUE;
    }

    /* fd->in_function_body == FALSE prevents yield/await during the parsing
       of the arguments in generator/async functions. They are parsed as
       regular identifiers for other function kinds. */
    fd->func_kind = func_kind;
    fd->func_type = func_type;

    if (func_type == JS_PARSE_FUNC_CLASS_CONSTRUCTOR ||
        func_type == JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR) {
        /* error if not invoked as a constructor */
        emit_op(s, OP_check_ctor);
    }

    if (func_type == JS_PARSE_FUNC_CLASS_CONSTRUCTOR) {
        emit_class_field_init(s);
    }
    
    /* parse arguments */
    fd->has_simple_parameter_list = TRUE;
    fd->has_parameter_expressions = FALSE;
    has_opt_arg = FALSE;
    if (func_type == JS_PARSE_FUNC_ARROW && s->token.val == TOK_IDENT) {
        JSAtom name;
        if (s->token.u.ident.is_reserved) {
            js_parse_error_reserved_identifier(s);
            goto fail;
        }
        name = s->token.u.ident.atom;
        if (add_arg(ctx, fd, name) < 0)
            goto fail;
        fd->defined_arg_count = 1;
    } else if (func_type != JS_PARSE_FUNC_CLASS_STATIC_INIT) {
        if (s->token.val == '(') {
            int skip_bits;
            /* if there is an '=' inside the parameter list, we
               consider there is a parameter expression inside */
            js_parse_skip_parens_token(s, &skip_bits, FALSE);
            if (skip_bits & SKIP_HAS_ASSIGNMENT)
                fd->has_parameter_expressions = TRUE;
            if (next_token(s))
                goto fail;
        } else {
            if (js_parse_expect(s, '('))
                goto fail;
        }

        if (fd->has_parameter_expressions) {
            fd->scope_level = -1; /* force no parent scope */
            if (push_scope(s) < 0)
                return -1;
        }
        
        while (s->token.val != ')') {
            JSAtom name;
            BOOL rest = FALSE;
            int idx, has_initializer;

            if (s->token.val == TOK_ELLIPSIS) {
                fd->has_simple_parameter_list = FALSE;
                rest = TRUE;
                if (next_token(s))
                    goto fail;
            }
            if (s->token.val == '[' || s->token.val == '{') {
                fd->has_simple_parameter_list = FALSE;
                if (rest) {
                    emit_op(s, OP_rest);
                    emit_u16(s, fd->arg_count);
                } else {
                    /* unnamed arg for destructuring */
                    idx = add_arg(ctx, fd, JS_ATOM_NULL);
                    emit_op(s, OP_get_arg);
                    emit_u16(s, idx);
                }
                has_initializer = js_parse_destructuring_element(s, fd->has_parameter_expressions ? TOK_LET : TOK_VAR, 1, TRUE, -1, TRUE);
                if (has_initializer < 0)
                    goto fail;
                if (has_initializer)
                    has_opt_arg = TRUE;
                if (!has_opt_arg)
                    fd->defined_arg_count++;
            } else if (s->token.val == TOK_IDENT) {
                if (s->token.u.ident.is_reserved) {
                    js_parse_error_reserved_identifier(s);
                    goto fail;
                }
                name = s->token.u.ident.atom;
                if (name == JS_ATOM_yield && fd->func_kind == JS_FUNC_GENERATOR) {
                    js_parse_error_reserved_identifier(s);
                    goto fail;
                }
                if (fd->has_parameter_expressions) {
                    if (define_var(s, fd, name, JS_VAR_DEF_LET) < 0)
                        goto fail;
                }
                /* XXX: could avoid allocating an argument if rest is true */
                idx = add_arg(ctx, fd, name);
                if (idx < 0)
                    goto fail;
                if (next_token(s))
                    goto fail;
                if (rest) {
                    emit_op(s, OP_rest);
                    emit_u16(s, idx);
                    if (fd->has_parameter_expressions) {
                        emit_op(s, OP_dup);
                        emit_op(s, OP_scope_put_var_init);
                        emit_atom(s, name);
                        emit_u16(s, fd->scope_level);
                    }
                    emit_op(s, OP_put_arg);
                    emit_u16(s, idx);
                    fd->has_simple_parameter_list = FALSE;
                    has_opt_arg = TRUE;
                } else if (s->token.val == '=') {
                    int label;
                    
                    fd->has_simple_parameter_list = FALSE;
                    has_opt_arg = TRUE;

                    if (next_token(s))
                        goto fail;

                    label = new_label(s);
                    emit_op(s, OP_get_arg);
                    emit_u16(s, idx);
                    emit_op(s, OP_dup);
                    emit_op(s, OP_undefined);
                    emit_op(s, OP_strict_eq);
                    emit_goto(s, OP_if_false, label);
                    emit_op(s, OP_drop);
                    if (js_parse_assign_expr(s))
                        goto fail;
                    set_object_name(s, name);
                    emit_op(s, OP_dup);
                    emit_op(s, OP_put_arg);
                    emit_u16(s, idx);
                    emit_label(s, label);
                    emit_op(s, OP_scope_put_var_init);
                    emit_atom(s, name);
                    emit_u16(s, fd->scope_level);
                } else {
                    if (!has_opt_arg) {
                        fd->defined_arg_count++;
                    }
                    if (fd->has_parameter_expressions) {
                        /* copy the argument to the argument scope */
                        emit_op(s, OP_get_arg);
                        emit_u16(s, idx);
                        emit_op(s, OP_scope_put_var_init);
                        emit_atom(s, name);
                        emit_u16(s, fd->scope_level);
                    }
                }
            } else {
                js_parse_error(s, "missing formal parameter");
                goto fail;
            }
            if (rest && s->token.val != ')') {
                js_parse_expect(s, ')');
                goto fail;
            }
            if (s->token.val == ')')
                break;
            if (js_parse_expect(s, ','))
                goto fail;
        }
        if ((func_type == JS_PARSE_FUNC_GETTER && fd->arg_count != 0) ||
            (func_type == JS_PARSE_FUNC_SETTER && fd->arg_count != 1)) {
            js_parse_error(s, "invalid number of arguments for getter or setter");
            goto fail;
        }
    }

    if (fd->has_parameter_expressions) {
        int idx;

        /* Copy the variables in the argument scope to the variable
           scope (see FunctionDeclarationInstantiation() in spec). The
           normal arguments are already present, so no need to copy
           them. */
        idx = fd->scopes[fd->scope_level].first;
        while (idx >= 0) {
            JSVarDef *vd = &fd->vars[idx];
            if (vd->scope_level != fd->scope_level)
                break;
            if (find_var(ctx, fd, vd->var_name) < 0) {
                if (add_var(ctx, fd, vd->var_name) < 0)
                    goto fail;
                vd = &fd->vars[idx]; /* fd->vars may have been reallocated */
                emit_op(s, OP_scope_get_var);
                emit_atom(s, vd->var_name);
                emit_u16(s, fd->scope_level);
                emit_op(s, OP_scope_put_var);
                emit_atom(s, vd->var_name);
                emit_u16(s, 0);
            }
            idx = vd->scope_next;
        }
        
        /* the argument scope has no parent, hence we don't use pop_scope(s) */
        emit_op(s, OP_leave_scope);
        emit_u16(s, fd->scope_level);

        /* set the variable scope as the current scope */
        fd->scope_level = 0;
        fd->scope_first = fd->scopes[fd->scope_level].first;
    }
    
    if (next_token(s))
        goto fail;

    /* generator function: yield after the parameters are evaluated */
    if (func_kind == JS_FUNC_GENERATOR ||
        func_kind == JS_FUNC_ASYNC_GENERATOR)
        emit_op(s, OP_initial_yield);

    /* in generators, yield expression is forbidden during the parsing
       of the arguments */
    fd->in_function_body = TRUE;
    push_scope(s);  /* enter body scope */
    fd->body_scope = fd->scope_level;

    if (s->token.val == TOK_ARROW) {
        if (next_token(s))
            goto fail;

        if (s->token.val != '{') {
            if (js_parse_function_check_names(s, fd, func_name))
                goto fail;

            if (js_parse_assign_expr(s))
                goto fail;

            if (func_kind != JS_FUNC_NORMAL)
                emit_op(s, OP_return_async);
            else
                emit_op(s, OP_return);

            if (!(fd->js_mode & JS_MODE_STRIP)) {
                /* save the function source code */
                /* the end of the function source code is after the last
                   token of the function source stored into s->last_ptr */
                fd->source_len = s->last_ptr - ptr;
                fd->source = js_strndup(ctx, (const char *)ptr, fd->source_len);
                if (!fd->source)
                    goto fail;
            }
            goto done;
        }
    }

    if (func_type != JS_PARSE_FUNC_CLASS_STATIC_INIT) {
        if (js_parse_expect(s, '{'))
            goto fail;
    }

    if (js_parse_directives(s))
        goto fail;

    /* in strict_mode, check function and argument names */
    if (js_parse_function_check_names(s, fd, func_name))
        goto fail;

    while (s->token.val != '}') {
        if (js_parse_source_element(s))
            goto fail;
    }
    if (!(fd->js_mode & JS_MODE_STRIP)) {
        /* save the function source code */
        fd->source_len = s->buf_ptr - ptr;
        fd->source = js_strndup(ctx, (const char *)ptr, fd->source_len);
        if (!fd->source)
            goto fail;
    }

    if (next_token(s)) {
        /* consume the '}' */
        goto fail;
    }

    /* in case there is no return, add one */
    if (js_is_live_code(s)) {
        emit_return(s, FALSE);
    }
 done:
    s->cur_func = fd->parent;

    /* Reparse identifiers after the function is terminated so that
       the token is parsed in the englobing function. It could be done
       by just using next_token() here for normal functions, but it is
       necessary for arrow functions with an expression body. */
    reparse_ident_token(s);
    
    /* create the function object */
    {
        int idx;
        JSAtom func_name = fd->func_name;

        /* the real object will be set at the end of the compilation */
        idx = cpool_add(s, JS_NULL);
        fd->parent_cpool_idx = idx;

        if (is_expr) {
            /* for constructors, no code needs to be generated here */
            if (func_type != JS_PARSE_FUNC_CLASS_CONSTRUCTOR &&
                func_type != JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR) {
                /* OP_fclosure creates the function object from the bytecode
                   and adds the scope information */
                emit_op(s, OP_fclosure);
                emit_u32(s, idx);
                if (func_name == JS_ATOM_NULL) {
                    emit_op(s, OP_set_name);
                    emit_u32(s, JS_ATOM_NULL);
                }
            }
        } else if (func_type == JS_PARSE_FUNC_VAR) {
            emit_op(s, OP_fclosure);
            emit_u32(s, idx);
            if (create_func_var) {
                if (s->cur_func->is_global_var) {
                    JSGlobalVar *hf;
                    /* the global variable must be defined at the start of the
                       function */
                    hf = add_global_var(ctx, s->cur_func, func_name);
                    if (!hf)
                        goto fail;
                    /* it is considered as defined at the top level
                       (needed for annex B.3.3.4 and B.3.3.5
                       checks) */
                    hf->scope_level = 0; 
                    hf->force_init = ((s->cur_func->js_mode & JS_MODE_STRICT) != 0);
                    /* store directly into global var, bypass lexical scope */
                    emit_op(s, OP_dup);
                    emit_op(s, OP_scope_put_var);
                    emit_atom(s, func_name);
                    emit_u16(s, 0);
                } else {
                    /* do not call define_var to bypass lexical scope check */
                    func_idx = find_var(ctx, s->cur_func, func_name);
                    if (func_idx < 0) {
                        func_idx = add_var(ctx, s->cur_func, func_name);
                        if (func_idx < 0)
                            goto fail;
                    }
                    /* store directly into local var, bypass lexical catch scope */
                    emit_op(s, OP_dup);
                    emit_op(s, OP_scope_put_var);
                    emit_atom(s, func_name);
                    emit_u16(s, 0);
                }
            }
            if (lexical_func_idx >= 0) {
                /* lexical variable will be initialized upon entering scope */
                s->cur_func->vars[lexical_func_idx].func_pool_idx = idx;
                emit_op(s, OP_drop);
            } else {
                /* store function object into its lexical name */
                /* XXX: could use OP_put_loc directly */
                emit_op(s, OP_scope_put_var_init);
                emit_atom(s, func_name);
                emit_u16(s, s->cur_func->scope_level);
            }
        } else {
            if (!s->cur_func->is_global_var) {
                int var_idx = define_var(s, s->cur_func, func_name, JS_VAR_DEF_VAR);

                if (var_idx < 0)
                    goto fail;
                /* the variable will be assigned at the top of the function */
                if (var_idx & ARGUMENT_VAR_OFFSET) {
                    s->cur_func->args[var_idx - ARGUMENT_VAR_OFFSET].func_pool_idx = idx;
                } else {
                    s->cur_func->vars[var_idx].func_pool_idx = idx;
                }
            } else {
                JSAtom func_var_name;
                JSGlobalVar *hf;
                if (func_name == JS_ATOM_NULL)
                    func_var_name = JS_ATOM__default_; /* export default */
                else
                    func_var_name = func_name;
                /* the variable will be assigned at the top of the function */
                hf = add_global_var(ctx, s->cur_func, func_var_name);
                if (!hf)
                    goto fail;
                hf->cpool_idx = idx;
                if (export_flag != JS_PARSE_EXPORT_NONE) {
                    if (!add_export_entry(s, s->cur_func->module, func_var_name,
                                          export_flag == JS_PARSE_EXPORT_NAMED ? func_var_name : JS_ATOM_default, JS_EXPORT_TYPE_LOCAL))
                        goto fail;
                }
            }
        }
    }
    return 0;
 fail:
    s->cur_func = fd->parent;
    js_free_function_def(ctx, fd);
    if (pfd)
        *pfd = NULL;
    return -1;
}
