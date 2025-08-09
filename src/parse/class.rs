use crate::{
    atom::{
        JS_ATOM_NULL, JS_FreeAtom, JS_FreeAtomRT, JSAtom, consts::*, js_atom_concat_num,
        js_atom_concat_str,
    },
    bytecode::{OpCode, OpCodeTemp},
    class::{
        ClassFieldsDef, JS_DEFINE_CLASS_HAS_HERITAGE, OP_DEFINE_METHOD_GETTER,
        OP_DEFINE_METHOD_METHOD, OP_DEFINE_METHOD_SETTER,
    },
    context::{JS_MODE_STRICT, JS_MODE_STRIP, JSContext},
    defer,
    function::{JSFunctionDef, JSFunctionKindEnum, JSVarDefEnum, JSVarKindEnum},
    module::JSExportTypeEnum,
    object::{
        PROP_TYPE_ASYNC, PROP_TYPE_ASYNC_STAR, PROP_TYPE_GET, PROP_TYPE_IDENT, PROP_TYPE_PRIVATE,
        PROP_TYPE_SET, PROP_TYPE_STAR,
    },
    parse::{obj::set_object_name_computed, token::Token},
    utils::write_to_ptr,
};

use super::{
    esm::JSParseExportEnum,
    function::JSParseFunctionEnum,
    state::JSParseState,
    token::{TOK_EXTENDS, TOK_IDENT, TOK_STATIC},
};

