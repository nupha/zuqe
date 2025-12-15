use crate::{
    atom::{Atom, js_atom_concat_num, js_atom_concat_str},
    bytecode::{OpCode, OpCodeTemp},
    class::{
        ClassFieldsDef, JS_DEFINE_CLASS_HAS_HERITAGE, OP_DEFINE_METHOD_GETTER,
        OP_DEFINE_METHOD_METHOD, OP_DEFINE_METHOD_SETTER,
    },
    defer,
    function::{JSFunctionDef, JSFunctionKindEnum, JSVarDefEnum, JSVarKindEnum},
    module::ExportType,
    object::{
        PROP_TYPE_ASYNC, PROP_TYPE_ASYNC_STAR, PROP_TYPE_GET, PROP_TYPE_IDENT, PROP_TYPE_PRIVATE,
        PROP_TYPE_SET, PROP_TYPE_STAR,
    },
    parse::obj::set_object_name_computed,
    stack_frame::ExecMode,
    utils::write_to_ptr,
};

use super::{
    esm::JSParseExportEnum,
    function::JSParseFunctionEnum,
    state::JSParseState,
    token::{TOK_EXTENDS, TOK_IDENT, TOK_STATIC},
};

impl<'s> JSParseState<'s> {
    /// TODO: return Result<Option<Atom>,i32>
    fn parse_class_name(
        &mut self,
        is_class_expr: bool,
        export_flag: JSParseExportEnum,
    ) -> Result<Atom, i32> {
        if self.token.val == TOK_IDENT {
            if self.token.data.is_reserved() {
                self.throw_reserved_identifier_error();
                return Err(-1);
            }

            // Note: calling `next_token()` will free the current atom, so we must dup atom in advance.
            let name = self.token.atom().unwrap();
            self.next_token()?;
            Ok(name)
        } else if !is_class_expr && export_flag != JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT {
            self.throw_syntax_error("class statement requires a name");
            Err(-1)
        } else {
            Ok(Atom::NULL)
        }
    }

    /// parse class field defination
    fn parse_class_field(&mut self, cfields: &mut ClassFieldsDef) -> Result<(), i32> {
        let mut init = std::ptr::null_mut::<JSFunctionDef>();

        if cfields.fields_init_fd.is_null() {
            self.emit_class_init_start(cfields)?;
        }

        self.cur_func_def = cfields.fields_init_fd;

        // XXX: could try to avoid creating a new function and
        // reuse 'fields_init_fd' with a specific 'var' scope
        // stack is now: <empty>
        self.parse_function_decl_ex(
            JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT,
            JSFunctionKindEnum::JS_FUNC_NORMAL,
            self.token.ptr,
            self.token.line_num,
            JSParseExportEnum::JS_PARSE_EXPORT_NONE,
            std::ptr::addr_of_mut!(init),
        )?;

        // stack is now: fclosure
        self.push_scope()?;
        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
        self.emit_atom(Atom::CONST_this);
        self.emit_u16(0);
        // stack is now: fclosure this
        self.emit_op(OpCode::SWAP);
        // stack is now: this fclosure
        self.emit_op(OpCode::CALL_METHOD);
        self.emit_u16(0);
        // stack is now: returnvalue
        self.emit_op(OpCode::DROP);
        // stack is now: <empty>
        self.pop_scope();
        self.cur_func_def = self.cur_func().parent;

        Ok(())
    }

