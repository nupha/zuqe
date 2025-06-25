use std::{ffi::CStr, ops::Deref};

use crate::{
    atom::{
        JS_ATOM_NULL, JS_DupAtom, JS_FreeAtom, JS_FreeAtomRT, JS_ValueToAtom, JSAtom, consts::*,
        js_atom_concat_num, js_atom_concat_str,
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
    parse::{
        esm::add_export_entry,
        obj::{set_object_name, set_object_name_computed},
        token::CHAR_SQUARE_L,
        var::define_var,
    },
    utils::write_to_ptr,
    value::{
        JSValue,
        object::{
            PROP_TYPE_ASYNC, PROP_TYPE_ASYNC_STAR, PROP_TYPE_GET, PROP_TYPE_IDENT,
            PROP_TYPE_PRIVATE, PROP_TYPE_SET, PROP_TYPE_STAR, PROP_TYPE_VAR,
        },
    },
};

use super::{
    emit::{
        emit_atom_dup, emit_goto, emit_label, emit_op, emit_op_temp, emit_u8, emit_u16, emit_u32,
    },
    error::js_parse_error_reserved_identifier,
    esm::JSParseExportEnum,
    function::JSParseFunctionEnum,
    state::JSParseState,
    token::{
        TOK_EXTENDS, TOK_IDENT, TOK_NUMBER, TOK_PRIVATE_NAME, TOK_STATIC, TOK_STRING, peek_token,
        token_is_ident, token_is_pseudo_keyword,
    },
};

impl<'s> JSParseState<'s> {
    /// caller should free the returned Atom.
    unsafe fn parse_class_name(
        &mut self,
        is_class_expr: bool,
        export_flag: JSParseExportEnum,
    ) -> Result<JSAtom, i32> {
        let mut class_name = JS_ATOM_NULL;

        if self.token.val == TOK_IDENT {
            if self.token.data.is_reserved() {
                js_parse_error_reserved_identifier(self);
                return Err(-1);
            }
            // next_token() will free the atom, dup it in advance.
            class_name = self.token.data.ident_atom().unwrap().dup(&self.ctx.rt());
            self.next_token().inspect_err(|_| {
                JS_FreeAtom(&self.ctx, class_name);
            })?;
        } else if !is_class_expr && export_flag != JSParseExportEnum::JS_PARSE_EXPORT_DEFAULT {
            self.throw_syntax_error("class statement requires a name");
            JS_FreeAtom(&self.ctx, class_name);
            return Err(-1);
        }

        Ok(class_name)
    }

    /// parse class field defination
    unsafe fn parse_class_field(&mut self, cf: &mut ClassFieldsDef) -> Result<(), i32> {
        let mut init = std::ptr::null_mut::<JSFunctionDef>();

        if cf.fields_init_fd.is_null() {
            emit_class_init_start(self, cf)?;
        }

        self.cur_func_ptr = cf.fields_init_fd;

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
        self.push_scope();
        emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
        emit_atom_dup(self, JS_ATOM_this);
        emit_u16(self, 0);
        // stack is now: fclosure this
        emit_op(self, OpCode::SWAP);
        // stack is now: this fclosure
        emit_op(self, OpCode::CALL_METHOD);
        emit_u16(self, 0);
        // stack is now: returnvalue
        emit_op(self, OpCode::DROP);
        // stack is now: <empty>
        self.pop_scope();
        self.cur_func_ptr = self.cur_func().parent;

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
            let fd = self.cur_func_ptr;

            let idx = find_private_class_field(&ctx, fd, name, (*fd).scope_level);
            if idx >= 0 {
                let vd = (*fd).vars.get_mut(idx as usize).unwrap();
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
            emit_op(self, OpCode::SET_HOME_OBJECT);
            // XXX: missing function name
            emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
            if is_set {
                let setter_name = get_private_setter_name(&self.ctx, name);
                if setter_name == JS_ATOM_NULL {
                    return Err(-1);
                }
                emit_atom_dup(self, setter_name);

                let ret = add_private_class_field(
                    self,
                    self.cur_func_mut(),
                    setter_name,
                    JSVarKindEnum::JS_VAR_PRIVATE_SETTER,
                    is_static,
                );
                JS_FreeAtom(&self.ctx, setter_name);
                if ret < 0 {
                    return Err(-1);
                }
            } else {
                emit_atom_dup(self, name);
            }
            emit_u16(self, self.cur_func().scope_level as _);
        } else {
            if name == JS_ATOM_NULL {
                emit_op(self, OpCode::DEFINE_METHOD_COMPUTED);
            } else {
                emit_op(self, OpCode::DEFINE_METHOD);
                emit_atom_dup(self, name);
            }
            emit_u8(
                self,
                if is_set {
                    OP_DEFINE_METHOD_SETTER
                } else {
                    OP_DEFINE_METHOD_GETTER
                },
            );
        }

        Ok(())
    }

    /// parse class defination body: enclosed in a {} pair.
    /// class_name, class_var_name are not consumed.
    unsafe fn parse_class_body(
        &mut self,
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

        // current method function def
        let mut method_fd = std::ptr::null_mut::<JSFunctionDef>();

        // class item type
        let mut prop_type: i32;

        let define_class_offset: i32;
        let mut is_private: bool;
        let class_start_ptr: *const u8 = self.token.ptr;
        let mut start_ptr: *const u8;

        let mut name = JS_ATOM_NULL;

        // NOTE: free name atom before exit.
        let rt = self.ctx.rt_ptr();
        let _a = JSAtom(name.0);
        defer!({
            JS_FreeAtomRT(rt.as_ptr(), _a);
        });

        // this scope contains the private fields
        self.push_scope();

        emit_op(self, OpCode::PUSH_CONST);
        let ctor_cpool_offset = self.cur_func_mut().byte_code.len();
        emit_u32(self, 0); // will be patched at the end of the class parsing

        emit_op(self, OpCode::DEFINE_CLASS);
        emit_atom_dup(
            self,
            if class_name == JS_ATOM_NULL {
                if class_var_name != JS_ATOM_NULL {
                    JS_ATOM_default
                } else {
                    JS_ATOM_empty_string
                }
            } else {
                class_name
            },
        );
        emit_u8(self, class_flags as u8);
        define_class_offset = self.cur_func_mut().last_opcode_pos;

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
                    name = JS_ATOM_static.dup(&self.ctx.rt());
                    prop_type = PROP_TYPE_IDENT;
                }
            }

            if is_static {
                emit_op(self, OpCode::SWAP);
            }

            start_ptr = self.token.ptr;
            if prop_type < 0 {
                prop_type = js_parse_property_name(self, &mut name, true, false, true);
                if prop_type < 0 {
                    return Err(-1);
                }
            }
            is_private = prop_type & PROP_TYPE_PRIVATE != 0;
            prop_type &= !PROP_TYPE_PRIVATE;

            if (name == JS_ATOM_constructor && !is_static && prop_type != PROP_TYPE_IDENT)
                || (name == JS_ATOM_prototype && is_static)
                || name == JS_ATOM_hash_constructor
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
                    let mut field_var_name: JSAtom = JS_ATOM_NULL;

                    // class field

                    // XXX: spec: not consistent with method name checks
                    if name == JS_ATOM_constructor || name == JS_ATOM_prototype {
                        self.throw_syntax_error("invalid field name");
                        return Err(-1);
                    }

                    if is_private {
                        let fd = self.cur_func_ptr;
                        if find_private_class_field(&self.ctx, fd, name, (*fd).scope_level) >= 0 {
                            private_field_already_defined!();
                        }
                        if add_private_class_field(
                            self,
                            fd,
                            name,
                            JSVarKindEnum::JS_VAR_PRIVATE_FIELD,
                            is_static,
                        ) < 0
                        {
                            return Err(-1);
                        }
                        emit_op(self, OpCode::PRIVATE_SYMBOL);
                        emit_atom_dup(self, name);
                        emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        emit_atom_dup(self, name);
                        emit_u16(self, self.cur_func().scope_level as _);
                    }

                    if cf.fields_init_fd.is_null() {
                        emit_class_init_start(self, cf)?;
                    }

                    if name == JS_ATOM_NULL {
                        // save the computed field name into a variable
                        field_var_name = js_atom_concat_num(
                            &self.ctx,
                            if is_static {
                                JS_ATOM_static_computed_field
                            } else {
                                JS_ATOM_computed_field
                            },
                            (*cf).computed_fields_count as u32,
                        );
                        if field_var_name == JS_ATOM_NULL {
                            return Err(-1);
                        }
                        if define_var(
                            self,
                            self.cur_func_mut(),
                            field_var_name,
                            JSVarDefEnum::JS_VAR_DEF_CONST,
                        ) < 0
                        {
                            JS_FreeAtom(&self.ctx, field_var_name);
                            return Err(-1);
                        }

                        emit_op(self, OpCode::TO_PROPKEY);
                        emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        emit_atom_dup(self, field_var_name);
                        emit_u16(self, self.cur_func().scope_level as _);
                    }

                    self.cur_func_ptr = cf.fields_init_fd;
                    emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                    emit_atom_dup(self, JS_ATOM_this);
                    emit_u16(self, 0);

                    if name == JS_ATOM_NULL {
                        emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                        emit_atom_dup(self, field_var_name);
                        emit_u16(self, self.cur_func().scope_level as _);
                        cf.computed_fields_count += 1;
                        JS_FreeAtom(&self.ctx, field_var_name);
                    } else if is_private {
                        emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                        emit_atom_dup(self, name);
                        emit_u16(self, self.cur_func().scope_level as _);
                    }

                    if self.token.val == '=' as i32 {
                        self.next_token()?;
                        self.parse_assign_expr()?;
                    } else {
                        emit_op(self, OpCode::UNDEFINED);
                    }
                    if is_private {
                        set_object_name_computed(self);
                        emit_op(self, OpCode::DEFINE_PRIVATE_FIELD);
                    } else if name == JS_ATOM_NULL {
                        set_object_name_computed(self);
                        emit_op(self, OpCode::DEFINE_ARRAY_EL);
                        emit_op(self, OpCode::DROP);
                    } else {
                        set_object_name(self, name);
                        emit_op(self, OpCode::DEFINE_FIELD);
                        emit_atom_dup(self, name);
                    }
                    self.cur_func_ptr = self.cur_func().parent;
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
                    } else if name == JS_ATOM_constructor && !is_static {
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
                        (*method_fd).need_home_object = true; // needed for brand check

                        let fd = self.cur_func_ptr;
                        if find_private_class_field(&self.ctx, fd, name, (*fd).scope_level) >= 0 {
                            private_field_already_defined!();
                        }

                        if add_private_class_field(
                            self,
                            fd,
                            name,
                            JSVarKindEnum::JS_VAR_PRIVATE_METHOD,
                            is_static,
                        ) < 0
                        {
                            return Err(-1);
                        }
                        emit_op(self, OpCode::SET_HOME_OBJECT);
                        emit_op(self, OpCode::SET_NAME);
                        emit_atom_dup(self, name);
                        emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        emit_atom_dup(self, name);
                        emit_u16(self, self.cur_func().scope_level as _);
                    } else {
                        if name == JS_ATOM_NULL {
                            emit_op(self, OpCode::DEFINE_METHOD_COMPUTED);
                        } else {
                            emit_op(self, OpCode::DEFINE_METHOD);
                            emit_atom_dup(self, name);
                        }
                        emit_u8(self, OP_DEFINE_METHOD_METHOD);
                    }
                }
            }

            if is_static {
                emit_op(self, OpCode::SWAP);
            }

            JS_FreeAtom(&self.ctx, name);
            name = JS_ATOM_NULL;
        }

        if self.token.val != '}' as i32 {
            self.throw_syntax_error("expecting '}'");
            return Err(-1);
        }

        if ctor_fd.is_null() {
            if js_parse_class_default_ctor(
                self,
                class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0,
                &mut ctor_fd,
            ) != 0
            {
                return Err(-1);
            }
        }

        // patch the constant pool index for the constructor
        write_to_ptr::<u32>(
            self.cur_func_mut()
                .byte_code
                .as_mut_ptr()
                .add(ctor_cpool_offset),
            (*ctor_fd).parent_cpool_idx as u32,
        );

        // store the class source code in the constructor.
        if self.cur_func().js_mode & JS_MODE_STRIP as u8 == 0 {
            debug_assert!(self.buf_ptr >= class_start_ptr);

            let src_len = self.buf_ptr.offset_from(class_start_ptr) as usize;
            if src_len > 0 {
                let mut src = Vec::<u8>::new();
                src.try_reserve_exact(src_len).map_err(|_| {
                    self.ctx.JS_ThrowOutOfMemory();
                    -1
                })?;
                src.extend_from_slice(std::slice::from_raw_parts(class_start_ptr, src_len));
                (*ctor_fd).source.replace(String::from_utf8_unchecked(src));
            } else {
                (*ctor_fd).source.take();
            }
        }

        // consume the '}'
        self.next_token()?;

        Ok(define_class_offset)
    }
}

