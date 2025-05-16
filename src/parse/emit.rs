use std::ops::DerefMut;

use crate::{
    atom::{consts::*, JSAtom, JSAtomTypeHint, JS_DupAtom, JS_ATOM_NULL},
    bytecode::{opcodes, OpCode, OpCodeTemp},
    function::{JSFunctionDef, JSFunctionKindEnum},
    mem::js_resize_array,
    utils::{push_vec_u16, push_vec_u32, push_vec_u8},
    value::{JSValue, JSValueConst, JS_FreeValue, JS_TAG_STRING, JS_VALUE_GET_TAG},
};

use super::state::JSParseState;

/* argument of OP_special_object */
pub enum OPSpecialObjectEnum {
    OP_SPECIAL_OBJECT_ARGUMENTS,
    OP_SPECIAL_OBJECT_MAPPED_ARGUMENTS,
    OP_SPECIAL_OBJECT_THIS_FUNC,
    OP_SPECIAL_OBJECT_NEW_TARGET,
    OP_SPECIAL_OBJECT_HOME_OBJECT,
    OP_SPECIAL_OBJECT_VAR_OBJECT,
    OP_SPECIAL_OBJECT_IMPORT_META,
}

impl JSFunctionDef {
    pub unsafe fn emit_u8(&mut self, val: u8) {
        push_vec_u8(&mut self.byte_code, val);
    }

    pub unsafe fn emit_u16(&mut self, val: u16) {
        push_vec_u16(&mut self.byte_code, val);
    }

    pub unsafe fn emit_u32(&mut self, val: u32) {
        push_vec_u32(&mut self.byte_code, val);
    }

    /// emit an atom, increase ref_count
    pub unsafe fn emit_atom_dup(&mut self, name: JSAtom) {
        self.emit_u32(JS_DupAtom(self.ctx.as_ptr(), name).0);
    }

    unsafe fn emit_op_u8(&mut self, op: u8, last_line_num: u32) {
        let bc = &mut self.byte_code;

        // Use the line number of the last token used, not the next token,
        // nor the current offset in the source file.
        if self.last_opcode_line_num != last_line_num {
            push_vec_u8(bc, OpCodeTemp::LINE_NUM as u8);
            push_vec_u32(bc, last_line_num);
            self.last_opcode_line_num = last_line_num;
        }

        self.last_opcode_pos = bc.len() as i32;
        bc.push(op);
    }
    pub unsafe fn emit_op(&mut self, op: OpCode, last_line_num: u32) {
        self.emit_op_u8(op as u8, last_line_num);
    }
    pub unsafe fn emit_op_temp(&mut self, op: OpCodeTemp, last_line_num: u32) {
        self.emit_op_u8(op as u8, last_line_num);
    }

