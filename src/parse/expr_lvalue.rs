use crate::{
    atom::{consts::*, JSAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{opcodes, OpCode, OpCodeTemp},
    context::JS_MODE_STRICT,
    utils::{get_u16, get_u32},
};

use super::{
    emit::{
        emit_atom_dup, emit_label, emit_label_raw, emit_op, emit_op_temp, emit_push_const,
        emit_u16, emit_u32, emit_u8, get_prev_opcode,
    },
    error::js_parse_error,
    state::{JSParseState, PF_POSTFIX_CALL},
    token::{TOK_DEC, TOK_FOR, TOK_INC},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutLValueEnum {
    /// [depth] v ->
    PUT_LVALUE_NOKEEP,
    /// [depth] v -> , keep depth (currently just disable optimizations)
    PUT_LVALUE_NOKEEP_DEPTH,
    /// [depth] v -> v
    PUT_LVALUE_KEEP_TOP,
    /// [depth] v0 v -> v0
    PUT_LVALUE_KEEP_SECOND,
    /// v [depth] ->
    PUT_LVALUE_NOKEEP_BOTTOM,
}

impl JSParseState {
    pub unsafe fn get_lvalue(
        &mut self,
        popcode: *mut i32,
        pscope: *mut i32,
        pname: *mut JSAtom,
        plabel: *mut i32,
        pdepth: *mut i32,
        keep: bool,
        tok: i32,
    ) -> Result<(), i32> {
        macro_rules! throw_error {
            () => {
                js_parse_error(
                    self,
                    match tok {
                        TOK_FOR => c"invalid for in/of left hand-side",
                        TOK_INC | TOK_DEC => c"invalid increment/decrement operand",
                        // 91='[', 123='{'
                        91 | 123 => c"invalid destructuring target",
                        _ => c"invalid assignment left-hand side",
                    },
                );
            };
        }

        let fd = self.cur_func;
        let mut scope = 0;
        let mut name = JS_ATOM_NULL;
        let mut label = -1;
        let depth: i32;

        // we check the last opcode to get the lvalue type
        let mut opcode = get_prev_opcode(fd);

        match opcode {
            opcodes::op_scope_get_var => {
                name = JSAtom(get_u32(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 1),
                ));
                scope = get_u16(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 5),
                );
                match name {
                    JS_ATOM_arguments | JS_ATOM_eval
                        if (*fd).js_mode & JS_MODE_STRICT as u8 != 0 =>
                    {
                        js_parse_error(self, c"invalid lvalue in strict mode");
                        return Err(-1);
                    }
                    JS_ATOM_this | JS_ATOM_new_target => {
                        throw_error!();
                        return Err(-1);
                    }
                    _ => {
                        depth = 2; // will generate OP_get_ref_value
                    }
                }
            }
            opcodes::OP_get_field => {
                name = JSAtom(get_u32(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 1),
                ));
                depth = 1;
            }
            opcodes::op_scope_get_private_field => {
                name = JSAtom(get_u32(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 1),
                ));
                scope = get_u16(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 5),
                );
                depth = 1;
            }
            opcodes::OP_get_array_el => {
                depth = 2;
            }
            opcodes::OP_get_super_value => {
                depth = 3;
            }
            _ => {
                throw_error!();
                return Err(-1);
            }
        }

        // remove the last opcode
        debug_assert!((*fd).last_opcode_pos > 0);
        (*fd).byte_code.truncate((*fd).last_opcode_pos as _);
        (*fd).last_opcode_pos = -1;

        if keep {
            // get the value but keep the object/fields on the stack
            match opcode {
                opcodes::op_scope_get_var => {
                    label = (*self.cur_func)
                        .add_label()
                        .expect("add_label() can't fail") as i32;

                    emit_op_temp(self, OpCodeTemp::SCOPE_MAKE_REF);
                    emit_atom_dup(self, name);
                    emit_u32(self, label as _);
                    emit_u16(self, scope);
                    (*fd).update_label_ref_count(label as _, 1);
                    emit_op(self, OpCode::GET_REF_VALUE);

                    opcode = OpCode::GET_REF_VALUE as _;
                }
                opcodes::OP_get_field => {
                    emit_op(self, OpCode::GET_FIELD2);
                    emit_atom_dup(self, name);
                }
                opcodes::op_scope_get_private_field => {
                    emit_op_temp(self, OpCodeTemp::SCOPE_GET_PRIVATE_FIELD2);
                    emit_atom_dup(self, name);
                    emit_u16(self, scope);
                }
                opcodes::OP_get_array_el => {
                    // XXX: replace by a single opcode ?
                    emit_op(self, OpCode::TO_PROPKEY2);
                    emit_op(self, OpCode::DUP2);
                    emit_op(self, OpCode::GET_ARRAY_EL);
                }
                opcodes::OP_get_super_value => {
                    emit_op(self, OpCode::TO_PROPKEY);
                    emit_op(self, OpCode::DUP3);
                    emit_op(self, OpCode::GET_SUPER_VALUE);
                }
                _ => {
                    unreachable!();
                }
            }
        } else {
            match opcode {
                opcodes::op_scope_get_var => {
                    label = (*self.cur_func)
                        .add_label()
                        .expect("add_label() can't fail") as i32;

                    emit_op_temp(self, OpCodeTemp::SCOPE_MAKE_REF);
                    emit_atom_dup(self, name);
                    emit_u32(self, label as _);
                    emit_u16(self, scope);
                    (*fd).update_label_ref_count(label as _, 1);

                    opcode = OpCode::GET_REF_VALUE as _;
                }
                opcodes::OP_get_array_el => {
                    emit_op(self, OpCode::TO_PROPKEY2);
                }
                opcodes::OP_get_super_value => {
                    emit_op(self, OpCode::TO_PROPKEY);
                }
                _ => {}
            }
        }

        *popcode = opcode as i32;
        *pscope = scope as _;
        // name has refcount for OP_get_field and OP_get_ref_value,
        // and JS_ATOM_NULL for other opcodes
        *pname = name;
        *plabel = label;
        if !pdepth.is_null() {
            *pdepth = depth;
        }

        Ok(())
    }

    // name has a live reference. 'is_let' is only used with opcode =
    // op_scope_get_var which is never generated by get_lvalue().
    pub unsafe fn put_lvalue(
        &mut self,
        opcode: i32,
        scope: i32,
        name: JSAtom,
        label: i32,
        special: PutLValueEnum,
        is_let: bool,
    ) {
        match opcode as u8 {
            opcodes::OP_get_field | opcodes::op_scope_get_private_field => {
                // depth = 1
                match special {
                    PutLValueEnum::PUT_LVALUE_NOKEEP | PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH => {}
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP => {
                        emit_op(self, OpCode::INSERT2); // obj v -> v obj v
                    }
                    PutLValueEnum::PUT_LVALUE_KEEP_SECOND => {
                        emit_op(self, OpCode::PERM3); // obj v0 v -> v0 obj v
                    }
                    PutLValueEnum::PUT_LVALUE_NOKEEP_BOTTOM => {
                        emit_op(self, OpCode::SWAP);
                    }
                }
            }
            opcodes::OP_get_array_el | opcodes::OP_get_ref_value => {
                // depth = 2
                if opcode == opcodes::OP_get_ref_value as i32 {
                    JS_FreeAtom(self.ctx, name);
                    emit_label(self, label);
                }
                match special {
                    PutLValueEnum::PUT_LVALUE_NOKEEP => {
                        emit_op(self, OpCode::NOP); // will trigger optimization
                    }
                    PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH => {}
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP => {
                        emit_op(self, OpCode::INSERT3); // obj prop v -> v obj prop v
                    }
                    PutLValueEnum::PUT_LVALUE_KEEP_SECOND => {
                        emit_op(self, OpCode::PERM4); // obj prop v0 v -> v0 obj prop v
                    }
                    PutLValueEnum::PUT_LVALUE_NOKEEP_BOTTOM => {
                        emit_op(self, OpCode::ROT3L);
                    }
                }
            }
            opcodes::OP_get_super_value => {
                // depth = 3
                match special {
                    PutLValueEnum::PUT_LVALUE_NOKEEP | PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH => {}
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP => {
                        emit_op(self, OpCode::INSERT4); // this obj prop v -> v this obj prop v
                    }
                    PutLValueEnum::PUT_LVALUE_KEEP_SECOND => {
                        emit_op(self, OpCode::PERM5); // this obj prop v0 v -> v0 this obj prop v
                    }
                    PutLValueEnum::PUT_LVALUE_NOKEEP_BOTTOM => {
                        emit_op(self, OpCode::ROT4L);
                    }
                }
            }
            _ => {}
        }

        match opcode as u8 {
            opcodes::op_scope_get_var => {
                // val --
                assert!(
                    special == PutLValueEnum::PUT_LVALUE_NOKEEP
                        || special == PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH
                );
                emit_op_temp(
                    self,
                    if is_let {
                        OpCodeTemp::SCOPE_PUT_VAR_INIT
                    } else {
                        OpCodeTemp::SCOPE_PUT_VAR
                    },
                );
                emit_u32(self, name.0); // has refcount
                emit_u16(self, scope as _);
            }
            opcodes::OP_get_field => {
                emit_op(self, OpCode::PUT_FIELD);
                emit_u32(self, name.0); // name has refcount
            }
            opcodes::op_scope_get_private_field => {
                emit_op_temp(self, OpCodeTemp::SCOPE_PUT_PRIVATE_FIELD);
                emit_u32(self, name.0); // name has refcount
                emit_u16(self, scope as _);
            }
            opcodes::OP_get_array_el => {
                emit_op(self, OpCode::PUT_ARRAY_EL);
            }
            opcodes::OP_get_ref_value => {
                emit_op(self, OpCode::PUT_REF_VALUE);
            }
            opcodes::OP_get_super_value => {
                emit_op(self, OpCode::PUT_SUPER_VALUE);
            }
            _ => {
                unreachable!();
            }
        }
    }

    pub fn parse_left_hand_side_expr(&mut self) -> Result<(), i32> {
        unsafe { self.parse_postfix_expr(PF_POSTFIX_CALL) }
    }
}