pub unsafe fn js_parse_class(
    s: &mut JSParseState,
    is_class_expr: bool,
    export_flag: JSParseExportEnum,
) -> i32 {
    let saved_js_mode = {
        let fd = s.cur_func_mut();
        let mode0 = fd.js_mode;
        // classes are parsed and executed in strict mode
        fd.js_mode |= JS_MODE_STRICT as u8;
        mode0
    };

    if s.next_token().is_err() {
        s.cur_func_mut().js_mode = saved_js_mode;
        return -1;
    }

    // parse class name
    let class_name = if let Ok(a) = s.parse_class_name(is_class_expr, export_flag) {
        a
    } else {
        s.cur_func_mut().js_mode = saved_js_mode;
        return -1;
    };
    let class_var_name = if !is_class_expr {
        if class_name == JS_ATOM_NULL {
            JS_ATOM__default_ // export default
        } else {
            class_name
        }
        .dup(&s.ctx.rt())
    } else {
        JS_ATOM_NULL
    };

    s.push_scope();

    let mut name = JS_ATOM_NULL;

    macro_rules! fail {
        () => {
            JS_FreeAtom(&s.ctx, name);
            JS_FreeAtom(&s.ctx, class_name);
            JS_FreeAtom(&s.ctx, class_var_name);
            s.cur_func_mut().js_mode = saved_js_mode;
            return -1;
        };
    }

    // parse class flags: extends.
    let mut class_flags: i32 = 0;
    if s.token.val == TOK_EXTENDS {
        class_flags = JS_DEFINE_CLASS_HAS_HERITAGE;
        if s.next_token().is_err() {
            fail!();
        }
        if s.parse_left_hand_side_expr().is_err() {
            fail!();
        }
    } else {
        emit_op(s, OpCode::UNDEFINED);
    }

    // add a 'const' definition for the class name
    if class_name != JS_ATOM_NULL {
        let class_name_var_idx = define_var(
            s,
            s.cur_func_mut(),
            class_name,
            JSVarDefEnum::JS_VAR_DEF_CONST,
        );
        if class_name_var_idx < 0 {
            fail!();
        }
    }

    if s.parse_expect('{' as i32).is_err() {
        fail!();
    }

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

    let define_class_offset = if let Ok(n) =
        s.parse_class_body(class_name, class_var_name, class_flags, &mut class_fields)
    {
        n
    } else {
        fail!();
    };

    // // start of class body

    // let mut method_fd = std::ptr::null_mut::<JSFunctionDef>();
    // let mut prop_type: i32;

    // let define_class_offset: i32;
    // let mut is_static: bool;
    // let mut is_private: bool;
    // let class_start_ptr: *const u8 = s.token.ptr;
    // let mut start_ptr: *const u8;
    // let mut class_fields = [ClassFieldsDef::default(); 2];

    // // this scope contains the private fields
    // s.push_scope();

    // emit_op(s, OpCode::PUSH_CONST);
    // let ctor_cpool_offset = s.cur_func_mut().byte_code.len();
    // emit_u32(s, 0); // will be patched at the end of the class parsing

    // emit_op(s, OpCode::DEFINE_CLASS);
    // emit_atom_dup(
    //     s,
    //     if class_name == JS_ATOM_NULL {
    //         if class_var_name != JS_ATOM_NULL {
    //             JS_ATOM_default
    //         } else {
    //             JS_ATOM_empty_string
    //         }
    //     } else {
    //         class_name
    //     },
    // );
    // emit_u8(s, class_flags as u8);
    // define_class_offset = s.cur_func_mut().last_opcode_pos;

    // for i in 0..2 {
    //     let cf = &mut class_fields[i as usize];
    //     cf.fields_init_fd = std::ptr::null_mut();
    //     cf.computed_fields_count = 0;
    //     cf.need_brand = false;
    //     cf.is_static = i != 0;
    // }

    // let mut ctor_fd: *mut JSFunctionDef = std::ptr::null_mut();
    // while s.token.val != '}' as i32 {
    //     if s.token.val == ';' as i32 {
    //         if s.next_token().is_err() {
    //             fail!();
    //         }
    //         continue;
    //     }
    //     is_static = s.token.val == TOK_STATIC;
    //     prop_type = -1;
    //     if is_static {
    //         if s.next_token().is_err() {
    //             fail!();
    //         }
    //         if s.token.val == '{' as i32 {
    //             let cf = &mut class_fields[is_static as usize];
    //             let mut init = std::ptr::null_mut::<JSFunctionDef>();

    //             if cf.fields_init_fd.is_null() {
    //                 if emit_class_init_start(s, cf).is_err() {
    //                     fail!();
    //                 }
    //             }

    //             s.cur_func_ptr = (*cf).fields_init_fd;

    //             // XXX: could try to avoid creating a new function and
    //             // reuse 'fields_init_fd' with a specific 'var'
    //             // scope
    //             // stack is now: <empty>
    //             if s.parse_function_decl2(
    //                 JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT,
    //                 JSFunctionKindEnum::JS_FUNC_NORMAL,
    //                 JS_ATOM_NULL,
    //                 (*s).token.ptr,
    //                 (*s).token.line_num,
    //                 JSParseExportEnum::JS_PARSE_EXPORT_NONE,
    //                 std::ptr::addr_of_mut!(init),
    //             )
    //             .is_err()
    //             {
    //                 fail!();
    //             }
    //             // stack is now: fclosure
    //             s.push_scope();
    //             emit_op_temp(s, OpCodeTemp::SCOPE_GET_VAR);
    //             emit_atom_dup(s, JS_ATOM_this);
    //             emit_u16(s, 0);
    //             // stack is now: fclosure this
    //             emit_op(s, OpCode::SWAP);
    //             // stack is now: this fclosure
    //             emit_op(s, OpCode::CALL_METHOD);
    //             emit_u16(s, 0);
    //             // stack is now: returnvalue
    //             emit_op(s, OpCode::DROP);
    //             // stack is now: <empty>
    //             s.pop_scope();
    //             s.cur_func_ptr = s.cur_func().parent;

    //             continue;
    //         }
    //         // allow "static" field name
    //         if s.token.val == ';' as i32 || s.token.val == '=' as i32 {
    //             is_static = false;
    //             name = JS_ATOM_static.dup(&s.ctx.rt());
    //             prop_type = PROP_TYPE_IDENT;
    //         }
    //     }
    //     if is_static {
    //         emit_op(s, OpCode::SWAP);
    //     }
    //     start_ptr = s.token.ptr;
    //     if prop_type < 0 {
    //         prop_type = js_parse_property_name(s, &mut name, true, false, true);
    //         if prop_type < 0 {
    //             fail!();
    //         }
    //     }
    //     is_private = prop_type & PROP_TYPE_PRIVATE != 0;
    //     prop_type &= !PROP_TYPE_PRIVATE;

    //     if (name == JS_ATOM_constructor && !is_static && prop_type != PROP_TYPE_IDENT)
    //         || (name == JS_ATOM_prototype && is_static)
    //         || name == JS_ATOM_hash_constructor
    //     {
    //         s.throw_syntax_error(c"invalid method name");
    //         fail!();
    //     }
    //     if prop_type == PROP_TYPE_GET || prop_type == PROP_TYPE_SET {
    //         let is_set: bool = prop_type - PROP_TYPE_GET != 0;

    //         if is_private {
    //             let fd = s.cur_func_mut();
    //             let idx = find_private_class_field(&s.ctx, fd, name, fd.scope_level);
    //             if idx >= 0 {
    //                 let vd = fd.vars.get_mut(idx as usize).unwrap();
    //                 let var_kind = vd.var_kind();

    //                 if matches!(
    //                     var_kind,
    //                     JSVarKindEnum::JS_VAR_PRIVATE_FIELD
    //                         | JSVarKindEnum::JS_VAR_PRIVATE_METHOD
    //                         | JSVarKindEnum::JS_VAR_PRIVATE_GETTER_SETTER
    //                 ) || var_kind
    //                     == if is_set {
    //                         JSVarKindEnum::JS_VAR_PRIVATE_SETTER
    //                     } else {
    //                         JSVarKindEnum::JS_VAR_PRIVATE_GETTER
    //                     }
    //                     || (var_kind
    //                         == if is_set {
    //                             JSVarKindEnum::JS_VAR_PRIVATE_GETTER
    //                         } else {
    //                             JSVarKindEnum::JS_VAR_PRIVATE_SETTER
    //                         }
    //                         && is_static != vd.is_static_private())
    //                 {
    //                     private_field_already_defined!();
    //                 }

    //                 vd.set_var_kind(JSVarKindEnum::JS_VAR_PRIVATE_GETTER_SETTER);
    //             } else {
    //                 if add_private_class_field(
    //                     s,
    //                     fd,
    //                     name,
    //                     if is_set {
    //                         JSVarKindEnum::JS_VAR_PRIVATE_SETTER
    //                     } else {
    //                         JSVarKindEnum::JS_VAR_PRIVATE_GETTER
    //                     },
    //                     is_static,
    //                 ) < 0
    //                 {
    //                     fail!();
    //                 }
    //             }
    //             class_fields[is_static as usize].need_brand = true;
    //         }

    //         if s.parse_function_decl2(
    //             if is_set {
    //                 JSParseFunctionEnum::JS_PARSE_FUNC_SETTER
    //             } else {
    //                 JSParseFunctionEnum::JS_PARSE_FUNC_GETTER
    //             },
    //             JSFunctionKindEnum::JS_FUNC_NORMAL,
    //             JS_ATOM_NULL,
    //             start_ptr,
    //             (*s).token.line_num,
    //             JSParseExportEnum::JS_PARSE_EXPORT_NONE,
    //             &mut method_fd,
    //         )
    //         .is_err()
    //         {
    //             fail!();
    //         }

    //         if is_private {
    //             (*method_fd).need_home_object = true; // needed for brand check
    //             emit_op(s, OpCode::SET_HOME_OBJECT);
    //             // XXX: missing function name
    //             emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
    //             if is_set {
    //                 let setter_name = get_private_setter_name(&s.ctx, name);
    //                 if setter_name == JS_ATOM_NULL {
    //                     fail!();
    //                 }
    //                 emit_atom_dup(s, setter_name);
    //                 let ret = add_private_class_field(
    //                     s,
    //                     s.cur_func_mut(),
    //                     setter_name,
    //                     JSVarKindEnum::JS_VAR_PRIVATE_SETTER,
    //                     is_static,
    //                 );
    //                 JS_FreeAtom(&s.ctx, setter_name);
    //                 if ret < 0 {
    //                     fail!();
    //                 }
    //             } else {
    //                 emit_atom_dup(s, name);
    //             }
    //             emit_u16(s, s.cur_func().scope_level as _);
    //         } else {
    //             if name == JS_ATOM_NULL {
    //                 emit_op(s, OpCode::DEFINE_METHOD_COMPUTED);
    //             } else {
    //                 emit_op(s, OpCode::DEFINE_METHOD);
    //                 emit_atom_dup(s, name);
    //             }
    //             emit_u8(
    //                 s,
    //                 if is_set {
    //                     OP_DEFINE_METHOD_SETTER
    //                 } else {
    //                     OP_DEFINE_METHOD_GETTER
    //                 },
    //             );
    //         }
    //     } else if prop_type == PROP_TYPE_IDENT && s.token.val != '(' as i32 {
    //         let cf = &mut class_fields[is_static as usize];
    //         let mut field_var_name: JSAtom = JS_ATOM_NULL;

    //         // class field

    //         // XXX: spec: not consistent with method name checks
    //         if name == JS_ATOM_constructor || name == JS_ATOM_prototype {
    //             s.throw_syntax_error(c"invalid field name");
    //             fail!();
    //         }

    //         if is_private {
    //             if find_private_class_field(&s.ctx, fd, name, fd.scope_level) >= 0 {
    //                 private_field_already_defined!();
    //             }
    //             if add_private_class_field(
    //                 s,
    //                 fd,
    //                 name,
    //                 JSVarKindEnum::JS_VAR_PRIVATE_FIELD,
    //                 is_static,
    //             ) < 0
    //             {
    //                 fail!();
    //             }
    //             emit_op(s, OpCode::PRIVATE_SYMBOL);
    //             emit_atom_dup(s, name);
    //             emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
    //             emit_atom_dup(s, name);
    //             emit_u16(s, (*s).cur_func().scope_level as _);
    //         }

    //         if cf.fields_init_fd.is_null() {
    //             if emit_class_init_start(s, cf).is_err() {
    //                 fail!();
    //             }
    //         }
    //         if name == JS_ATOM_NULL {
    //             // save the computed field name into a variable
    //             field_var_name = js_atom_concat_num(
    //                 &s.ctx,
    //                 if is_static {
    //                     JS_ATOM_static_computed_field
    //                 } else {
    //                     JS_ATOM_computed_field
    //                 },
    //                 (*cf).computed_fields_count as u32,
    //             );
    //             if field_var_name == JS_ATOM_NULL {
    //                 fail!();
    //             }
    //             if define_var(s, fd, field_var_name, JSVarDefEnum::JS_VAR_DEF_CONST) < 0 {
    //                 JS_FreeAtom(&s.ctx, field_var_name);
    //                 fail!();
    //             }
    //             emit_op(s, OpCode::TO_PROPKEY);
    //             emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
    //             emit_atom_dup(s, field_var_name);
    //             emit_u16(s, s.cur_func().scope_level as _);
    //         }
    //         s.cur_func_ptr = (*cf).fields_init_fd;
    //         emit_op_temp(s, OpCodeTemp::SCOPE_GET_VAR);
    //         emit_atom_dup(s, JS_ATOM_this);
    //         emit_u16(s, 0);

    //         if name == JS_ATOM_NULL {
    //             emit_op_temp(s, OpCodeTemp::SCOPE_GET_VAR);
    //             emit_atom_dup(s, field_var_name);
    //             emit_u16(s, s.cur_func().scope_level as _);
    //             cf.computed_fields_count += 1;
    //             JS_FreeAtom(&s.ctx, field_var_name);
    //         } else if is_private {
    //             emit_op_temp(s, OpCodeTemp::SCOPE_GET_VAR);
    //             emit_atom_dup(s, name);
    //             emit_u16(s, s.cur_func().scope_level as _);
    //         }

    //         if s.token.val == '=' as i32 {
    //             if s.next_token().is_err() {
    //                 fail!();
    //             }
    //             if s.parse_assign_expr().is_err() {
    //                 fail!();
    //             }
    //         } else {
    //             emit_op(s, OpCode::UNDEFINED);
    //         }
    //         if is_private {
    //             set_object_name_computed(s);
    //             emit_op(s, OpCode::DEFINE_PRIVATE_FIELD);
    //         } else if name == JS_ATOM_NULL {
    //             set_object_name_computed(s);
    //             emit_op(s, OpCode::DEFINE_ARRAY_EL);
    //             emit_op(s, OpCode::DROP);
    //         } else {
    //             set_object_name(s, name);
    //             emit_op(s, OpCode::DEFINE_FIELD);
    //             emit_atom_dup(s, name);
    //         }
    //         s.cur_func_ptr = s.cur_func().parent;
    //         if s.parse_expect_semi().is_err() {
    //             fail!();
    //         }
    //     } else {
    //         let mut func_type = JSParseFunctionEnum::JS_PARSE_FUNC_METHOD;
    //         let mut func_kind = JSFunctionKindEnum::JS_FUNC_NORMAL;

    //         if prop_type == PROP_TYPE_STAR {
    //             func_kind = JSFunctionKindEnum::JS_FUNC_GENERATOR;
    //         } else if prop_type == PROP_TYPE_ASYNC {
    //             func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
    //         } else if prop_type == PROP_TYPE_ASYNC_STAR {
    //             func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR;
    //         } else if name == JS_ATOM_constructor && !is_static {
    //             if !ctor_fd.is_null() {
    //                 s.throw_syntax_error(c"property constructor appears more than once");
    //                 fail!();
    //             }
    //             func_type = if class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0 {
    //                 JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
    //             } else {
    //                 JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
    //             };
    //         }
    //         if is_private {
    //             class_fields[is_static as usize].need_brand = true;
    //         }
    //         if s.parse_function_decl2(
    //             func_type,
    //             func_kind,
    //             JS_ATOM_NULL,
    //             start_ptr,
    //             s.token.line_num,
    //             JSParseExportEnum::JS_PARSE_EXPORT_NONE,
    //             &mut method_fd,
    //         )
    //         .is_err()
    //         {
    //             fail!();
    //         }
    //         if func_type == JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR
    //             || func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR
    //         {
    //             ctor_fd = method_fd;
    //         } else if is_private {
    //             (*method_fd).need_home_object = true; // needed for brand check
    //             if find_private_class_field(&s.ctx, fd, name, fd.scope_level) >= 0 {
    //                 private_field_already_defined!();
    //             }
    //             if add_private_class_field(
    //                 s,
    //                 fd,
    //                 name,
    //                 JSVarKindEnum::JS_VAR_PRIVATE_METHOD,
    //                 is_static,
    //             ) < 0
    //             {
    //                 fail!();
    //             }
    //             emit_op(s, OpCode::SET_HOME_OBJECT);
    //             emit_op(s, OpCode::SET_NAME);
    //             emit_atom_dup(s, name);
    //             emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
    //             emit_atom_dup(s, name);
    //             emit_u16(s, (*s).cur_func().scope_level as _);
    //         } else {
    //             if name == JS_ATOM_NULL {
    //                 emit_op(s, OpCode::DEFINE_METHOD_COMPUTED);
    //             } else {
    //                 emit_op(s, OpCode::DEFINE_METHOD);
    //                 emit_atom_dup(s, name);
    //             }
    //             emit_u8(s, OP_DEFINE_METHOD_METHOD);
    //         }
    //     }
    //     if is_static {
    //         emit_op(s, OpCode::SWAP);
    //     }
    //     JS_FreeAtom(&s.ctx, name);
    //     name = JS_ATOM_NULL;
    // }

    // if s.token.val != '}' as i32 {
    //     s.throw_syntax_error(c"expecting '}'");
    //     fail!();
    // }

    // if ctor_fd.is_null() {
    //     if js_parse_class_default_ctor(
    //         s,
    //         class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0,
    //         &mut ctor_fd,
    //     ) != 0
    //     {
    //         fail!();
    //     }
    // }
    // // patch the constant pool index for the constructor
    // write_to_ptr::<u32>(
    //     fd.byte_code.as_mut_ptr().add(ctor_cpool_offset),
    //     (*ctor_fd).parent_cpool_idx as u32,
    // );

    // // store the class source code in the constructor.
    // if fd.js_mode & JS_MODE_STRIP as u8 == 0 {
    //     debug_assert!(s.buf_ptr >= class_start_ptr);

    //     let src_len = s.buf_ptr.offset_from(class_start_ptr) as usize;
    //     if src_len > 0 {
    //         let mut src = Vec::<u8>::new();
    //         if src.try_reserve_exact(src_len).is_err() {
    //             &s.ctx.JS_ThrowOutOfMemory();
    //             fail!();
    //         }
    //         src.extend_from_slice(std::slice::from_raw_parts(class_start_ptr, src_len));
    //         (*ctor_fd).source.replace(String::from_utf8_unchecked(src));
    //     } else {
    //         (*ctor_fd).source.take();
    //     }
    // }

    // // consume the '}'
    // if s.next_token().is_err() {
    //     fail!();
    // }

    // out of class body

    {
        let cf = &mut class_fields[0];

        if cf.need_brand {
            // add a private brand to the prototype
            emit_op(s, OpCode::DUP);
            emit_op(s, OpCode::NULL);
            emit_op(s, OpCode::SWAP);
            emit_op(s, OpCode::ADD_BRAND);

            // define the brand field in 'this' of the initializer
            if cf.fields_init_fd.is_null() {
                if emit_class_init_start(s, cf).is_err() {
                    fail!();
                }
            }
            // patch the start of the function to enable the OP_add_brand_instance code
            (*cf.fields_init_fd).byte_code[cf.brand_push_pos as usize] = OpCode::PUSH_TRUE as u8;
        }

        // store the function to initialize the fields...
        let var_idx = define_var(
            s,
            s.cur_func_ptr,
            JS_ATOM_class_fields_init,
            JSVarDefEnum::JS_VAR_DEF_CONST,
        );
        if var_idx < 0 {
            fail!();
        }
        if !cf.fields_init_fd.is_null() {
            emit_class_init_end(s, cf);
        } else {
            emit_op(s, OpCode::UNDEFINED);
        }
        emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
        emit_atom_dup(s, JS_ATOM_class_fields_init);
        emit_u16(s, (*s).cur_func().scope_level as _);
    }

    // drop the prototype
    emit_op(s, OpCode::DROP);

    if class_fields[1].need_brand {
        // add a private brand to the class
        emit_op(s, OpCode::DUP);
        emit_op(s, OpCode::DUP);
        emit_op(s, OpCode::ADD_BRAND);
    }

    if class_name != JS_ATOM_NULL {
        /* store the class name in the scoped class name variable (it
        is independent from the class statement variable
        definition) */
        emit_op(s, OpCode::DUP);
        emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
        emit_atom_dup(s, class_name);
        emit_u16(s, s.cur_func().scope_level as _);
    }

    /* initialize the static fields */
    if !class_fields[1].fields_init_fd.is_null() {
        let cf = &class_fields[1];
        emit_op(s, OpCode::DUP);
        emit_class_init_end(s, cf);
        emit_op(s, OpCode::CALL_METHOD);
        emit_u16(s, 0);
        emit_op(s, OpCode::DROP);
    }

    s.pop_scope();
    s.pop_scope();

    /* the class statements have a block level scope */
    if class_var_name != JS_ATOM_NULL {
        if define_var(
            s,
            s.cur_func_ptr,
            class_var_name,
            JSVarDefEnum::JS_VAR_DEF_LET,
        ) < 0
        {
            fail!();
        }
        emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR_INIT);
        emit_atom_dup(s, class_var_name);
        emit_u16(s, s.cur_func().scope_level as _);
    } else {
        if class_name == JS_ATOM_NULL {
            /* cannot use OP_set_name because the name of the class
            must be defined before the static initializers are
            executed */
            emit_op_temp(s, OpCodeTemp::SET_CLASS_NAME);
            emit_u32(
                s,
                s.cur_func().last_opcode_pos as u32 + 1 - define_class_offset as u32,
            );
        }
    }

    if export_flag != JSParseExportEnum::JS_PARSE_EXPORT_NONE {
        let md = s.cur_func().module.clone().unwrap();
        if add_export_entry(
            s,
            &md,
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
        )
        .is_err()
        {
            fail!();
        }
    }

    // Cleanup
    JS_FreeAtom(&s.ctx, class_name);
    JS_FreeAtom(&s.ctx, class_var_name);
    s.cur_func_mut().js_mode = saved_js_mode;

    return 0;
}

