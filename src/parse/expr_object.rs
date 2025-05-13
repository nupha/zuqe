use crate::{
    atom::{consts::JS_ATOM___proto__, JSAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{OpCode, OpCodeTemp},
    class::{OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD},
    function::JSFunctionKindEnum,
    parse::{
        class::js_parse_property_name,
        emit::{emit_atom_dup, emit_op, emit_op_temp, emit_u16, emit_u8},
        error::js_parse_error,
        obj::{set_object_name, set_object_name_computed},
        token::{CHAR_BRACKET_L, CHAR_COLON, CHAR_CURLY_R, TOK_ELLIPSIS},
        JSParseFunctionEnum,
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
        emit_op(self, OpCode::OBJECT);

        let mut has_proto = false;

        while self.token.val != '}' as i32 {
            // specific case for getter/setter
            let start_ptr = self.token.ptr;
            let start_line = self.token.line_num;

            if self.token.val == TOK_ELLIPSIS {
                self.next_token()?;
                self.parse_assign_expr()?;

                emit_op(self, OpCode::NULL); // dummy excludeList
                emit_op(self, OpCode::COPY_DATA_PROPERTIES);
                emit_u8(self, 2 | (1 << 2) | (0 << 5));
                emit_op(self, OpCode::DROP); // pop excludeList
                emit_op(self, OpCode::DROP); // pop src object

                // next
                if self.token.val != ',' as i32 {
                    break;
                }
                self.next_token()?;
            }

            let mut name: JSAtom = JS_ATOM_NULL;
            let prop_type = js_parse_property_name(self, &mut name, true, true, false);
            if prop_type < 0 {
                JS_FreeAtom(self.ctx.as_ptr(), name);
                return Err(-1);
            };

            if prop_type == PROP_TYPE_VAR {
                // shortcut for x: x
                emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                emit_atom_dup(self, name);
                emit_u16(self, self.cur_func.as_ref().unwrap().scope_level as _);
                emit_op(self, OpCode::DEFINE_FIELD);
                emit_atom_dup(self, name);
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
                    JS_FreeAtom(self.ctx.as_ptr(), name);
                    return Err(-1);
                }

                if name == JS_ATOM_NULL {
                    emit_op(self, OpCode::DEFINE_METHOD_COMPUTED);
                } else {
                    emit_op(self, OpCode::DEFINE_METHOD);
                    emit_atom_dup(self, name);
                }
                emit_u8(self, op_flags as u8 | OP_DEFINE_METHOD_ENUMERABLE);
            } else {
                if self.parse_expect(CHAR_COLON).is_err() {
                    JS_FreeAtom(self.ctx.as_ptr(), name);
                    return Err(-1);
                }
                if self.parse_assign_expr().is_err() {
                    JS_FreeAtom(self.ctx.as_ptr(), name);
                    return Err(-1);
                }

                if name == JS_ATOM_NULL {
                    set_object_name_computed(self);
                    emit_op(self, OpCode::DEFINE_ARRAY_EL);
                    emit_op(self, OpCode::DROP);
                } else if name == JS_ATOM___proto__ {
                    if has_proto {
                        js_parse_error(self, c"\"duplicate __proto__ property name\"");
                        JS_FreeAtom(self.ctx.as_ptr(), name);
                        return Err(-1);
                    }
                    emit_op(self, OpCode::SET_PROTO);
                    has_proto = true;
                } else {
                    set_object_name(self, name);
                    emit_op(self, OpCode::DEFINE_FIELD);
                    emit_atom_dup(self, name);
                }
            }

            JS_FreeAtom(self.ctx.as_ptr(), name);

            // next
            if self.token.val != ',' as i32 {
                break;
            }
            self.next_token()?;
        } // while

        self.parse_expect(CHAR_CURLY_R)
    }
}