    pub unsafe fn emit_return(&mut self, mut hasval: bool, last_line_num: u32) {
        // execute the finally blocks before return
        if self.func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL {
            if !hasval {
                // no value: direct return in case of async generator
                self.emit_op(OpCode::UNDEFINED, last_line_num);
                hasval = true;
            } else if self.func_kind == JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR {
                // the await must be done before handling the "finally" in
                // case it raises an exception
                self.emit_op(OpCode::AWAIT, last_line_num);
            }
        }

        let mut top = self.top_break;
        while !top.is_null() {
            if (*top).has_iterator || (*top).label_finally != -1 {
                if !hasval {
                    self.emit_op(OpCode::UNDEFINED, last_line_num);
                    hasval = true;
                }
                // Remove the stack elements up to and including the catch
                // offset. When 'yield' is used in an expression we have
                // no easy way to count them, so we use this specific
                // instruction instead.
                self.emit_op(OpCode::NIP_CATCH, last_line_num);

                // stack: iter_obj next ret_val
                if (*top).has_iterator {
                    if self.func_kind == JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR {
                        self.emit_op(OpCode::NIP, last_line_num); // next
                        self.emit_op(OpCode::SWAP, last_line_num);
                        self.emit_op(OpCode::GET_FIELD2, last_line_num);
                        self.emit_atom_dup(JS_ATOM_return);

                        // stack: iter_obj return_func
                        self.emit_op(OpCode::DUP, last_line_num);
                        self.emit_op(OpCode::IS_UNDEFINED_OR_NULL, last_line_num);

                        let label_next = self.emit_goto(OpCode::IF_TRUE, -1, last_line_num);
                        self.emit_op(OpCode::CALL_METHOD, last_line_num);
                        self.emit_u16(0);
                        self.emit_op(OpCode::ITERATOR_CHECK_OBJECT, last_line_num);
                        self.emit_op(OpCode::AWAIT, last_line_num);

                        let label_next2 = self.emit_goto(OpCode::GOTO, -1, last_line_num);
                        self.emit_label(label_next, last_line_num);
                        self.emit_op(OpCode::DROP, last_line_num);
                        self.emit_label(label_next2, last_line_num);
                        self.emit_op(OpCode::DROP, last_line_num);
                    } else {
                        self.emit_op(OpCode::ROT3R, last_line_num);
                        self.emit_op(OpCode::UNDEFINED, last_line_num); // dummy catch offset
                        self.emit_op(OpCode::ITERATOR_CLOSE, last_line_num);
                    }
                } else {
                    // execute the "finally" block
                    self.emit_goto(OpCode::GOSUB, (*top).label_finally, last_line_num);
                }
            }

            top = (*top).prev;
        }

        if self.is_derived_class_constructor {
            // 'this' can be uninitialized, so it may be accessed only if
            // the derived class constructor does not return an object
            let label_return = if hasval {
                self.emit_op(OpCode::CHECK_CTOR_RETURN, last_line_num);
                let l = self.emit_goto(OpCode::IF_FALSE, -1, last_line_num);
                self.emit_op(OpCode::DROP, last_line_num);
                l
            } else {
                -1
            };

            // The error should be raised in the caller context, so we use
            // a specific opcode
            self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR_CHECKTHIS, last_line_num);
            self.emit_atom_dup(JS_ATOM_this);
            self.emit_u16(0);

            self.emit_label(label_return, last_line_num);
            self.emit_op(OpCode::RETURN, last_line_num);
        } else if self.func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL {
            self.emit_op(OpCode::RETURN_ASYNC, last_line_num);
        } else {
            self.emit_op(
                if hasval {
                    OpCode::RETURN
                } else {
                    OpCode::RETURN_UNDEF
                },
                last_line_num,
            );
        }
    }

    /* don't update the last opcode and don't emit line number info */
    unsafe fn emit_label_raw(&mut self, label: i32) {
        debug_assert!(label >= 0);
        self.emit_u8(OpCodeTemp::LABEL as u8);
        self.emit_u32(label as _);
        self.labels[label as usize].pos = self.byte_code.len() as _;
    }

    /* return the label ID offset */
    pub unsafe fn emit_label(&mut self, label: i32, last_line_num: u32) -> i32 {
        if label >= 0 {
            self.emit_op_temp(OpCodeTemp::LABEL, last_line_num);
            self.emit_u32(label as _);
            self.labels[label as usize].pos = self.byte_code.len() as _;
            self.byte_code.len() as i32 - 4
        } else {
            -1
        }
    }

    /// return label or -1 if dead code
    pub unsafe fn emit_goto(&mut self, opcode: OpCode, mut label: i32, last_line_num: u32) -> i32 {
        if self.is_live_code() {
            if label < 0 {
                label = self.add_label().map_or(-1, |x| x as i32);
            }
            debug_assert!(label >= 0);

            self.emit_op(opcode, last_line_num);
            self.emit_u32(label as _);

            self.labels[label as usize].ref_count += 1;
            label
        } else {
            -1
        }
    }

    /// Note: `val` keep ref_count unchanged after calling this function
    pub unsafe fn emit_push_const(
        &mut self,
        val: JSValueConst,
        as_atom: bool,
        last_line_num: u32,
    ) -> i32 {
        if JS_VALUE_GET_TAG(val) == JS_TAG_STRING && as_atom {
            // Note: converting to atom will consume the string,
            // so clone the string description to keep val's refcount
            if let Ok(atom) = self
                .ctx
                .rt_mut()
                .add_atom_jsstring(val.with_string(|s| s.clone()), JSAtomTypeHint::String)
            {
                if !atom.is_int_type() {
                    self.emit_op(OpCode::PUSH_ATOM_VALUE, last_line_num);
                    self.emit_u32(atom.0);
                    return 0;
                }
            }

            // if atom != JS_ATOM_NULL && !atom.is_int_type() {
            //     self.emit_op(OpCode::PUSH_ATOM_VALUE, last_line_num);
            //     self.emit_u32(atom.0);
            //     return 0;
            // }
        }

        if let Ok(idx) = self.cpool_add(self.ctx, val.dup()) {
            self.emit_op(OpCode::PUSH_CONST, last_line_num);
            self.emit_u32(idx as u32);
            0
        } else {
            -1
        }
    }

    /* initialize the class fields, called by the constructor. Note:
    super() can be called in an arrow function, so <this> and
    <class_fields_init> can be variable references */
    pub unsafe fn emit_class_field_init(&mut self, last_line_num: u32) {
        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR, last_line_num);
        self.emit_atom_dup(JS_ATOM_class_fields_init);
        self.emit_u16(self.scope_level as _);

        /* no need to call the class field initializer if not defined */
        self.emit_op(OpCode::DUP, last_line_num);
        let label_next = self.emit_goto(OpCode::IF_FALSE, -1, last_line_num);

        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR, last_line_num);
        self.emit_atom_dup(JS_ATOM_this);
        self.emit_u16(0);

        self.emit_op(OpCode::SWAP, last_line_num);

        self.emit_op(OpCode::CALL_METHOD, last_line_num);
        self.emit_u16(0);

        self.emit_label(label_next, last_line_num);
        self.emit_op(OpCode::DROP, last_line_num);
    }

    pub unsafe fn emit_spread_code(&mut self, depth: i32, last_line_num: u32) {
        // XXX: could check if enum object is an actual array and optimize
        //       slice extraction. enumeration record and target array are in a
        //       different order from OP_append case.
        // enum_rec xxx -- enum_rec xxx array 0
        self.emit_op(OpCode::ARRAY_FROM, last_line_num);
        self.emit_u16(0);
        self.emit_op(OpCode::PUSH_I32, last_line_num);
        self.emit_u32(0);

        let label_rest_next = self.add_label().map_or(-1, |x| x as i32);
        self.emit_label(label_rest_next, last_line_num);
        self.emit_op(OpCode::FOR_OF_NEXT, last_line_num);
        self.emit_u8((2 + depth) as u8);

        let label_rest_done = self.emit_goto(OpCode::IF_TRUE, -1, last_line_num);
        // array idx val -- array idx
        self.emit_op(OpCode::DEFINE_ARRAY_EL, last_line_num);
        self.emit_op(OpCode::INC, last_line_num);
        self.emit_goto(OpCode::GOTO, label_rest_next as _, last_line_num);
        self.emit_label(label_rest_done, last_line_num);
        // enum_rec xxx array idx undef -- enum_rec xxx array
        self.emit_op(OpCode::DROP, last_line_num);
        self.emit_op(OpCode::DROP, last_line_num);
    }

    // pub unsafe fn emit_break(
    //     &mut self,
    //     name: JSAtom,
    //     is_continue: bool,
    //     last_line_num: u32,
    // ) -> i32 {
    //     let mut scope_level = self.scope_level;
    //     let mut top = self.top_break;

    //     while !top.is_null() {
    //         (*s).close_scopes(scope_level, (*top).scope_level);

    //         scope_level = (*top).scope_level;
    //         if is_continue
    //             && (*top).label_cont != -1
    //             && (name == JS_ATOM_NULL || (*top).label_name == name)
    //         {
    //             // continue stays inside the same block
    //             self.emit_goto(OpCode::GOTO, (*top).label_cont, last_line_num);
    //             return 0;
    //         }
    //         if !is_continue
    //             && (*top).label_break != -1
    //             && (name == JS_ATOM_NULL || (*top).label_name == name)
    //         {
    //             self.emit_goto(OpCode::GOTO, (*top).label_break, last_line_num);
    //             return 0;
    //         }

    //         let i = if (*top).has_iterator {
    //             self.emit_op(OpCode::ITERATOR_CLOSE, last_line_num);
    //             3
    //         } else {
    //             0
    //         };
    //         for _ in i..(*top).drop_count {
    //             self.emit_op(OpCode::DROP, last_line_num);
    //         }

    //         if (*top).label_finally != -1 {
    //             // must push dummy value to keep same stack depth
    //             self.emit_op(OpCode::UNDEFINED, last_line_num);
    //             self.emit_goto(OpCode::GOSUB, (*top).label_finally, last_line_num);
    //             self.emit_op(OpCode::DROP, last_line_num);
    //         }
    //         top = (*top).prev;
    //     }

    //     if name == JS_ATOM_NULL {
    //         if is_continue {
    //             return js_parse_error(s, c"continue must be inside loop");
    //         } else {
    //             return js_parse_error(s, c"break must be inside loop or switch");
    //         }
    //     } else {
    //         return js_parse_error(s, c"break/continue label not found");
    //     }
    // }

    #[inline(always)]
    pub unsafe fn get_prev_opcode(&self) -> u8 {
        if self.last_opcode_pos < 0 {
            OpCode::INVALID as _
        } else {
            self.byte_code[self.last_opcode_pos as usize] as _
        }
    }

    pub unsafe fn is_live_code(&self) -> bool {
        match self.get_prev_opcode() {
            opcodes::OP_tail_call
            | opcodes::OP_tail_call_method
            | opcodes::OP_return
            | opcodes::OP_return_undef
            | opcodes::OP_return_async
            | opcodes::OP_throw
            | opcodes::OP_throw_error
            | opcodes::OP_goto
            | opcodes::OP_ret => false,

            #[cfg(feature = "short_opcode")]
            opcodes::OpCode::GOTO8 | opcodes::OpCode::GOTO16 => false,

            _ => true,
        }
    }

    // pub unsafe fn throw_parse_error(
    //     &mut self,
    //     message: &CStr,
    //     filename: *const c_char,
    //     line_num: Option<u32>,
    // ) {
    //     let ctx = self.ctx.as_mut().unwrap();

    //     ctx.JS_ThrowError2(JSErrorEnum::Syntax, message, false);

    //     let mut backtrace_flags = 0;
    //     if self.backtrace_barrier {
    //         backtrace_flags = JS_BACKTRACE_FLAG_SINGLE_LEVEL;
    //     }

    //     build_backtrace(
    //         ctx,
    //         (*ctx.rt).current_exception,
    //         filename,
    //         line_num,
    //         backtrace_flags,
    //     );
    // }
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_u8(s: *mut JSParseState, val: u8) {
    (*s).cur_func().emit_u8(val)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_u16(s: *mut JSParseState, val: u16) {
    (*s).cur_func().emit_u16(val)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_u32(s: *mut JSParseState, val: u32) {
    (*s).cur_func().emit_u32(val)
}

/// emit an atom, increase ref_count
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_atom_dup(s: *mut JSParseState, name: JSAtom) {
    (*s).cur_func().emit_atom_dup(name)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
unsafe fn emit_op_u8(s: *mut JSParseState, op: u8) {
    (*s).cur_func().emit_op_u8(op, (*s).last_line_num)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_op(s: *mut JSParseState, op: OpCode) {
    (*s).cur_func().emit_op(op, (*s).last_line_num)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_op_temp(s: *mut JSParseState, op: OpCodeTemp) {
    (*s).cur_func().emit_op_temp(op, (*s).last_line_num)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_return(s: *mut JSParseState, hasval: bool) {
    (*s).cur_func().emit_return(hasval, (*s).last_line_num)
}

/* don't update the last opcode and don't emit line number info */
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_label_raw(s: *mut JSParseState, label: i32) {
    (*s).cur_func().emit_label_raw(label)
}

/* return the label ID offset */
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_label(s: *mut JSParseState, label: i32) -> i32 {
    (*s).cur_func().emit_label(label, (*s).last_line_num)
}

/* return label or -1 if dead code */
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_goto(s: *mut JSParseState, opcode: OpCode, label: i32) -> i32 {
    (*s).cur_func().emit_goto(opcode, label, (*s).last_line_num)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_push_const(s: *mut JSParseState, val: JSValueConst, as_atom: bool) -> i32 {
    (*s).cur_func()
        .emit_push_const(val, as_atom, (*s).last_line_num)
}

/* initialize the class fields, called by the constructor. Note:
super() can be called in an arrow function, so <this> and
<class_fields_init> can be variable references */
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_class_field_init(s: *mut JSParseState) {
    (*s).cur_func().emit_class_field_init((*s).last_line_num)
}

#[inline(always)]
pub unsafe fn get_prev_opcode(fd: *mut JSFunctionDef) -> u8 {
    if (*fd).last_opcode_pos < 0 {
        OpCode::INVALID as _
    } else {
        (*fd).byte_code[(*fd).last_opcode_pos as usize] as _
    }
}

pub unsafe fn js_is_live_code(s: *mut JSParseState) -> bool {
    match get_prev_opcode((*s).cur_func()) {
        opcodes::OP_tail_call
        | opcodes::OP_tail_call_method
        | opcodes::OP_return
        | opcodes::OP_return_undef
        | opcodes::OP_return_async
        | opcodes::OP_throw
        | opcodes::OP_throw_error
        | opcodes::OP_goto
        | opcodes::OP_ret => false,

        #[cfg(feature = "short_opcode")]
        opcodes::OpCode::GOTO8 | opcodes::OpCode::GOTO16 => false,

        _ => true,
    }
}

pub unsafe fn js_emit_spread_code(s: *mut JSParseState, depth: i32) {
    // XXX: could check if enum object is an actual array and optimize
    //       slice extraction. enumeration record and target array are in a
    //       different order from OP_append case.
    // enum_rec xxx -- enum_rec xxx array 0
    emit_op(s, OpCode::ARRAY_FROM);
    emit_u16(s, 0);
    emit_op(s, OpCode::PUSH_I32);
    emit_u32(s, 0);

    let label_rest_next = (*s).cur_func().add_label().map_or(-1, |x| x as i32);
    emit_label(s, label_rest_next);
    emit_op(s, OpCode::FOR_OF_NEXT);
    emit_u8(s, (2 + depth) as u8); // C2RS: explicit cast from i32 to u8

    let label_rest_done = emit_goto(s, OpCode::IF_TRUE, -1);
    // array idx val -- array idx
    emit_op(s, OpCode::DEFINE_ARRAY_EL);
    emit_op(s, OpCode::INC);
    emit_goto(s, OpCode::GOTO, label_rest_next as _);
    emit_label(s, label_rest_done);
    // enum_rec xxx array idx undef -- enum_rec xxx array
    emit_op(s, OpCode::DROP);
    emit_op(s, OpCode::DROP);
}

pub unsafe fn emit_break(s: *mut JSParseState, name: JSAtom, is_continue: bool) -> i32 {
    let mut scope_level = (*s).cur_func().scope_level;
    let mut top = (*s).cur_func().top_break;

    while !top.is_null() {
        (*s).close_scopes(scope_level, (*top).scope_level);
        scope_level = (*top).scope_level;
        if is_continue
            && (*top).label_cont != -1
            && (name == JS_ATOM_NULL || (*top).label_name == name)
        {
            // continue stays inside the same block
            emit_goto(s, OpCode::GOTO, (*top).label_cont);
            return 0;
        }
        if !is_continue
            && (*top).label_break != -1
            && (name == JS_ATOM_NULL || (*top).label_name == name)
        {
            emit_goto(s, OpCode::GOTO, (*top).label_break);
            return 0;
        }

        let i = if (*top).has_iterator {
            emit_op(s, OpCode::ITERATOR_CLOSE);
            3
        } else {
            0
        };
        for _ in i..(*top).drop_count {
            emit_op(s, OpCode::DROP);
        }

        if (*top).label_finally != -1 {
            // must push dummy value to keep same stack depth
            emit_op(s, OpCode::UNDEFINED);
            emit_goto(s, OpCode::GOSUB, (*top).label_finally);
            emit_op(s, OpCode::DROP);
        }
        top = (*top).prev;
    }

    (*s).throw_syntax_error(if name == JS_ATOM_NULL {
        if is_continue {
            c"continue must be inside loop"
        } else {
            c"break must be inside loop or switch"
        }
    } else {
        c"break/continue label not found"
    });

    -1
}