// if the property is an expression, name = JS_ATOM_NULL
pub unsafe fn js_parse_property_name(
    s: *mut JSParseState,
    pname: *mut JSAtom,
    allow_method: bool,
    allow_var: bool,
    allow_private: bool,
) -> i32 {
    let ctx = &(*s).ctx;
    let mut name: JSAtom = JS_ATOM_NULL;
    let mut prop_type = PROP_TYPE_IDENT;
    let mut private = 0_i32;

    enum GotoLabel {
        IdentFound(bool),
        InvalidProp,
        Fail(JSAtom),
    }

    let mut goto: Option<GotoLabel> = loop {
        if allow_method {
            if token_is_pseudo_keyword(s, JS_ATOM_get) || token_is_pseudo_keyword(s, JS_ATOM_set) {
                /* get x(), set x() */
                name = (*s).token.data.ident_atom().unwrap().dup(&(*s).ctx.rt());
                if (*s).next_token().is_err() {
                    break Some(GotoLabel::Fail(name));
                }
                if (*s).token.val == ':' as i32
                    || (*s).token.val == ',' as i32
                    || (*s).token.val == '}' as i32
                    || (*s).token.val == '(' as i32
                {
                    break Some(GotoLabel::IdentFound(true));
                }

                prop_type = if name == JS_ATOM_get {
                    PROP_TYPE_GET
                } else {
                    PROP_TYPE_SET
                };
                JS_FreeAtom(ctx, name);
            } else if (*s).token.val == '*' as i32 {
                if (*s).next_token().is_err() {
                    break Some(GotoLabel::Fail(JS_ATOM_NULL));
                }
                prop_type = PROP_TYPE_STAR;
            } else if token_is_pseudo_keyword(s, JS_ATOM_async)
                && peek_token(s, true) != '\n' as i32
            {
                name = (*s).token.data.ident_atom().unwrap().dup(&(*s).ctx.rt());
                if (*s).next_token().is_err() {
                    break Some(GotoLabel::Fail(name));
                }

                if (*s).token.val == ':' as i32
                    || (*s).token.val == ',' as i32
                    || (*s).token.val == '}' as i32
                    || (*s).token.val == '(' as i32
                {
                    break Some(GotoLabel::IdentFound(true));
                }

                JS_FreeAtom(ctx, name);
                if (*s).token.val == '*' as i32 {
                    if (*s).next_token().is_err() {
                        break Some(GotoLabel::Fail(JS_ATOM_NULL));
                    }
                    prop_type = PROP_TYPE_ASYNC_STAR;
                } else {
                    prop_type = PROP_TYPE_ASYNC;
                }
            }
        }

        match (*s).token.val {
            TOK_STRING => {
                name = JS_ValueToAtom(ctx, (*s).token.data.string().unwrap());
                if name == JS_ATOM_NULL {
                    break Some(GotoLabel::Fail(JS_ATOM_NULL));
                }
                if (*s).next_token().is_err() {
                    break Some(GotoLabel::Fail(name));
                }
            }
            TOK_NUMBER => {
                // JSValue int32 or float64
                name = JS_ValueToAtom(ctx, &(*s).token.data.number().unwrap());
                if name == JS_ATOM_NULL {
                    break Some(GotoLabel::Fail(JS_ATOM_NULL));
                }
                if (*s).next_token().is_err() {
                    break Some(GotoLabel::Fail(name));
                }
            }
            // '['
            CHAR_SQUARE_L => {
                if (*s).next_token().is_err() {
                    break Some(GotoLabel::Fail(JS_ATOM_NULL));
                }
                if (*s).parse_expr().is_err() {
                    break Some(GotoLabel::Fail(JS_ATOM_NULL));
                }
                if (*s).parse_expect(']' as i32).is_err() {
                    break Some(GotoLabel::Fail(JS_ATOM_NULL));
                }
                name = JS_ATOM_NULL;
            }
            TOK_PRIVATE_NAME if allow_private => {
                name = (*s).token.data.ident_atom().unwrap().dup(&(*s).ctx.rt());
                if (*s).next_token().is_err() {
                    break Some(GotoLabel::Fail(name));
                }
                private = PROP_TYPE_PRIVATE;
            }
            t if token_is_ident(t) => {
                /* variable can only be a non-reserved identifier */
                let is_non_reserved_ident =
                    (*s).token.val == TOK_IDENT && !(*s).token.data.is_reserved();
                /* keywords and reserved words have a valid atom */
                name = (*s).token.data.ident_atom().unwrap().dup(&(*s).ctx.rt());
                if (*s).next_token().is_err() {
                    break Some(GotoLabel::Fail(name));
                }

                break Some(GotoLabel::IdentFound(is_non_reserved_ident));
            }
            _ => {
                break Some(GotoLabel::InvalidProp);
            }
        }

        break None;
    };

    if let Some(l) = goto.take() {
        match l {
            GotoLabel::IdentFound(is_non_reserved_ident) => {
                if is_non_reserved_ident && prop_type == PROP_TYPE_IDENT && allow_var {
                    if !((*s).token.val == ':' as i32
                        || ((*s).token.val == '(' as i32 && allow_method))
                    {
                        prop_type = PROP_TYPE_VAR;
                    }
                }
            }
            GotoLabel::InvalidProp => {
                JS_FreeAtom(ctx, name);
                (*s).throw_syntax_error("invalid property name");
                *pname = JS_ATOM_NULL;
                return -1;
            }
            GotoLabel::Fail(atom) => {
                if atom != JS_ATOM_NULL {
                    JS_FreeAtom(ctx, atom);
                }
                *pname = JS_ATOM_NULL;
                return -1;
            }
        }
    }

    if !matches!(prop_type, PROP_TYPE_IDENT | PROP_TYPE_VAR) && (*s).token.val != '(' as i32 {
        JS_FreeAtom(ctx, name);
        (*s).throw_syntax_error("invalid property name");
        *pname = JS_ATOM_NULL;
        -1
    } else {
        *pname = name;
        prop_type | private
    }
}