    fn parse_class_getset(
        &mut self,
        is_set: bool,
        is_private: bool,
        is_static: bool,
        name: Atom,
        cf: &mut ClassFieldsDef,
        start_ptr: *const u8,
    ) -> Result<(), i32> {
        if is_private {
            let fd = unsafe { &mut *self.cur_func_def };

            let idx = find_private_class_field(fd, name, fd.scope_level);
            if idx >= 0 {
                let vd = fd.vars.get_mut(idx as usize).unwrap();
                let var_kind = vd.var_kind();

                if matches!(
                    var_kind,
                    JSVarKindEnum::JS_VAR_PRIVATE_FIELD
                        | JSVarKindEnum::JS_VAR_PRIVATE_METHOD
                        | JSVarKindEnum::JS_VAR_PRIVATE_GETTER_SETTER
                ) || var_kind
                    == if is_set {
                        JSVarKindEnum::JS_VAR_PRIVATE_SETTER
                    } else {
                        JSVarKindEnum::JS_VAR_PRIVATE_GETTER
                    }
                    || (var_kind
                        == if is_set {
                            JSVarKindEnum::JS_VAR_PRIVATE_GETTER
                        } else {
                            JSVarKindEnum::JS_VAR_PRIVATE_SETTER
                        }
                        && is_static != vd.is_static_private())
                {
                    self.throw_syntax_error("private class field is already defined");
                    return Err(-1);
                }

                vd.set_var_kind(JSVarKindEnum::JS_VAR_PRIVATE_GETTER_SETTER);
            } else if add_private_class_field(
                self,
                fd,
                name,
                if is_set {
                    JSVarKindEnum::JS_VAR_PRIVATE_SETTER
                } else {
                    JSVarKindEnum::JS_VAR_PRIVATE_GETTER
                },
                is_static,
            ) < 0
            {
                return Err(-1);
            }

            cf.need_brand = true;
        }

        // parse getter or setter function
        let mut method_fd = std::ptr::null_mut::<JSFunctionDef>();
        self.parse_function_decl_ex(
            if is_set {
                JSParseFunctionEnum::JS_PARSE_FUNC_SETTER
            } else {
                JSParseFunctionEnum::JS_PARSE_FUNC_GETTER
            },
            JSFunctionKindEnum::JS_FUNC_NORMAL,
            start_ptr,
            self.token.line_num,
            JSParseExportEnum::JS_PARSE_EXPORT_NONE,
            &mut method_fd,
        )?;
        debug_assert!(!method_fd.is_null());

        if is_private {
            unsafe {
                (&mut *method_fd).attrs.need_home_object = true; // needed for brand check
            }

            self.emit_op(OpCode::SET_HOME_OBJECT);
            // XXX: missing function name
            self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
            if is_set {
                let setter_name = js_atom_concat_str(&self.ctx, name, "<set>")?;
                self.emit_atom(setter_name);

                let ret = add_private_class_field(
                    self,
                    self.cur_func_mut(),
                    setter_name,
                    JSVarKindEnum::JS_VAR_PRIVATE_SETTER,
                    is_static,
                );

                // setter_name.free(self.ctx.rt_mut());
                if ret < 0 {
                    return Err(-1);
                }
            } else {
                self.emit_atom(name);
            }
            self.emit_u16(self.cur_func().scope_level as _);
        } else {
            if name.is_null() {
                self.emit_op(OpCode::DEFINE_METHOD_COMPUTED);
            } else {
                self.emit_op(OpCode::DEFINE_METHOD);
                self.emit_atom(name);
            }
            self.emit_u8(if is_set {
                OP_DEFINE_METHOD_SETTER
            } else {
                OP_DEFINE_METHOD_GETTER
            });
        }

        Ok(())
    }

