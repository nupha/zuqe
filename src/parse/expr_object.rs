use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp},
    class::{OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD},
    function::JSFunctionKindEnum,
    object::{
        PROP_TYPE_ASYNC, PROP_TYPE_ASYNC_STAR, PROP_TYPE_GET, PROP_TYPE_IDENT, PROP_TYPE_PRIVATE,
        PROP_TYPE_SET, PROP_TYPE_STAR, PROP_TYPE_VAR,
    },
    parse::{
        JSParseFunctionEnum,
        obj::set_object_name_computed,
        tokenize::{
            CHAR_BRACKET_L, CHAR_COLON, CHAR_COMMA, CHAR_CURLY_R, CHAR_SEMI, CHAR_SQUARE_L,
            TOK_ELLIPSIS, TOK_IDENT, TOK_NUMBER, TOK_PRIVATE_NAME, TOK_STRING,
        },
    },
    value::JSValue,
};

use super::JSParseState;

impl<'s> JSParseState<'s> {
    pub fn parse_object_literal(&mut self) -> Result<i32, i32> {
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
            } else {
                let (name, prop_type) = self.parse_property_name(true, true, false)?;

                if prop_type == PROP_TYPE_VAR {
                    // shortcut for x: x
                    self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                    self.emit_atom(name);
                    self.emit_u16(self.cur_func().scope_level as _);
                    self.emit_op(OpCode::DEFINE_FIELD);
                    self.emit_atom(name);
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

                    self.parse_function_decl(func_type, func_kind, start_ptr, start_line)?;

                    if name.is_null() {
                        self.emit_op(OpCode::DEFINE_METHOD_COMPUTED);
                    } else {
                        self.emit_op(OpCode::DEFINE_METHOD);
                        self.emit_atom(name);
                    }
                    self.emit_u8(op_flags as u8 | OP_DEFINE_METHOD_ENUMERABLE);
                } else {
                    self.parse_expect(CHAR_COLON)?;
                    self.parse_assign_expr()?;

                    if name.is_null() {
                        set_object_name_computed(self);
                        self.emit_op(OpCode::DEFINE_ARRAY_EL);
                        self.emit_op(OpCode::DROP);
                    } else if name == Atom::CONST___proto__ {
                        if has_proto {
                            self.throw_syntax_error("duplicate __proto__ property name");
                            return Err(-1);
                        }
                        self.emit_op(OpCode::SET_PROTO);
                        has_proto = true;
                    } else {
                        self.set_object_name(name);
                        self.emit_op(OpCode::DEFINE_FIELD);
                        self.emit_atom(name);
                    }
                }
            }

            // next
            if self.token.val != ',' as i32 {
                break;
            }
            self.next_token()?;
        } // while token is '}'

        self.parse_expect(CHAR_CURLY_R)
    }

    /// parse property name, returns name atom and property type.
    /// if the property is an expression, returned name will be JS_ATOM_NULL.
    /// returned atom is duplicated, caller must free it.
    pub(super) fn parse_property_name(
        &mut self,
        allow_method: bool,
        allow_var: bool,
        allow_private: bool,
    ) -> Result<(Atom, i32), i32> {
        let mut prop_type = PROP_TYPE_IDENT;
        let mut private = 0_i32;
        let mut is_non_reserved_ident = false;
        let mut name: Option<Atom> = None;

        if allow_method {
            if self.token_is_pseudo_keyword(Atom::CONST_get)
                || self.token_is_pseudo_keyword(Atom::CONST_set)
            {
                // found `get...` or `set...`
                name.replace(self.token.atom().unwrap());

                match self.next_token()? {
                    CHAR_COLON | CHAR_COMMA | CHAR_CURLY_R | CHAR_BRACKET_L | CHAR_SEMI => {
                        // ident found: a var named `get` or `set`
                        // case ':' - w/ init value
                        // case ';' - w/o init value
                        // ,}(
                        is_non_reserved_ident = true;
                    }
                    _ => {
                        prop_type = if name.take().unwrap() == Atom::CONST_get {
                            PROP_TYPE_GET
                        } else {
                            PROP_TYPE_SET
                        };
                    }
                }
            } else if self.token.val == '*' as i32 {
                self.next_token()?;
                prop_type = PROP_TYPE_STAR;
            } else if self.token_is_pseudo_keyword(Atom::CONST_async)
                && self.peek_token(true) != '\n' as i32
            {
                name.replace(self.token.atom().unwrap());

                self.next_token()?;

                if matches!(
                    self.token.val,
                    CHAR_COLON | CHAR_COMMA | CHAR_CURLY_R | CHAR_BRACKET_L
                ) {
                    // :,}(
                    is_non_reserved_ident = true;
                } else {
                    name.take();
                    prop_type = if self.token.val == '*' as i32 {
                        self.next_token()?;
                        PROP_TYPE_ASYNC_STAR
                    } else {
                        PROP_TYPE_ASYNC
                    };
                }
            }
        }

        if name.is_none() {
            match self.token.val {
                TOK_STRING => {
                    let sbuf = self.token.data.take_string().unwrap();
                    name.replace(
                        self.ctx
                            .create_string_with_buf(sbuf)
                            .and_then(|s| s.to_atom(self.ctx.rt_ptr()))?,
                    );
                    self.next_token()?;
                }
                TOK_NUMBER => {
                    name.replace(
                        JSValue::from(self.token.data.number().unwrap()).to_atom(&self.ctx)?,
                    );
                    self.next_token()?;
                }
                // '['
                CHAR_SQUARE_L => {
                    self.next_token()?;
                    self.parse_expr()?;
                    self.parse_expect(']' as i32)?;
                }
                TOK_PRIVATE_NAME if allow_private => {
                    name.replace(self.token.atom().unwrap());
                    self.next_token()?;
                    private = PROP_TYPE_PRIVATE;
                }
                t if JSParseState::token_is_ident(t) => {
                    /* variable can only be a non-reserved identifier */
                    is_non_reserved_ident =
                        self.token.val == TOK_IDENT && !self.token.data.is_reserved();

                    /* keywords and reserved words have a valid atom */
                    name.replace(self.token.atom().unwrap());

                    self.next_token()?;
                }
                _ => {
                    self.throw_syntax_error("invalid property name");
                    return Err(-1);
                }
            }
        }

        if is_non_reserved_ident
            && prop_type == PROP_TYPE_IDENT
            && allow_var
            && !(self.token.val == ':' as i32 || (self.token.val == '(' as i32 && allow_method))
        {
            prop_type = PROP_TYPE_VAR;
        }

        if !matches!(prop_type, PROP_TYPE_IDENT | PROP_TYPE_VAR) && self.token.val != '(' as i32 {
            self.throw_syntax_error("invalid property name");
            return Err(-1);
        }

        Ok((name.unwrap_or(Atom::NULL), prop_type | private))
    }
}
