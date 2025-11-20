use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, opcodes},
    function::{FuncCallType, JSFunctionDef},
    utils::read_from_ptr,
};

use super::{
    emit::get_prev_opcode,
    state::JSParseState,
    token::{CHAR_COMMA, TOK_ELLIPSIS},
};

impl<'s> JSParseState<'s> {
    /// parse function call for current function def.
    pub(super) fn parse_func_call(
        &mut self,
        call_type: FuncCallType,
        has_optional_chain: bool,
        optional_chaining_label: &mut i32,
    ) -> Result<(), i32> {
        self.next_token()?;

        let opcode = if call_type == FuncCallType::FUNC_CALL_NORMAL {
            unsafe { self.parse_func_call2(call_type, has_optional_chain, optional_chaining_label) }
        } else {
            opcodes::OP_invalid
        };
        self.parse_func_call3(
            unsafe { self.cur_func_def.as_mut().unwrap() },
            opcode,
            call_type,
            has_optional_chain,
            optional_chaining_label,
        )
    }

    fn parse_func_call3(
        &mut self,
        fd: &mut JSFunctionDef, // TODO: remove this arg
        opcode: u8,
        call_type: FuncCallType,
        _has_optional_chain: bool,
        _optional_chaining_label: &mut i32,
    ) -> Result<(), i32> {
        match call_type {
            FuncCallType::FUNC_CALL_SUPER_CTOR => {
                self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                self.emit_atom(Atom::CONST_this_active_func);
                self.emit_u16(0);

                self.emit_op(OpCode::GET_SUPER);

                self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                self.emit_atom(Atom::CONST_new_target);
                self.emit_u16(0);
            }
            FuncCallType::FUNC_CALL_NEW => {
                self.emit_op(OpCode::DUP); // new.target = function
            }

            #[cfg(debug_assertions)]
            FuncCallType::FUNC_CALL_TEMPLATE => {
                unreachable!(
                    "parse template call won't go here, it should been done in Self::parse_postfix_expr()"
                );
            }
            _ => {}
        }

        // parse arguments
        let mut arg_count = 0;
        while self.token.val != ')' as i32 {
            if arg_count >= 65535 {
                self.throw_syntax_error("Too many call arguments");
                return Err(-1);
            }
            if self.token.val == TOK_ELLIPSIS {
                break;
            }

            self.parse_assign_expr()?;

            arg_count += 1;

            if self.token.val == ')' as i32 {
                break;
            }

            // accept a trailing comma before the ')'
            self.parse_expect(',' as i32)?;
        }

        // TODO: use match self.token.val {} instead
        // XXX
        if self.token.val == TOK_ELLIPSIS {
            // ellipsis args, e.g.: f(...[a,b,c]), f(a,b, ...[x,y,z])
            self.emit_op(OpCode::ARRAY_FROM);
            self.emit_u16(arg_count as _);
            self.emit_op(OpCode::PUSH_I32);
            self.emit_u32(arg_count as _);

            // on stack: array idx
            while self.token.val != ')' as i32 {
                if self.token.val == TOK_ELLIPSIS {
                    self.next_token()?;
                    self.parse_assign_expr()?;

                    /* XXX: could pass is_last indicator? */
                    self.emit_op(OpCode::APPEND);
                } else {
                    self.parse_assign_expr()?;

                    /* array idx val */
                    self.emit_op(OpCode::DEFINE_ARRAY_EL);
                    self.emit_op(OpCode::INC);
                }
                if self.token.val == ')' as i32 {
                    break;
                }

                /* accept a trailing comma before the ')' */
                self.parse_expect(CHAR_COMMA)?;
            } // while

            self.next_token()?;

            // drop the index
            self.emit_op(OpCode::DROP);

            // apply function call
            match opcode {
                opcodes::OP_get_field
                | opcodes::op_scope_get_private_field
                | opcodes::OP_get_array_el
                | opcodes::op_scope_get_ref => {
                    // obj func array -> func obj array
                    self.emit_op(OpCode::PERM3);
                    self.emit_op(OpCode::APPLY);
                    self.emit_u16((call_type == FuncCallType::FUNC_CALL_NEW) as _);
                }
                opcodes::OP_eval => {
                    self.emit_op(OpCode::APPLY_EVAL);
                    self.emit_u16(fd.scope_level as _);
                    fd.has_eval_call = true;
                }
                _ => {
                    if call_type == FuncCallType::FUNC_CALL_SUPER_CTOR {
                        self.emit_op(OpCode::APPLY);
                        self.emit_u16(1);
                        /* set the 'this' value */
                        self.emit_op(OpCode::DUP);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom(Atom::CONST_this);
                        self.emit_u16(0);

                        self.emit_class_field_init();
                    } else if call_type == FuncCallType::FUNC_CALL_NEW {
                        /* obj func array -> func obj array */
                        self.emit_op(OpCode::PERM3);
                        self.emit_op(OpCode::APPLY);
                        self.emit_u16(1);
                    } else {
                        /* func array -> func undef array */
                        self.emit_op(OpCode::UNDEFINED);
                        self.emit_op(OpCode::SWAP);
                        self.emit_op(OpCode::APPLY);
                        self.emit_u16(0);
                    }
                }
            }
        } else {
            self.next_token()?;
            self.emit_func_call(fd, call_type, opcode, arg_count as _);
        }

        Ok(())
    }