    /// parse class defination body: enclosed in a {} pair.
    /// class_name, class_var_name are not consumed.
    fn parse_class_body(
        &mut self,
        class_source: *const u8,
        class_name: Atom,
        class_var_name: Atom,
        class_flags: i32,
        class_fields: &mut [ClassFieldsDef; 2],
    ) -> Result<i32, i32> {
        macro_rules! private_field_already_defined {
            () => {
                self.throw_syntax_error("private class field is already defined");
                return Err(-1);
            };
        }

        // class item type
        let mut prop_type: i32;
        let mut is_private: bool;
        let mut start_ptr: *const u8;

        // current method function def
        let mut method_fd = std::ptr::null_mut::<JSFunctionDef>();
        // prop name atom
        let mut name = Atom::NULL;

        // this scope contains the private fields
        self.push_scope().unwrap();

        self.emit_op(OpCode::PUSH_CONST);
        let ctor_cpool_offset = self.cur_func_mut().byte_code.len();
        self.emit_u32(0); // will be patched at the end of the class parsing

        self.emit_op(OpCode::DEFINE_CLASS);
        self.emit_atom(if class_name.is_null() {
            if !class_var_name.is_null() {
                Atom::CONST_default
            } else {
                Atom::CONST_empty_string
            }
        } else {
            class_name
        });
        self.emit_u8(class_flags as u8);

        let define_class_offset: i32 = self.cur_func_mut().last_opcode_pos;

        // class function defination
        let mut ctor_fd: *mut JSFunctionDef = std::ptr::null_mut();

        while self.token.val != '}' as i32 {
            if self.token.val == ';' as i32 {
                self.next_token()?;
                continue;
            }

            let mut is_static = self.token.val == TOK_STATIC;
            prop_type = -1;

            if is_static {
                self.next_token()?;

                // "static" method
                if self.token.val == '{' as i32 {
                    self.parse_class_field(&mut class_fields[1])?; // 1: static field
                    continue;
                }

                // "static" field name
                if self.token.val == ';' as i32 || self.token.val == '=' as i32 {
                    is_static = false;
                    name = Atom::CONST_static;
                    prop_type = PROP_TYPE_IDENT;
                }
            }

            if is_static {
                self.emit_op(OpCode::SWAP);
            }

            start_ptr = self.token.ptr;
            if prop_type < 0 {
                let (nm, pt) = self.parse_property_name(true, false, true)?;
                name = nm;
                prop_type = pt;
            }
            is_private = prop_type & PROP_TYPE_PRIVATE != 0;
            prop_type &= !PROP_TYPE_PRIVATE;

            if (name == Atom::CONST_constructor && !is_static && prop_type != PROP_TYPE_IDENT)
                || (name == Atom::CONST_prototype && is_static)
                || name == Atom::CONST_hash_constructor
            {
                self.throw_syntax_error("invalid method name");
                return Err(-1);
            }

            match prop_type {
                PROP_TYPE_GET | PROP_TYPE_SET => {
                    self.parse_class_getset(
                        prop_type == PROP_TYPE_SET,
                        is_private,
                        is_static,
                        name,
                        &mut class_fields[is_static as usize],
                        start_ptr,
                    )?;
                }
                PROP_TYPE_IDENT if self.token.val != '(' as i32 => {
                    let cf = &mut class_fields[is_static as usize];
                    let mut field_var_name = Atom::NULL;

                    // class field

                    // XXX: spec: not consistent with method name checks
                    if matches!(name, Atom::CONST_constructor | Atom::CONST_prototype) {
                        self.throw_syntax_error("invalid field name");
                        return Err(-1);
                    }

                    if is_private {
                        if find_private_class_field(
                            self.cur_func_def,
                            name,
                            self.cur_func().scope_level,
                        ) >= 0
                        {
                            private_field_already_defined!();
                        }
                        if add_private_class_field(
                            self,
                            self.cur_func_mut(),
                            name,
                            JSVarKindEnum::JS_VAR_PRIVATE_FIELD,
                            is_static,
                        ) < 0
                        {
                            return Err(-1);
                        }
                        self.emit_op(OpCode::PRIVATE_SYMBOL);
                        self.emit_atom(name);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom(name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    }

                    if cf.fields_init_fd.is_null() {
                        self.emit_class_init_start(cf)?;
                    }

                    if name.is_null() {
                        // save the computed field name into a variable
                        field_var_name = js_atom_concat_num(
                            &self.ctx,
                            if is_static {
                                Atom::CONST__static_computed_field_
                            } else {
                                Atom::CONST__computed_field_
                            },
                            cf.computed_fields_count as u32,
                        )?;

                        self.define_var_cur_func(field_var_name, JSVarDefEnum::JS_VAR_DEF_CONST)?;

                        self.emit_op(OpCode::TO_PROPKEY);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom(field_var_name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    }

                    self.cur_func_def = cf.fields_init_fd;
                    self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                    self.emit_atom(Atom::CONST_this);
                    self.emit_u16(0);

                    if name.is_null() {
                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom(field_var_name);
                        self.emit_u16(self.cur_func().scope_level as _);
                        cf.computed_fields_count += 1;
                    } else if is_private {
                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom(name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    }

                    if self.token.val == '=' as i32 {
                        self.next_token()?;
                        self.parse_assign_expr()?;
                    } else {
                        self.emit_op(OpCode::UNDEFINED);
                    }
                    if is_private {
                        set_object_name_computed(self);
                        self.emit_op(OpCode::DEFINE_PRIVATE_FIELD);
                    } else if name.is_null() {
                        set_object_name_computed(self);
                        self.emit_op(OpCode::DEFINE_ARRAY_EL);
                        self.emit_op(OpCode::DROP);
                    } else {
                        self.set_object_name(name);
                        self.emit_op(OpCode::DEFINE_FIELD);
                        self.emit_atom(name);
                    }
                    self.cur_func_def = self.cur_func().parent;
                    self.parse_expect_semi()?;
                }

                _ => {
                    let mut func_type = JSParseFunctionEnum::JS_PARSE_FUNC_METHOD;

                    let func_kind = match prop_type {
                        PROP_TYPE_STAR => JSFunctionKindEnum::JS_FUNC_GENERATOR,
                        PROP_TYPE_ASYNC => JSFunctionKindEnum::JS_FUNC_ASYNC,
                        PROP_TYPE_ASYNC_STAR => JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR,
                        _ => {
                            if name == Atom::CONST_constructor && !is_static {
                                if !ctor_fd.is_null() {
                                    self.throw_syntax_error(
                                        "property constructor appears more than once",
                                    );
                                    return Err(-1);
                                }
                                func_type = if class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0 {
                                    JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
                                } else {
                                    JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                                };
                            }
                            JSFunctionKindEnum::JS_FUNC_NORMAL
                        }
                    };

                    if is_private {
                        class_fields[is_static as usize].need_brand = true;
                    }

                    self.parse_function_decl_ex(
                        func_type,
                        func_kind,
                        start_ptr,
                        self.token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_NONE,
                        &mut method_fd,
                    )?;

                    debug_assert!(!method_fd.is_null());

                    if matches!(
                        func_type,
                        JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
                            | JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                    ) {
                        ctor_fd = method_fd;
                    } else if is_private {
                        unsafe {
                            (*method_fd).attrs.need_home_object = true; // needed for brand check
                        }

                        if find_private_class_field(
                            self.cur_func_def,
                            name,
                            self.cur_func().scope_level,
                        ) >= 0
                        {
                            private_field_already_defined!();
                        }

                        if add_private_class_field(
                            self,
                            self.cur_func_mut(),
                            name,
                            JSVarKindEnum::JS_VAR_PRIVATE_METHOD,
                            is_static,
                        ) < 0
                        {
                            return Err(-1);
                        }
                        self.emit_op(OpCode::SET_HOME_OBJECT);
                        self.emit_op(OpCode::SET_NAME);
                        self.emit_atom(name);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom(name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    } else {
                        if name.is_null() {
                            self.emit_op(OpCode::DEFINE_METHOD_COMPUTED);
                        } else {
                            self.emit_op(OpCode::DEFINE_METHOD);
                            self.emit_atom(name);
                        }
                        self.emit_u8(OP_DEFINE_METHOD_METHOD);
                    }
                }
            }

            if is_static {
                self.emit_op(OpCode::SWAP);
            }

            name = Atom::NULL;
        }

        if self.token.val != '}' as i32 {
            self.throw_syntax_error("expecting '}'");
            return Err(-1);
        }

        if ctor_fd.is_null() {
            // default constructor implied
            ctor_fd = self.parse_class_default_ctor(
                class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0,
                ctor_fd,
            )?;
        }

        // patch the constant pool index for the constructor
        unsafe {
            write_to_ptr::<u32>(
                self.cur_func_mut()
                    .byte_code
                    .as_mut_ptr()
                    .add(ctor_cpool_offset),
                (*ctor_fd).parent_cpool_idx as u32,
            );
        }

        // store the class source code in the constructor.
        if !self.cur_func().attrs.exec_mode.contains(ExecMode::STRIP) {
            debug_assert!(self.buf_ptr >= class_source);

            let src_len = unsafe { self.buf_ptr.offset_from(class_source) as usize };
            if src_len > 0 {
                let mut src = Vec::<u8>::new();
                src.try_reserve_exact(src_len).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
                unsafe {
                    src.extend_from_slice(std::slice::from_raw_parts(class_source, src_len));
                    (*ctor_fd).source.replace(String::from_utf8_unchecked(src));
                }
            } else {
                unsafe {
                    (*ctor_fd).source.take();
                }
            }
        }

        // consume the '}'
        self.next_token()?;

        Ok(define_class_offset)
    }

    pub fn parse_class(
        &mut self,
        is_class_expr: bool,
        export_flag: JSParseExportEnum,
    ) -> Result<(), i32> {
        // class is always parsed and executed in strict mode
        let orig_mode = {
            let fd = self.cur_func_mut();
            let m = fd.attrs.exec_mode;
            fd.attrs.exec_mode |= ExecMode::STRICT;
            m
        };
        // Note: restore original mode on exit
        let _cur_func_ptr = self.cur_func_def;
        defer!(unsafe {
            _cur_func_ptr.as_mut().unwrap().attrs.exec_mode = orig_mode;
        });

        let class_source = self.token.ptr;

        // parse an ident for class name
        self.next_token()?;

        // parse class name
        let class_name = self.parse_class_name(is_class_expr, export_flag)?;

        let class_var_name = if !is_class_expr {
            if class_name.is_null() {
                Atom::CONST__default_ // export default
            } else {
                class_name
            }
        } else {
            Atom::NULL
        };

        self.push_scope().unwrap();

        // parse class flags: extends.
        let mut class_flags: i32 = 0;
        if self.token.val == TOK_EXTENDS {
            class_flags = JS_DEFINE_CLASS_HAS_HERITAGE;
            self.next_token()?;
            self.parse_left_hand_side_expr()?;
        } else {
            self.emit_op(OpCode::UNDEFINED);
        }

        // add a 'const' definition for the class name
        if !class_name.is_null() {
            self.define_var_cur_func(class_name, JSVarDefEnum::JS_VAR_DEF_CONST)?;
        }

        self.parse_expect('{' as i32)?;

        let mut class_fields = [
            // for instance
            ClassFieldsDef {
                fields_init_fd: std::ptr::null_mut(),
                computed_fields_count: 0,
                need_brand: false,
                brand_push_pos: 0,
                is_static: false,
            },
            // for static
            ClassFieldsDef {
                fields_init_fd: std::ptr::null_mut(),
                computed_fields_count: 0,
                need_brand: false,
                brand_push_pos: 0,
                is_static: true,
            },
        ];

        // entering class body {
        let define_class_offset = self.parse_class_body(
            class_source,
            class_name,
            class_var_name,
            class_flags,
            &mut class_fields,
        )?;

        // out of class body }
        {
            let cf = &mut class_fields[0];

            if cf.need_brand {
                // add a private brand to the prototype
                self.emit_op(OpCode::DUP);
                self.emit_op(OpCode::NULL);
                self.emit_op(OpCode::SWAP);
                self.emit_op(OpCode::ADD_BRAND);

                // define the brand field in 'this' of the initializer
                if cf.fields_init_fd.is_null() {
                    self.emit_class_init_start(cf)?;
                }
                // patch the start of the function to enable the OP_add_brand_instance code
                unsafe {
                    (&mut (*cf.fields_init_fd).byte_code)[cf.brand_push_pos as usize] =
                        OpCode::PUSH_TRUE as u8;
                }
            }

            // store the function to initialize the fields...
            self.define_var_cur_func(
                Atom::CONST__class_fields_init_,
                JSVarDefEnum::JS_VAR_DEF_CONST,
            )?;

            if !cf.fields_init_fd.is_null() {
                self.emit_class_init_end(cf);
            } else {
                self.emit_op(OpCode::UNDEFINED);
            }
            self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
            self.emit_atom(Atom::CONST__class_fields_init_);
            self.emit_u16(self.cur_func().scope_level as _);
        }

        // drop the prototype
        self.emit_op(OpCode::DROP);

        if class_fields[1].need_brand {
            // add a private brand to the class
            self.emit_op(OpCode::DUP);
            self.emit_op(OpCode::DUP);
            self.emit_op(OpCode::ADD_BRAND);
        }

        if !class_name.is_null() {
            /* store the class name in the scoped class name variable (it
            is independent from the class statement variable
            definition) */
            self.emit_op(OpCode::DUP);
            self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
            self.emit_atom(class_name);
            self.emit_u16(self.cur_func().scope_level as _);
        }

        /* initialize the static fields */
        if !class_fields[1].fields_init_fd.is_null() {
            let cf = &class_fields[1];
            self.emit_op(OpCode::DUP);
            self.emit_class_init_end(cf);
            self.emit_op(OpCode::CALL_METHOD);
            self.emit_u16(0);
            self.emit_op(OpCode::DROP);
        }

        self.pop_scope();
        self.pop_scope();

        /* the class statements have a block level scope */
        if !class_var_name.is_null() {
            self.define_var_cur_func(class_var_name, JSVarDefEnum::JS_VAR_DEF_LET)?;
            self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
            self.emit_atom(class_var_name);
            self.emit_u16(self.cur_func().scope_level as _);
        } else if class_name.is_null() {
            /* cannot use OP_set_name because the name of the class
            must be defined before the static initializers are
            executed */
            self.emit_op_temp(OpCodeTemp::SET_CLASS_NAME);
            self.emit_u32(self.cur_func().last_opcode_pos as u32 + 1 - define_class_offset as u32);
        }

        if export_flag != JSParseExportEnum::JS_PARSE_EXPORT_NONE {
            self.add_module_export(
                None,
                class_var_name,
                if export_flag == JSParseExportEnum::JS_PARSE_EXPORT_NAMED {
                    class_var_name
                } else {
                    Atom::CONST_default
                },
                ExportType::JS_EXPORT_TYPE_LOCAL {
                    var_idx: u16::MAX, // unknown
                    var_ref: None,
                },
            )?;
        }

        Ok(())
    }

    // XXX: could generate specific bytecode
    fn parse_class_default_ctor(
        &mut self,
        has_super: bool,
        fd: *mut JSFunctionDef,
    ) -> Result<*mut JSFunctionDef, i32> {
        let pos = self.get_pos();

        let (src, func_type) = if has_super {
            // spec change: no argument evaluation
            (
                "(){super(...arguments);}",
                JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR,
            )
        } else {
            ("(){}", JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR)
        };

        // save current state
        let orig_buf_ptr = self.buf_ptr;
        let orig_buf_end = self.input_end;
        let orig_token = std::mem::take(&mut self.token);

        // set new source and parse
        self.buf_ptr = src.as_ptr();
        self.input_end = unsafe { src.as_ptr().add(src.len()) };

        let mut fd_fixed = fd;

        // Note: calling graph: next_token -> parse_function_decl2 -> seek_token
        let ret = self.next_token().and_then(|_| {
            self.parse_function_decl_ex(
                func_type,
                JSFunctionKindEnum::JS_FUNC_NORMAL,
                src.as_ptr(),
                orig_token.line_num,
                JSParseExportEnum::JS_PARSE_EXPORT_NONE,
                &mut fd_fixed,
            )
        });

        // restore original parse state
        self.buf_ptr = orig_buf_ptr;
        self.input_end = orig_buf_end;
        self.token = orig_token;

        if ret.is_err() {
            Err(-1)
        } else {
            self.seek_token(&pos)?;
            Ok(fd_fixed)
        }
    }
}

// create a function to initialize class fields
pub(super) fn js_parse_function_class_fields_init<'s>(
    s: &mut JSParseState<'s>,
) -> Result<Box<JSFunctionDef>, i32> {
    let mut fd = JSFunctionDef::new(&s.ctx, Some(s.cur_func_mut()), false, false, s.filename, 0)?;

    fd.func_type = JSParseFunctionEnum::JS_PARSE_FUNC_METHOD;
    fd.has_home_object = true; // <-- this line must be prior to `fd.attrs.super_allowed = XXX`
    fd.has_arguments_binding = false;
    fd.has_this_binding = true;

    fd.attrs.func_kind = JSFunctionKindEnum::JS_FUNC_NORMAL;
    fd.attrs.super_allowed = fd.has_home_object;
    fd.attrs.new_target_allowed = true;

    Ok(fd)
}

/// add a private field variable in the current scope
fn add_private_class_field(
    s: &JSParseState,
    fd: &mut JSFunctionDef,
    name: Atom,
    var_kind: JSVarKindEnum,
    is_static: bool,
) -> i32 {
    let idx = if let Ok(i) = fd.add_scope_var(&s.ctx, name, var_kind) {
        i
    } else {
        return -1;
    };

    let vd = fd.vars.get_mut(idx as usize).unwrap();
    vd.set_is_lexical(true);
    vd.set_is_const(true);
    vd.set_is_static_private(is_static);

    idx as i32
}

// find field in the current scope
fn find_private_class_field(fd: *mut JSFunctionDef, name: Atom, scope_level: i32) -> i32 {
    let mut idx = unsafe { (&*fd).scopes[scope_level as usize].first };
    while idx != -1 {
        let vd = unsafe { (&*fd).vars.get_unchecked(idx as usize) };
        if vd.scope_level != scope_level {
            break;
        }
        if vd.var_name == name {
            return idx;
        }
        idx = vd.scope_next;
    }

    -1
}
