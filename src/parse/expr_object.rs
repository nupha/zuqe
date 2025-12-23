use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp},
    class::{OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD},
    function::JSFunctionKindEnum,
    object::{
        PROP_TYPE_ASYNC, PROP_TYPE_ASYNC_STAR, PROP_TYPE_GET, PROP_TYPE_IDENT, PROP_TYPE_PRIVATE,
        PROP_TYPE_SET, PROP_TYPE_STAR, PROP_TYPE_VAR,
    },
    value::JSValue,
};

use super::{
    JSParseFunctionEnum, JSParseState, obj::set_object_name_computed, tokenize::TokenKind,
};

impl<'s> JSParseState<'s> {
    pub(super) fn parse_object_literal(&mut self) -> Result<TokenKind, i32> {
        self.next_token()?;

        // XXX: add an initial length that will be patched back
        self.emit_op(OpCode::OBJECT);

        let mut has_proto = false;

        while self.token.val != TokenKind::Char('}') {
            // specific case for getter/setter
            let start_ptr = self.token.ptr;
            let start_line = self.token.line_num;

            if self.token.val == TokenKind::ELLIPSIS {
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
                } else if self.token.val == TokenKind::Char('(') {
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
                    self.parse_expect(TokenKind::Char(':'))?;
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
            if self.token.val != TokenKind::Char(',') {
                break;
            }
            self.next_token()?;
        } // while token is '}'

        self.parse_expect(TokenKind::Char('}'))
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
                    TokenKind::Char(':' | ',' | '}' | '(' | ';') => {
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
            } else if self.token.val == TokenKind::Char('*') {
                self.next_token()?;
                prop_type = PROP_TYPE_STAR;
            } else if self.token_is_pseudo_keyword(Atom::CONST_async)
                && self.peek_token(true) != TokenKind::Char('\n')
            {
                name.replace(self.token.atom().unwrap());

                self.next_token()?;

                if matches!(self.token.val, TokenKind::Char(':' | ',' | '}' | '(')) {
                    is_non_reserved_ident = true;
                } else {
                    name.take();
                    prop_type = if self.token.val == TokenKind::Char('*') {
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
                TokenKind::STRING => {
                    let sbuf = self.token.data.take_string().unwrap();
                    name.replace(
                        self.ctx
                            .create_string_with_buf(sbuf)
                            .and_then(|s| s.to_atom(self.ctx.rt_ptr()))?,
                    );
                    self.next_token()?;
                }
                TokenKind::NUMBER => {
                    name.replace(
                        JSValue::from(self.token.data.number().unwrap()).to_atom(&self.ctx)?,
                    );
                    self.next_token()?;
                }
                TokenKind::Char('[') => {
                    self.next_token()?;
                    self.parse_expr()?;
                    self.parse_expect(TokenKind::Char(']'))?;
                }
                TokenKind::PRIVATE_NAME if allow_private => {
                    name.replace(self.token.atom().unwrap());
                    self.next_token()?;
                    private = PROP_TYPE_PRIVATE;
                }
                t if Self::token_is_ident(t) => {
                    /* variable can only be a non-reserved identifier */
                    is_non_reserved_ident =
                        self.token.val == TokenKind::IDENT && !self.token.data.is_reserved();

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
            && !(self.token.val == TokenKind::Char(':')
                || (allow_method && self.token.val == TokenKind::Char('(')))
        {
            prop_type = PROP_TYPE_VAR;
        }

        if !matches!(prop_type, PROP_TYPE_IDENT | PROP_TYPE_VAR)
            && self.token.val != TokenKind::Char('(')
        {
            self.throw_syntax_error("invalid property name");
            return Err(-1);
        }

        Ok((name.unwrap_or(Atom::NULL), prop_type | private))
    }
}