    pub(super) unsafe fn parse_func_call2(
        &mut self,
        call_type: FuncCallType,
        has_optional_chain: bool,
        optional_chaining_label: &mut i32,
    ) -> u8 {
        let mut opcode = get_prev_opcode(self.cur_func());
        let drop_count: i32;

        match opcode {
            opcodes::OP_get_field => {
                // keep the object on the stack
                let fd = self.cur_func_mut();
                fd.byte_code[fd.last_opcode_pos as usize] = opcodes::OP_get_field2;
                drop_count = 2;
            }
            opcodes::op_get_field_opt_chain => {
                let fd = self.cur_func_mut();
                let opt_chain_label = unsafe {
                    read_from_ptr::<u32>(
                        fd.byte_code
                            .as_ptr()
                            .offset(fd.last_opcode_pos as isize + 1 + 4 + 1),
                    )
                };
                /* keep the object on the stack */
                fd.byte_code[fd.last_opcode_pos as usize] = opcodes::OP_get_field2;
                fd.byte_code.truncate(fd.last_opcode_pos as usize + 1 + 4); // ...size = fd->last_opcode_pos + 1 + 4;
                let next_label = self.emit_goto(OpCode::GOTO, -1);
                self.emit_label(opt_chain_label as _);
                /* need an additional undefined value for the
                case where the optional field does not
                exists */
                self.emit_op(OpCode::UNDEFINED);
                self.emit_label(next_label);
                drop_count = 2;
                opcode = OpCode::GET_FIELD as u8;
            }
            opcodes::op_scope_get_private_field => {
                /* keep the object on the stack */
                let fd = self.cur_func_mut();
                fd.byte_code[fd.last_opcode_pos as usize] = opcodes::op_scope_get_private_field2;
                drop_count = 2;
            }
            opcodes::OP_get_array_el => {
                /* keep the object on the stack */
                let fd = self.cur_func_mut();
                fd.byte_code[fd.last_opcode_pos as usize] = opcodes::OP_get_array_el2;
                drop_count = 2;
            }
            opcodes::op_get_array_el_opt_chain => {
                let fd = self.cur_func_mut();
                let opt_chain_label = unsafe {
                    read_from_ptr::<u32>(
                        fd.byte_code
                            .as_ptr()
                            .offset(fd.last_opcode_pos as isize + 1 + 1),
                    )
                };
                /* keep the object on the stack */
                let fd = self.cur_func_mut();
                fd.byte_code[fd.last_opcode_pos as usize] = opcodes::OP_get_array_el2;
                fd.byte_code.truncate(fd.last_opcode_pos as usize + 1);

                let next_label = self.emit_goto(OpCode::GOTO, -1);
                self.emit_label(opt_chain_label as _);
                /* need an additional undefined value for the
                case where the optional field does not
                exists */
                self.emit_op(OpCode::UNDEFINED);
                self.emit_label(next_label);
                drop_count = 2;
                opcode = opcodes::OP_get_array_el;
            }
            opcodes::op_scope_get_var => {
                let fd = self.cur_func_mut();
                let name = Atom::from_id(unsafe {
                    read_from_ptr::<u32>(
                        fd.byte_code
                            .as_ptr()
                            .offset(fd.last_opcode_pos as isize + 1),
                    )
                });
                let scope = unsafe {
                    read_from_ptr::<u16>(
                        fd.byte_code
                            .as_ptr()
                            .offset(fd.last_opcode_pos as isize + 5),
                    )
                };

                if name == Atom::CONST_eval
                    && call_type == FuncCallType::FUNC_CALL_NORMAL
                    && !has_optional_chain
                {
                    /* direct 'eval' */
                    opcode = opcodes::OP_eval;
                } else {
                    /* verify if function name resolves to a simple
                      get_loc/get_arg: a function call inside a `with`
                      statement can resolve to a method call of the
                      `with` context object
                    */
                    /* XXX: always generate the op_scope_get_ref
                    and remove it in variable resolution
                    pass ? */
                    if fd.has_with_scope(scope as _) {
                        opcode = opcodes::op_scope_get_ref;
                        fd.byte_code[fd.last_opcode_pos as usize] = opcode;
                    }
                }
                drop_count = 1;
            }
            opcodes::OP_get_super_value => {
                let fd = self.cur_func_mut();
                fd.byte_code[fd.last_opcode_pos as usize] = opcodes::OP_get_array_el;
                /* on stack: this func_obj */
                opcode = opcodes::OP_get_array_el;
                drop_count = 2;
            }
            _ => {
                opcode = opcodes::OP_invalid;
                drop_count = 1;
            }
        }

        if has_optional_chain {
            self.optional_chain_test(optional_chaining_label, drop_count);
        }

        opcode
    }