impl<'s> JSParseState<'s> {
    /// caller should free the returned Atom.
    /// TODO: return Result<Option<JSAtom>,i32>
    fn parse_class_name(
        &mut self,
        is_class_expr: bool,
        export_flag: JSParseExportEnum,
    ) -> Result<JSAtom, i32> {
        if self.token.val == TOK_IDENT {
            if self.token.data.is_reserved() {
                self.throw_reserved_identifier_error();
                return Err(-1);
            }

            // Note: next_token() will free the atom, dup class_name atom to keep it.
            let class_name = self.token.atom().unwrap().dup(self.ctx.rt());

            self.next_token().inspect_err(|_| {
                JS_FreeAtom(&self.ctx, class_name);
            })?;
            Ok(class_name)
        } else if !is_class_expr && export_flag != JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT {
            self.throw_syntax_error("class statement requires a name");
            Err(-1)
        } else {
            Ok(JS_ATOM_NULL)
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
        self.parse_function_decl2(
            JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT,
            JSFunctionKindEnum::JS_FUNC_NORMAL,
            JS_ATOM_NULL,
            self.token.ptr,
            self.token.line_num,
            JSParseExportEnum::JS_PARSE_EXPORT_NONE,
            std::ptr::addr_of_mut!(init),
        )?;

        // stack is now: fclosure
        self.push_scope()?;
        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
        self.emit_atom_dup(JS_ATOM_this);
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

    unsafe fn parse_class_getset(
        &mut self,
        is_set: bool,
        is_private: bool,
        is_static: bool,
        name: JSAtom,
        cf: &mut ClassFieldsDef,
        start_ptr: *const u8,
    ) -> Result<(), i32> {
        if is_private {
            let ctx = self.ctx.clone();
            let fd = self.cur_func_def;

            let idx = find_private_class_field(&ctx, fd, name, (*fd).scope_level);
            if idx >= 0 {
                let vd = (&mut (*fd).vars).get_mut(idx as usize).unwrap();
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
            } else {
                if add_private_class_field(
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
            }

            cf.need_brand = true;
        }

        // parse getter or setter function
        let mut method_fd = std::ptr::null_mut::<JSFunctionDef>();
        self.parse_function_decl2(
            if is_set {
                JSParseFunctionEnum::JS_PARSE_FUNC_SETTER
            } else {
                JSParseFunctionEnum::JS_PARSE_FUNC_GETTER
            },
            JSFunctionKindEnum::JS_FUNC_NORMAL,
            JS_ATOM_NULL,
            start_ptr,
            self.token.line_num,
            JSParseExportEnum::JS_PARSE_EXPORT_NONE,
            &mut method_fd,
        )?;

        if is_private {
            (*method_fd).need_home_object = true; // needed for brand check
            self.emit_op(OpCode::SET_HOME_OBJECT);
            // XXX: missing function name
            self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
            if is_set {
                let setter_name = get_private_setter_name(&self.ctx, name);
                if setter_name == JS_ATOM_NULL {
                    return Err(-1);
                }
                self.emit_atom_dup(setter_name);

                let ret = unsafe {
                    add_private_class_field(
                        self,
                        self.cur_func_mut(),
                        setter_name,
                        JSVarKindEnum::JS_VAR_PRIVATE_SETTER,
                        is_static,
                    )
                };
                JS_FreeAtom(&self.ctx, setter_name);
                if ret < 0 {
                    return Err(-1);
                }
            } else {
                self.emit_atom_dup(name);
            }
            self.emit_u16(self.cur_func().scope_level as _);
        } else {
            if name == JS_ATOM_NULL {
                self.emit_op(OpCode::DEFINE_METHOD_COMPUTED);
            } else {
                self.emit_op(OpCode::DEFINE_METHOD);
                self.emit_atom_dup(name);
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
        class_name: JSAtom,
        class_var_name: JSAtom,
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
        let define_class_offset: i32;
        let mut is_private: bool;
        let mut start_ptr: *const u8;

        // current method function def
        let mut method_fd = std::ptr::null_mut::<JSFunctionDef>();

        // name is mutable, use reference because using defer!()
        let mut _name = JS_ATOM_NULL;
        let name = &mut _name;

        // NOTE: clean up on exit: free name atom.
        let _rt = self.ctx.rt_ptr();
        let _name_ptr = name as *mut JSAtom;
        defer!(unsafe {
            JS_FreeAtomRT(_rt.as_ptr(), *_name_ptr);
        });

        // this scope contains the private fields
        self.push_scope().unwrap();

        self.emit_op(OpCode::PUSH_CONST);
        let ctor_cpool_offset = self.cur_func_mut().byte_code.len();
        self.emit_u32(0); // will be patched at the end of the class parsing

        self.emit_op(OpCode::DEFINE_CLASS);
        self.emit_atom_dup(if class_name == JS_ATOM_NULL {
            if class_var_name != JS_ATOM_NULL {
                JS_ATOM_default
            } else {
                JS_ATOM_empty_string
            }
        } else {
            class_name
        });
        self.emit_u8(class_flags as u8);
        define_class_offset = self.cur_func_mut().last_opcode_pos;

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
                    *name = JS_ATOM_static.dup(&self.ctx.rt());
                    prop_type = PROP_TYPE_IDENT;
                }
            }

            if is_static {
                self.emit_op(OpCode::SWAP);
            }

            start_ptr = self.token.ptr;
            if prop_type < 0 {
                (*name, prop_type) = self.parse_property_name(true, false, true)?;
            }
            is_private = prop_type & PROP_TYPE_PRIVATE != 0;
            prop_type &= !PROP_TYPE_PRIVATE;

            if (*name == JS_ATOM_constructor && !is_static && prop_type != PROP_TYPE_IDENT)
                || (*name == JS_ATOM_prototype && is_static)
                || *name == JS_ATOM_hash_constructor
            {
                self.throw_syntax_error("invalid method name");
                return Err(-1);
            }

            match prop_type {
                PROP_TYPE_GET | PROP_TYPE_SET => unsafe {
                    self.parse_class_getset(
                        prop_type == PROP_TYPE_SET,
                        is_private,
                        is_static,
                        *name,
                        &mut class_fields[is_static as usize],
                        start_ptr,
                    )?;
                },
                PROP_TYPE_IDENT if self.token.val != '(' as i32 => {
                    let cf = &mut class_fields[is_static as usize];
                    let mut field_var_name: JSAtom = JS_ATOM_NULL;

                    // class field

                    // XXX: spec: not consistent with method name checks
                    if matches!(*name, JS_ATOM_constructor | JS_ATOM_prototype) {
                        self.throw_syntax_error("invalid field name");
                        return Err(-1);
                    }

                    if is_private {
                        let fd = self.cur_func_def;
                        if unsafe {
                            find_private_class_field(&self.ctx, fd, *name, (*fd).scope_level) >= 0
                        } {
                            private_field_already_defined!();
                        }
                        if unsafe {
                            add_private_class_field(
                                self,
                                fd,
                                *name,
                                JSVarKindEnum::JS_VAR_PRIVATE_FIELD,
                                is_static,
                            )
                        } < 0
                        {
                            return Err(-1);
                        }
                        self.emit_op(OpCode::PRIVATE_SYMBOL);
                        self.emit_atom_dup(*name);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom_dup(*name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    }

                    if cf.fields_init_fd.is_null() {
                        self.emit_class_init_start(cf)?;
                    }

                    if *name == JS_ATOM_NULL {
                        // save the computed field name into a variable
                        field_var_name = unsafe {
                            js_atom_concat_num(
                                &self.ctx,
                                if is_static {
                                    JS_ATOM__static_computed_field_
                                } else {
                                    JS_ATOM__computed_field_
                                },
                                (*cf).computed_fields_count as u32,
                            )
                        };
                        if field_var_name == JS_ATOM_NULL {
                            return Err(-1);
                        }
                        if self
                            .define_var_cur_func(field_var_name, JSVarDefEnum::JS_VAR_DEF_CONST)
                            .is_err()
                        {
                            JS_FreeAtom(&self.ctx, field_var_name);
                            return Err(-1);
                        }

                        self.emit_op(OpCode::TO_PROPKEY);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom_dup(field_var_name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    }

                    self.cur_func_def = cf.fields_init_fd;
                    self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                    self.emit_atom_dup(JS_ATOM_this);
                    self.emit_u16(0);

                    if *name == JS_ATOM_NULL {
                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom_dup(field_var_name);
                        self.emit_u16(self.cur_func().scope_level as _);
                        cf.computed_fields_count += 1;
                        JS_FreeAtom(&self.ctx, field_var_name);
                    } else if is_private {
                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom_dup(*name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    }

                    if self.token.val == '=' as i32 {
                        self.next_token()?;
                        self.parse_assign_expr()?;
                    } else {
                        self.emit_op(OpCode::UNDEFINED);
                    }
                    if is_private {
                        unsafe {
                            set_object_name_computed(self);
                        }
                        self.emit_op(OpCode::DEFINE_PRIVATE_FIELD);
                    } else if *name == JS_ATOM_NULL {
                        unsafe {
                            set_object_name_computed(self);
                        }
                        self.emit_op(OpCode::DEFINE_ARRAY_EL);
                        self.emit_op(OpCode::DROP);
                    } else {
                        self.set_object_name(*name);
                        self.emit_op(OpCode::DEFINE_FIELD);
                        self.emit_atom_dup(*name);
                    }
                    self.cur_func_def = self.cur_func().parent;
                    self.parse_expect_semi()?;
                }

                _ => {
                    let mut func_type = JSParseFunctionEnum::JS_PARSE_FUNC_METHOD;
                    let mut func_kind = JSFunctionKindEnum::JS_FUNC_NORMAL;

                    if prop_type == PROP_TYPE_STAR {
                        func_kind = JSFunctionKindEnum::JS_FUNC_GENERATOR;
                    } else if prop_type == PROP_TYPE_ASYNC {
                        func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
                    } else if prop_type == PROP_TYPE_ASYNC_STAR {
                        func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR;
                    } else if *name == JS_ATOM_constructor && !is_static {
                        if !ctor_fd.is_null() {
                            self.throw_syntax_error("property constructor appears more than once");
                            return Err(-1);
                        }
                        func_type = if class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0 {
                            JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
                        } else {
                            JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                        };
                    }
                    if is_private {
                        class_fields[is_static as usize].need_brand = true;
                    }

                    self.parse_function_decl2(
                        func_type,
                        func_kind,
                        JS_ATOM_NULL,
                        start_ptr,
                        self.token.line_num,
                        JSParseExportEnum::JS_PARSE_EXPORT_NONE,
                        &mut method_fd,
                    )?;

                    if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
                        || func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
                    {
                        ctor_fd = method_fd;
                    } else if is_private {
                        unsafe {
                            (*method_fd).need_home_object = true; // needed for brand check
                        }
                        let fd = self.cur_func_def;
                        if unsafe {
                            find_private_class_field(&self.ctx, fd, *name, (*fd).scope_level)
                        } >= 0
                        {
                            private_field_already_defined!();
                        }

                        if unsafe {
                            add_private_class_field(
                                self,
                                fd,
                                *name,
                                JSVarKindEnum::JS_VAR_PRIVATE_METHOD,
                                is_static,
                            )
                        } < 0
                        {
                            return Err(-1);
                        }
                        self.emit_op(OpCode::SET_HOME_OBJECT);
                        self.emit_op(OpCode::SET_NAME);
                        self.emit_atom_dup(*name);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom_dup(*name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    } else {
                        if *name == JS_ATOM_NULL {
                            self.emit_op(OpCode::DEFINE_METHOD_COMPUTED);
                        } else {
                            self.emit_op(OpCode::DEFINE_METHOD);
                            self.emit_atom_dup(*name);
                        }
                        self.emit_u8(OP_DEFINE_METHOD_METHOD);
                    }
                }
            }

            if is_static {
                self.emit_op(OpCode::SWAP);
            }

            JS_FreeAtom(&self.ctx, *name);
            *name = JS_ATOM_NULL;
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
        if self.cur_func().js_mode & JS_MODE_STRIP as u8 == 0 {
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
            let m = fd.js_mode;
            fd.js_mode |= JS_MODE_STRICT as u8;
            m
        };
        // Note: restore original mode on exit
        let _cur_func_ptr = self.cur_func_def;
        defer!(unsafe {
            _cur_func_ptr.as_mut().unwrap().js_mode = orig_mode;
        });

        let class_source = self.token.ptr;

        // parse an ident for class name
        self.next_token()?;

        // parse class name
        let class_name = self.parse_class_name(is_class_expr, export_flag)?;

        let class_var_name = if !is_class_expr {
            if class_name == JS_ATOM_NULL {
                JS_ATOM__default_ // export default
            } else {
                class_name
            }
            .dup(&self.ctx.rt())
        } else {
            JS_ATOM_NULL
        };

        // clean up on exit:
        // - free atom class_name, class_var_name
        // - restore original mode on cur_func.
        let mut _rt = self.ctx.rt_ptr();
        let _class_name_atom_id = class_name.0;
        let _class_var_name_atom_id = class_var_name.0;
        defer!(unsafe {
            JS_FreeAtomRT(_rt.as_mut(), JSAtom(_class_name_atom_id));
            JS_FreeAtomRT(_rt.as_mut(), JSAtom(_class_var_name_atom_id));
        });

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
        if class_name != JS_ATOM_NULL {
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
            self.define_var_cur_func(JS_ATOM__class_fields_init_, JSVarDefEnum::JS_VAR_DEF_CONST)?;

            if !cf.fields_init_fd.is_null() {
                self.emit_class_init_end(cf);
            } else {
                self.emit_op(OpCode::UNDEFINED);
            }
            self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
            self.emit_atom_dup(JS_ATOM__class_fields_init_);
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

        if class_name != JS_ATOM_NULL {
            /* store the class name in the scoped class name variable (it
            is independent from the class statement variable
            definition) */
            self.emit_op(OpCode::DUP);
            self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
            self.emit_atom_dup(class_name);
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
        if class_var_name != JS_ATOM_NULL {
            self.define_var_cur_func(class_var_name, JSVarDefEnum::JS_VAR_DEF_LET)?;
            self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
            self.emit_atom_dup(class_var_name);
            self.emit_u16(self.cur_func().scope_level as _);
        } else {
            if class_name == JS_ATOM_NULL {
                /* cannot use OP_set_name because the name of the class
                must be defined before the static initializers are
                executed */
                self.emit_op_temp(OpCodeTemp::SET_CLASS_NAME);
                self.emit_u32(
                    self.cur_func().last_opcode_pos as u32 + 1 - define_class_offset as u32,
                );
            }
        }

        if export_flag != JSParseExportEnum::JS_PARSE_EXPORT_NONE {
            self.add_module_export(
                None,
                class_var_name,
                if export_flag == JSParseExportEnum::JS_PARSE_EXPORT_NAMED {
                    class_var_name
                } else {
                    JS_ATOM_default
                },
                JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                    var_idx: u16::MAX,
                    var_ref: None,
                },
            )?;
        }

        // Hint: clean up will be done in defer!()

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
        let orig_buf_end = self.buf_end;
        let orig_token = std::mem::replace(&mut self.token, Token::default());

        // set new source and parse
        self.buf_ptr = src.as_ptr();
        self.buf_end = unsafe { src.as_ptr().add(src.len()) };

        let mut fd_fixed = fd;

        // Note: calling graph: next_token -> parse_function_decl2 -> seek_token
        let ret = self.next_token().and_then(|_| unsafe {
            self.parse_function_decl2(
                func_type,
                JSFunctionKindEnum::JS_FUNC_NORMAL,
                JS_ATOM_NULL,
                src.as_ptr(),
                orig_token.line_num,
                JSParseExportEnum::JS_PARSE_EXPORT_NONE,
                &mut fd_fixed,
            )
        });

        // restore original parse state
        self.buf_ptr = orig_buf_ptr;
        self.buf_end = orig_buf_end;
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
pub unsafe fn js_parse_function_class_fields_init(
    s: *mut JSParseState,
) -> Result<Box<JSFunctionDef>, i32> {
    let mut fd = JSFunctionDef::new(
        &(*s).ctx,
        (*s).cur_func_mut(),
        false,
        false,
        (*s).filename,
        0,
    )?;

    fd.func_name = JS_ATOM_NULL;
    fd.has_prototype = false;
    fd.has_home_object = true;

    fd.has_arguments_binding = false;
    fd.has_this_binding = true;
    fd.is_derived_class_constructor = false;
    fd.new_target_allowed = true;
    fd.super_call_allowed = false;
    fd.super_allowed = fd.has_home_object;
    fd.arguments_allowed = false;

    fd.func_kind = JSFunctionKindEnum::JS_FUNC_NORMAL;
    fd.func_type = JSParseFunctionEnum::JS_PARSE_FUNC_METHOD;

    Ok(fd)
}

// add a private field variable in the current scope
pub unsafe fn add_private_class_field(
    s: *mut JSParseState,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    var_kind: JSVarKindEnum,
    is_static: bool,
) -> i32 {
    let idx = if let Ok(i) = (*fd).add_scope_var(&(*s).ctx, name, var_kind) {
        i
    } else {
        return -1;
    };

    let vd = (&mut (*fd).vars).get_mut(idx as usize).unwrap();
    vd.set_is_lexical(true);
    vd.set_is_const(true);
    vd.set_is_static_private(is_static);

    idx as i32
}

// find field in the current scope
pub unsafe fn find_private_class_field(
    ctx: &JSContext,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    scope_level: i32,
) -> i32 {
    let mut idx = (&(*fd).scopes)[scope_level as usize].first;
    while idx != -1 {
        let vd = (&(*fd).vars).get(idx as usize).unwrap();
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

/* build a private setter function name from the private getter name */
pub fn get_private_setter_name(ctx: &JSContext, name: JSAtom) -> JSAtom {
    unsafe { js_atom_concat_str(ctx, name, c"<set>".as_ptr().cast()) }
}

// XXX: could generate specific bytecode
#[deprecated(note = "use JSParseState::parse_class_default_ctor() instead")]
pub unsafe fn js_parse_class_default_ctor(
    s: &mut JSParseState,
    has_super: bool,
    pfd: *mut *mut JSFunctionDef,
) -> i32 {
    unreachable!("unused");
}
