use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, consts::JS_ATOM___proto__},
    bytecode::{OpCode, OpCodeTemp},
    class::{OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD},
    function::JSFunctionKindEnum,
    parse::{
        JSParseFunctionEnum,
        class::js_parse_property_name,
        obj::{set_object_name, set_object_name_computed},
        token::{CHAR_BRACKET_L, CHAR_COLON, CHAR_CURLY_R, TOK_ELLIPSIS},
    },
    value::object::{
        PROP_TYPE_ASYNC, PROP_TYPE_ASYNC_STAR, PROP_TYPE_GET, PROP_TYPE_SET, PROP_TYPE_STAR,
        PROP_TYPE_VAR,
    },
};

use super::JSParseState;

impl<'s> JSParseState<'s> {
    pub unsafe fn parse_object_literal(&mut self) -> Result<(), i32> {
        self.next_token()?;

        // XXX: add an initial length that will be patched back
        self.emit_op(OpCode::OBJECT);

        let mut has_proto = false;

        while self.token.val != '}' as i32 {
            // specific case for getter/setter
            let start_ptr = self.token.ptr;
            let start_line = self.token.line_num;

            if self.token.val == TOK_ELLIPSIS {
                self.next_token()?;
                self.parse_assign_expr()?;

                self.emit_op(OpCode::NULL); // dummy excludeList
                self.emit_op(OpCode::COPY_DATA_PROPERTIES);
                self.emit_u8(2 | (1 << 2));
                self.emit_op(OpCode::DROP); // pop excludeList
                self.emit_op(OpCode::DROP); // pop src object

                // next
                if self.token.val != ',' as i32 {
                    break;
                }
                self.next_token()?;
            }

            let mut name: JSAtom = JS_ATOM_NULL;
            let prop_type = js_parse_property_name(self, &mut name, true, true, false);
            if prop_type < 0 {
                JS_FreeAtom(&self.ctx, name);
                return Err(-1);
            };

            if prop_type == PROP_TYPE_VAR {
                // shortcut for x: x
                self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                self.emit_atom_dup(name);
                self.emit_u16(self.cur_func().scope_level as _);
                self.emit_op(OpCode::DEFINE_FIELD);
                self.emit_atom_dup(name);
            } else if self.token.val == CHAR_BRACKET_L {
                let is_getset = matches!(prop_type, PROP_TYPE_GET | PROP_TYPE_SET);
                let (func_type, func_kind, op_flags): (
                    JSParseFunctionEnum,
                    JSFunctionKindEnum,
                    i32,
                ) = if is_getset {
                    (
                        // type
                        if prop_type == PROP_TYPE_GET {
                            JSParseFunctionEnum::JS_PARSE_FUNC_GETTER
                        } else {
                            JSParseFunctionEnum::JS_PARSE_FUNC_SETTER
                        },
                        // kind
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        // opflags
                        OP_DEFINE_METHOD_GETTER as i32 + prop_type - PROP_TYPE_GET,
                    )
                } else {
                    (
                        // type
                        JSParseFunctionEnum::JS_PARSE_FUNC_METHOD,
                        // kind
                        if prop_type == PROP_TYPE_STAR {
                            JSFunctionKindEnum::JS_FUNC_GENERATOR
                        } else if prop_type == PROP_TYPE_ASYNC {
                            JSFunctionKindEnum::JS_FUNC_ASYNC
                        } else if prop_type == PROP_TYPE_ASYNC_STAR {
                            JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
                        } else {
                            JSFunctionKindEnum::JS_FUNC_NORMAL
                        },
                        // opflags
                        OP_DEFINE_METHOD_METHOD as i32,
                    )
                };

                if self
                    .parse_function_decl(func_type, func_kind, JS_ATOM_NULL, start_ptr, start_line)
                    .is_err()
                {
                    JS_FreeAtom(&self.ctx, name);
                    return Err(-1);
                }

                if name == JS_ATOM_NULL {
                    self.emit_op(OpCode::DEFINE_METHOD_COMPUTED);
                } else {
                    self.emit_op(OpCode::DEFINE_METHOD);
                    self.emit_atom_dup(name);
                }
                self.emit_u8(op_flags as u8 | OP_DEFINE_METHOD_ENUMERABLE);
            } else {
                if self.parse_expect(CHAR_COLON).is_err() {
                    JS_FreeAtom(&self.ctx, name);
                    return Err(-1);
                }
                if self.parse_assign_expr().is_err() {
                    JS_FreeAtom(&self.ctx, name);
                    return Err(-1);
                }

                if name == JS_ATOM_NULL {
                    set_object_name_computed(self);
                    self.emit_op(OpCode::DEFINE_ARRAY_EL);
                    self.emit_op(OpCode::DROP);
                } else if name == JS_ATOM___proto__ {
                    if has_proto {
                        self.throw_syntax_error("duplicate __proto__ property name");
                        JS_FreeAtom(&self.ctx, name);
                        return Err(-1);
                    }
                    self.emit_op(OpCode::SET_PROTO);
                    has_proto = true;
                } else {
                    set_object_name(self, name);
                    self.emit_op(OpCode::DEFINE_FIELD);
                    self.emit_atom_dup(name);
                }
            }

            JS_FreeAtom(&self.ctx, name);

            // next
            if self.token.val != ',' as i32 {
                break;
            }
            self.next_token()?;
        } // while

        self.parse_expect(CHAR_CURLY_R)
    }
}