// create a function to init}ialize class fields
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

pub unsafe fn emit_class_init_start(
    s: *mut JSParseState,
    cf: *mut ClassFieldsDef,
) -> Result<(), i32> {
    let label_add_brand: i32;

    (*cf).fields_init_fd = Box::into_raw(js_parse_function_class_fields_init(s)?);

    (*s).cur_func_ptr = (*cf).fields_init_fd;

    if !(*cf).is_static {
        // add the brand to the newly created instance
        // XXX: would be better to add the code only if needed, maybe in a
        // later pass
        emit_op(s, OpCode::PUSH_FALSE); // will be patched later
        (*cf).brand_push_pos = (*(*cf).fields_init_fd).last_opcode_pos;
        label_add_brand = emit_goto(s, OpCode::IF_FALSE, -1);

        emit_op_temp(s, OpCodeTemp::SCOPE_GET_VAR);
        emit_atom_dup(s, JS_ATOM_this);
        emit_u16(s, 0);

        emit_op_temp(s, OpCodeTemp::SCOPE_GET_VAR);
        emit_atom_dup(s, JS_ATOM__home_object_);
        emit_u16(s, 0);

        emit_op(s, OpCode::ADD_BRAND);

        emit_label(s, label_add_brand);
    }
    (*s).cur_func_ptr = (*s).cur_func().parent;

    Ok(())
}

