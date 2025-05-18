use crate::{
    atom::{consts::*, JSAtom},
    bytecode::{opcodes, OpCode, OpCodeTemp},
    function::{FuncCallType, JSFunctionDef},
    utils::{get_u16, read_from_ptr},
};

use super::{
    emit::{
        emit_atom_dup, emit_class_field_init, emit_goto, emit_label, emit_op, emit_op_temp,
        emit_u16, emit_u32, get_prev_opcode,
    },
    parse::has_with_scope,
    state::JSParseState,
    token::{CHAR_COMMA, TOK_ELLIPSIS},
};

impl<'s> JSParseState<'s> {
    pub(super) unsafe fn parse_func_call(
        &mut self,
        fd: *mut JSFunctionDef,
        call_type: FuncCallType,
        has_optional_chain: bool,
        optional_chaining_label: &mut i32,
    ) -> Result<(), i32> {
        self.next_token()?;

        let opcode = if call_type == FuncCallType::FUNC_CALL_NORMAL {
            self.parse_func_call2(fd, call_type, has_optional_chain, optional_chaining_label)
        } else {
            opcodes::OP_invalid
        };

        self.parse_func_call3(
            fd,
            opcode,
            call_type,
            has_optional_chain,
            optional_chaining_label,
        )
    }

    pub(super) unsafe fn parse_func_call3(
        &mut self,
        fd: *mut JSFunctionDef,
        opcode: u8,
        call_type: FuncCallType,
        has_optional_chain: bool,
        optional_chaining_label: &mut i32,
    ) -> Result<(), i32> {
        match call_type {
            FuncCallType::FUNC_CALL_TEMPLATE => {
                let mut argc = 0;
                self.parse_template(true, &mut argc)?;
                self.emit_func_call(fd, call_type, opcode, argc as _);
            }
            FuncCallType::FUNC_CALL_NEW => {
                emit_op(self, OpCode::DUP); // new.target = function
            }
            FuncCallType::FUNC_CALL_SUPER_CTOR => {
                emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                emit_atom_dup(self, JS_ATOM_this_active_func);
                emit_u16(self, 0);

                emit_op(self, OpCode::GET_SUPER);

                emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                emit_atom_dup(self, JS_ATOM_new_target);
                emit_u16(self, 0);
            }
            FuncCallType::FUNC_CALL_NORMAL => {}
        }

        // parse arguments
        let mut arg_count = 0;

        while self.token.val != ')' as i32 {
            if arg_count >= 65535 {
                self.throw_syntax_error(c"Too many call arguments");
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
            self.parse_expect(CHAR_COMMA)?;
        }

        if self.token.val == TOK_ELLIPSIS {
            // ellipsis args, e.g.: f(...[a,b,c]), f(a,b, ...[x,y,z])
            emit_op(self, OpCode::ARRAY_FROM);
            emit_u16(self, arg_count as _);
            emit_op(self, OpCode::PUSH_I32);
            emit_u32(self, arg_count as _);

            // on stack: array idx
            while self.token.val != ')' as i32 {
                if self.token.val == TOK_ELLIPSIS {
                    self.next_token()?;
                    self.parse_assign_expr()?;

                    /* XXX: could pass is_last indicator? */
                    emit_op(self, OpCode::APPEND);
                } else {
                    self.parse_assign_expr()?;

                    /* array idx val */
                    emit_op(self, OpCode::DEFINE_ARRAY_EL);
                    emit_op(self, OpCode::INC);
                }
                if self.token.val == ')' as i32 {
                    break;
                }

                /* accept a trailing comma before the ')' */
                self.parse_expect(CHAR_COMMA)?;
            } // while

            self.next_token()?;

            // drop the index
            emit_op(self, OpCode::DROP);

            // apply function call
            match opcode {
                opcodes::OP_get_field
                | opcodes::op_scope_get_private_field
                | opcodes::OP_get_array_el
                | opcodes::op_scope_get_ref => {
                    // obj func array -> func obj array
                    emit_op(self, OpCode::PERM3);
                    emit_op(self, OpCode::APPLY);
                    emit_u16(self, (call_type == FuncCallType::FUNC_CALL_NEW) as _);
                }
                opcodes::OP_eval => {
                    emit_op(self, OpCode::APPLY_EVAL);
                    emit_u16(self, (*fd).scope_level as _);
                    (*fd).has_eval_call = true;
                }
                _ => {
                    if call_type == FuncCallType::FUNC_CALL_SUPER_CTOR {
                        emit_op(self, OpCode::APPLY);
                        emit_u16(self, 1);
                        /* set the 'this' value */
                        emit_op(self, OpCode::DUP);
                        emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        emit_atom_dup(self, JS_ATOM_this);
                        emit_u16(self, 0);

                        emit_class_field_init(self);
                    } else if call_type == FuncCallType::FUNC_CALL_NEW {
                        /* obj func array -> func obj array */
                        emit_op(self, OpCode::PERM3);
                        emit_op(self, OpCode::APPLY);
                        emit_u16(self, 1);
                    } else {
                        /* func array -> func undef array */
                        emit_op(self, OpCode::UNDEFINED);
                        emit_op(self, OpCode::SWAP);
                        emit_op(self, OpCode::APPLY);
                        emit_u16(self, 0);
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
        fd: *mut JSFunctionDef,
        call_type: FuncCallType,
        has_optional_chain: bool,
        optional_chaining_label: &mut i32,
    ) -> u8 {
        let mut opcode = get_prev_opcode(fd);
        let drop_count: i32;

        match opcode {
            opcodes::OP_get_field => {
                /* keep the object on the stack */
                (*fd).byte_code[(*fd).last_opcode_pos as usize] = opcodes::OP_get_field2;
                drop_count = 2;
            }
            opcodes::op_get_field_opt_chain => {
                let opt_chain_label = read_from_ptr::<u32>(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 1 + 4 + 1),
                );
                /* keep the object on the stack */
                (*fd).byte_code[(*fd).last_opcode_pos as usize] = opcodes::OP_get_field2;
                (*fd)
                    .byte_code
                    .truncate((*fd).last_opcode_pos as usize + 1 + 4); // ...size = fd->last_opcode_pos + 1 + 4;
                let next_label = emit_goto(self, OpCode::GOTO, -1);
                emit_label(self, opt_chain_label as _);
                /* need an additional undefined value for the
                case where the optional field does not
                exists */
                emit_op(self, OpCode::UNDEFINED);
                emit_label(self, next_label);
                drop_count = 2;
                opcode = OpCode::GET_FIELD as u8;
            }
            opcodes::op_scope_get_private_field => {
                /* keep the object on the stack */
                (*fd).byte_code[(*fd).last_opcode_pos as usize] =
                    opcodes::op_scope_get_private_field2;
                drop_count = 2;
            }
            opcodes::OP_get_array_el => {
                /* keep the object on the stack */
                (*fd).byte_code[(*fd).last_opcode_pos as usize] = opcodes::OP_get_array_el2;
                drop_count = 2;
            }
            opcodes::op_get_array_el_opt_chain => {
                let opt_chain_label = read_from_ptr::<u32>(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 1 + 1),
                );
                /* keep the object on the stack */
                (*fd).byte_code[(*fd).last_opcode_pos as usize] = opcodes::OP_get_array_el2;
                (*fd).byte_code.truncate((*fd).last_opcode_pos as usize + 1);
                let next_label = emit_goto(self, OpCode::GOTO, -1);
                emit_label(self, opt_chain_label as _);
                /* need an additional undefined value for the
                case where the optional field does not
                exists */
                emit_op(self, OpCode::UNDEFINED);
                emit_label(self, next_label);
                drop_count = 2;
                opcode = opcodes::OP_get_array_el;
            }
            opcodes::op_scope_get_var => {
                let name = JSAtom(read_from_ptr::<u32>(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 1),
                ));
                let scope = get_u16(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 5),
                );
                if name == JS_ATOM_eval
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
                    if has_with_scope(fd, scope as _) {
                        opcode = opcodes::op_scope_get_ref;
                        (*fd).byte_code[(*fd).last_opcode_pos as usize] = opcode;
                    }
                }
                drop_count = 1;
            }
            opcodes::OP_get_super_value => {
                (*fd).byte_code[(*fd).last_opcode_pos as usize] = opcodes::OP_get_array_el;
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

    unsafe fn emit_func_call(
        &mut self,
        fd: *mut JSFunctionDef,
        call_type: FuncCallType,
        opcode: u8,
        arg_count: u16,
    ) {
        match opcode {
            opcodes::OP_get_field
            | opcodes::op_scope_get_private_field
            | opcodes::OP_get_array_el
            | opcodes::op_scope_get_ref => {
                emit_op(self, OpCode::CALL_METHOD);
                emit_u16(self, arg_count);
            }
            opcodes::OP_eval => {
                debug_assert!((*fd).scope_level > 0);
                emit_op(self, OpCode::EVAL);
                emit_u16(self, arg_count);
                emit_u16(self, (*fd).scope_level as _);
                (*fd).has_eval_call = true;
            }
            _ => {
                if call_type == FuncCallType::FUNC_CALL_SUPER_CTOR {
                    emit_op(self, OpCode::CALL_CONSTRUCTOR);
                    emit_u16(self, arg_count);

                    /* set the 'this' value */
                    emit_op(self, OpCode::DUP);
                    emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                    emit_atom_dup(self, JS_ATOM_this);
                    emit_u16(self, 0);

                    emit_class_field_init(self);
                } else if call_type == FuncCallType::FUNC_CALL_NEW {
                    emit_op(self, OpCode::CALL_CONSTRUCTOR);
                    emit_u16(self, arg_count);
                } else {
                    emit_op(self, OpCode::CALL);
                    emit_u16(self, arg_count);
                }
            }
        }
    }
}