    pub(super) fn emit_func_call(
        &mut self,
        fd: &mut JSFunctionDef,
        call_type: FuncCallType,
        opcode: u8,
        arg_count: u16,
    ) {
        match opcode {
            opcodes::OP_get_field
            | opcodes::op_scope_get_private_field
            | opcodes::OP_get_array_el
            | opcodes::op_scope_get_ref => {
                self.emit_op(OpCode::CALL_METHOD);
                self.emit_u16(arg_count);
            }
            opcodes::OP_eval => {
                debug_assert!(fd.scope_level >= 0);
                self.emit_op(OpCode::EVAL);
                self.emit_u16(arg_count);
                self.emit_u16(fd.scope_level as _);

                fd.has_eval_call = true;
            }
            _ => {
                if call_type == FuncCallType::FUNC_CALL_SUPER_CTOR {
                    self.emit_op(OpCode::CALL_CONSTRUCTOR);
                    self.emit_u16(arg_count);

                    /* set the 'this' value */
                    self.emit_op(OpCode::DUP);
                    self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                    self.emit_atom(Atom::CONST_this);
                    self.emit_u16(0);

                    self.emit_class_field_init();
                } else if call_type == FuncCallType::FUNC_CALL_NEW {
                    self.emit_op(OpCode::CALL_CONSTRUCTOR);
                    self.emit_u16(arg_count);
                } else {
                    self.emit_op(OpCode::CALL);
                    self.emit_u16(arg_count);
                }
            }
        }
    }
}