pub unsafe fn emit_class_init_end(s: *mut JSParseState, cf: *const ClassFieldsDef) {
    (*s).cur_func_ptr = (*cf).fields_init_fd;
    emit_op(s, OpCode::RETURN_UNDEF);
    (*s).cur_func_ptr = (*s).cur_func().parent;

    let cpool_idx = (*s)
        .cur_func_mut()
        .cpool_add(JSValue::NULL)
        .map_or(-1, |x| x as i32);

    (*(*cf).fields_init_fd).parent_cpool_idx = cpool_idx;
    emit_op(s, OpCode::FCLOSURE);
    emit_u32(s, cpool_idx as _);

    emit_op(s, OpCode::SET_HOME_OBJECT);
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

    let vd = (*fd).vars.get_mut(idx as usize).unwrap();
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
    let mut idx = (*fd).scopes[scope_level as usize].first;
    while idx != -1 {
        let vd = (*fd).vars.get(idx as usize).unwrap();
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
pub unsafe fn get_private_setter_name(ctx: &JSContext, name: JSAtom) -> JSAtom {
    js_atom_concat_str(ctx, name, c"<set>".as_ptr().cast())
}

// XXX: could generate specific bytecode
pub unsafe fn js_parse_class_default_ctor(
    s: *mut JSParseState,
    has_super: bool,
    pfd: *mut *mut JSFunctionDef,
) -> i32 {
    let pos = (*s).get_pos();

    let (str, func_type) = if has_super {
        // spec change: no argument evaluation
        (
            "(){super(...arguments);}\0",
            JSParseFunctionEnum::JS_PARSE_FUNC_DERIVED_CLASS_CONSTRUCTOR,
        )
    } else {
        (
            "(){}\0",
            JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_CONSTRUCTOR,
        )
    };

    let line_num = (*s).token.line_num;
    let saved_buf_end = (*s).buf_end;

    (*s).buf_ptr = str.as_ptr();
    (*s).buf_end = str
        .as_ptr()
        .add(CStr::from_ptr(str.as_ptr().cast()).count_bytes());

    // next_token -> parse_function_decl2 -> seek_token
    let mut ret = (*s)
        .next_token()
        .and_then(|_| {
            (*s).parse_function_decl2(
                func_type,
                JSFunctionKindEnum::JS_FUNC_NORMAL,
                JS_ATOM_NULL,
                str.as_ptr(),
                line_num,
                JSParseExportEnum::JS_PARSE_EXPORT_NONE,
                pfd,
            )
        })
        .map_or(-1, |_| 0);

    (*s).buf_end = saved_buf_end;

    ret |= if (*s).seek_token(&pos).is_err() {
        -1
    } else {
        0
    };

    ret
}
