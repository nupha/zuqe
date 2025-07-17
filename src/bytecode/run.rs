use crate::{
    atom::{
        AtomType, JS_ATOM_NULL, JS_FreeAtom, JSAtom,
        consts::{JS_ATOM_return, JS_ATOM_throw},
    },
    bytecode::{
        OpCode, SpecialObject,
        arithmatic::{UnaryOp, js_eq, op_binary_logic},
        operators::{
            js_append_enumerate, js_for_in_next, js_for_in_start, js_for_of_next, js_for_of_start,
            js_has_unscopable, js_iterator_get_value_done, js_method_set_home_object,
            js_method_set_properties, js_op_define_class, js_operator_delete, js_operator_in,
            js_operator_instanceof, js_operator_private_in,
        },
    },
    class::{
        OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD,
        OP_DEFINE_METHOD_SETTER,
    },
    context::{JS_MODE_STRICT, JSContext, is_strict_mode},
    error::{
        JS_THROW_ERROR_DELETE_SUPER, JS_THROW_ERROR_ITERATOR_THROW, JS_THROW_VAR_REDECL,
        JS_THROW_VAR_RO, JS_THROW_VAR_UNINITIALIZED, JS_ThrowReferenceError,
        JS_ThrowReferenceErrorNotDefined, JS_ThrowReferenceErrorUninitialized,
        JS_ThrowReferenceErrorUninitialized2, JS_ThrowSyntaxErrorVarRedeclaration,
    },
    eval::JS_EVAL_TYPE_DIRECT,
    function::{
        Bytecode, CallContext, JS_Call, JS_CallConstructorInternal, JS_CallInternal,
        JSFunctionKindEnum, JSVarRef, build_arg_list, js_build_arguments,
        js_build_mapped_arguments, js_build_rest, js_function_apply,
    },
    generator::{FUNC_RET_AWAIT, FUNC_RET_INITIAL_YIELD, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR},
    iterator::JS_IteratorClose,
    module::{js_dynamic_import, js_import_meta},
    object::{
        JS_DefineObjectName, JS_DefineObjectNameComputed, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE,
        JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD, JS_PROP_THROW,
        JS_PROP_THROW_STRICT, JS_PROP_VARREF, JS_PROP_WRITABLE, JS_SetPrototypeInternal, JSObject,
        JSProperty,
    },
    regexp::js_regexp_constructor_internal,
    stack_frame::StackFrame,
    utils::read_from_ptr,
    value::JSValue,
};

impl Bytecode {
    /// run bytecodes.
    /// before running, runtime current stack frame will be set to `sframe`.
    /// when exit, runtime current stack frame will be restored back to the previous frame.
    pub fn run(
        &self,
        caller_ctx: &JSContext, // Context of the calling environment
        func_home_obj: Option<&JSObject>,
        sframe: &mut StackFrame, // The working stack frame
        this_obj: &JSValue,      // The 'this' value for the function execution
        new_target: JSValue,     // The new.target value for constructor calls
        pc_offset: usize,        // Current instruction pointer (program counter) offset
        sp_offset: usize,        // Current stack pointer offset
        closure_var_refs: &[JSVarRef],
        args: &[JSValue], // actual calling arguments pass in by caller, not padded length
        throwed: bool,    // Indicates if an exception has been thrown
    ) -> JSValue {
        let mode = sframe.js_mode;

        // use bytecode's context
        let realm = self.0.borrow().realm.upgrade().unwrap();
        let ctx = &realm;

        let bytecodes = self.opcodes();
        debug_assert!(
            pc_offset < bytecodes.len(),
            "pc_offset={}, bytecodes.len()={}",
            pc_offset,
            bytecodes.len()
        );

        let cpools = self.cpools_slice();

        let sp_base = sframe.stack_base_ptr().as_ptr();
        let pc_base = bytecodes.as_ptr();
        // convert index to pointer
        let (mut pc, mut sp) = unsafe { (pc_base.add(pc_offset), sp_base.add(sp_offset)) };

        if throwed {
            // has thrown current exception, handle it before execution.
            let (cur_pc, cur_sp) = unsafe {
                (
                    pc.offset_from(pc_base) as usize,
                    sp.offset_from(sp_base) as usize,
                )
            };

            if let Some((new_pc, new_sp)) = sframe.handle_exception(ctx, cur_pc, cur_sp) {
                // continue execution: update pc and sp.
                unsafe {
                    pc = pc_base.offset(new_pc as _);
                    sp = sp_base.offset(new_sp as _);
                }
            } else {
                // abort execution: sp=0, pc unchanged
                sframe.handle_exit(
                    ctx,
                    cur_pc,
                    0,
                    self.0.borrow().func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL,
                );
                return JSValue::EXCEPTION;
            }
        }

        /// get value ref at position `idx`
        #[cfg(debug_assertions)]
        macro_rules! SP_REF {
            (0) => {
                sp.as_mut().unwrap()
            };
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_REF {
            (0) => {
                unsafe { &mut *sp }
            };
            ($idx:expr) => {
                unsafe { &mut *sp.offset($idx as isize) }
            };
        }

        /// put a value at position `idx`, drop old value at that position
        macro_rules! SP_PUT {
            (0, $val:expr) => {
                unsafe { std::ptr::replace::<JSValue>(sp, $val) }
            };
            ($idx:expr, $val:expr) => {
                unsafe { std::ptr::replace::<JSValue>(sp.offset(($idx) as isize), $val) }
            };
        }
        /// equivelent to SP_PUT!(idx, JSValue::UNDEFINED)
        macro_rules! SP_TAKE {
            ($idx:expr) => {
                SP_PUT!($idx, JSValue::UNDEFINED)
            };
        }

        macro_rules! SP_GO {
            ($step:expr) => {
                sp = unsafe { sp.offset(($step) as isize) }
            };
        }

        macro_rules! PC_GO {
            ($step:expr) => {
                pc = unsafe { pc.offset(($step) as isize) }
            };
        }
        macro_rules! PC_FETCH {
            (i32) => {
                unsafe {
                    let v = read_from_ptr::<i32>(pc);
                    pc = pc.add(4);
                    v
                }
            };
            (u32) => {
                unsafe {
                    let v = read_from_ptr::<u32>(pc);
                    pc = pc.add(4);
                    v
                }
            };
            (i16) => {
                unsafe {
                    let v = read_from_ptr::<i16>(pc);
                    pc = pc.add(2);
                    v
                }
            };
            (u16) => {
                unsafe {
                    let v = read_from_ptr::<u16>(pc);
                    pc = pc.add(2);
                    v
                }
            };
            (u8) => {
                unsafe {
                    let v = read_from_ptr::<u8>(pc);
                    pc = pc.add(1);
                    v
                }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! ARG_REF {
            ($idx:expr) => {
                unsafe { sframe.arg_base_ptr.offset(($idx) as _).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! ARG_REF {
            ($idx:expr) => {
                unsafe { &mut *sframe.arg_base_ptr.offset(($idx) as _) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (sframe.var_base_ptr.offset(($idx) as isize).as_mut().unwrap()) }
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (&mut *sframe.var_base_ptr.offset(($idx) as isize)) }
        };
    }

        #[cfg(debug_assertions)]
        macro_rules! CPOOL {
        [$idx:expr] => {
            &cpools.as_ref()[($idx) as usize]
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! CPOOL {
        [$idx:expr] => {
            unsafe {
                &*cpools.as_ref().as_ptr().add(($idx) as usize)
            }
        };
    }

        enum State {
            Exception,
            /// exit with return value and is_generator flag
            Done(JSValue, bool),
        }

        'restart: loop {
            // loop running bytecodes
            let state: State = 'opcodes: loop {
                #[cfg(all(debug_assertions, feature = "dump_bytecode"))]
                unsafe {
                    let pos = pc.offset_from(pc_base) as usize;
                    print!("[O.o] RUN {:5}: ", pos);
                    crate::bytecode::dump::debug_dump_opcode_one(ctx, self, pos);
                    println!();
                }

                let opcode = unsafe { std::mem::transmute::<u8, OpCode>(PC_FETCH!(u8)) };

                match opcode {
                    OpCode::PUSH_I32 => {
                        let a = PC_FETCH!(i32);
                        SP_PUT!(0, JSValue::Int(a));
                        SP_GO!(1);
                    }
                    OpCode::PUSH_CONST => {
                        let id = PC_FETCH!(u32);
                        SP_PUT!(0, CPOOL![id].clone());
                        SP_GO!(1);
                    }

                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_MINUS1 | OpCode::PUSH_0..=OpCode::PUSH_7 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_I8 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_I16 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_CONST8 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_FCLOSURE8 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_EMPTY_STRING => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_GET_LENGTH => {
                        todo!();
                    }

                    OpCode::PUSH_ATOM_VALUE => {
                        let a = PC_FETCH!(u32);
                        SP_PUT!(0, JSAtom(a).to_value(ctx, false));
                        SP_GO!(1);
                    }
                    OpCode::UNDEFINED => {
                        SP_PUT!(0, JSValue::UNDEFINED);
                        SP_GO!(1);
                    }
                    OpCode::NULL => {
                        SP_PUT!(0, JSValue::NULL);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_THIS => {
                        // only called at the start of a function
                        let val = if mode & JS_MODE_STRICT == 0 {
                            match this_obj {
                                JSValue::Object(_) => this_obj.clone(),
                                JSValue::NULL | JSValue::UNDEFINED => {
                                    JSValue::Object(ctx.global_obj().clone())
                                }
                                _ => {
                                    if let Ok(v) = this_obj.coerce_to_object(ctx) {
                                        v.into()
                                    } else {
                                        break State::Exception;
                                    }
                                }
                            }
                        } else {
                            this_obj.clone()
                        };
                        debug_assert!(!val.is_exception());

                        SP_PUT!(0, val);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_FALSE => {
                        SP_PUT!(0, JSValue::Bool(false));
                        SP_GO!(1);
                    }
                    OpCode::PUSH_TRUE => {
                        SP_PUT!(0, JSValue::Bool(true));
                        SP_GO!(1);
                    }
                    OpCode::OBJECT => {
                        SP_PUT!(
                            0,
                            ctx.create_base_object()
                                .map_or(JSValue::EXCEPTION, JSValue::Object)
                        );
                        SP_GO!(1);
                        if SP_REF!(-1).is_exception() {
                            break State::Exception;
                        }
                    }
                    OpCode::SPECIAL_OBJECT => {
                        let id = PC_FETCH!(u8);
                        debug_assert!(
                            (SpecialObject::Arguments as u8..=SpecialObject::ImportMeta as u8)
                                .contains(&id)
                        );
                        match unsafe { std::mem::transmute::<u8, SpecialObject>(id) } {
                            SpecialObject::Arguments => {
                                SP_PUT!(0, js_build_arguments(ctx, args));
                                SP_GO!(1);
                                if SP_REF!(-1).is_exception() {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::MappedArguments => {
                                SP_PUT!(
                                    0,
                                    js_build_mapped_arguments(
                                        ctx,
                                        args,
                                        sframe,
                                        std::cmp::min(
                                            args.len() as i32,
                                            self.0.borrow().arg_count as i32
                                        ),
                                    )
                                );
                                SP_GO!(1);
                                if SP_REF!(-1).is_exception() {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::ThisFunc => {
                                SP_PUT!(0, sframe.cur_running.clone());
                                SP_GO!(1);
                            }
                            SpecialObject::NewTarget => {
                                SP_PUT!(0, new_target.clone());
                                SP_GO!(1);
                            }
                            SpecialObject::HomeObject => {
                                SP_PUT!(
                                    0,
                                    if let Some(ho) = func_home_obj {
                                        JSValue::Object(ho.clone())
                                    } else {
                                        JSValue::UNDEFINED
                                    }
                                );
                                SP_GO!(1);
                            }
                            SpecialObject::VarObject => {
                                SP_PUT!(
                                    0,
                                    ctx.create_base_object_from_proto(JSValue::NULL)
                                        .map_or(JSValue::EXCEPTION, JSValue::Object)
                                );
                                SP_GO!(1);
                                if SP_REF!(-1).is_exception() {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::ImportMeta => {
                                SP_PUT!(0, js_import_meta(ctx));
                                SP_GO!(1);
                                if SP_REF!(-1).is_exception() {
                                    break State::Exception;
                                }
                            }
                        }
                    }
                    OpCode::REST => {
                        let first = PC_FETCH!(u16);
                        SP_PUT!(0, js_build_rest(ctx, first as _, args));
                        SP_GO!(1);
                        if SP_REF!(-1).is_exception() {
                            break State::Exception;
                        }
                    }
                    OpCode::DROP => {
                        SP_TAKE!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::NIP => {
                        // Stack operation: [a, b, ^] -> [b, ^] where ^ is stack top
                        // Removes the second value from top and moves top value down
                        SP_PUT!(-2, SP_TAKE!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::NIP1 => {
                        // Stack operation: [a, b, c, ^] -> [b, c, ^] where ^ is stack top
                        // Removes the third value from top and shifts remaining values down
                        SP_PUT!(-3, SP_TAKE!(-2));
                        SP_PUT!(-2, SP_TAKE!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::DUP => {
                        // Stack operation: [a, ^] -> [a, a, ^] where ^ is stack top
                        // Duplicates the top value on the stack
                        SP_PUT!(0, SP_REF!(-1).clone());
                        SP_GO!(1);
                    }
                    OpCode::DUP2 => {
                        // Stack operation: [a, b, ^] -> [a, b, a, b, ^] where ^ is stack top
                        // Duplicates the top two values on the stack
                        SP_PUT!(0, SP_REF!(-2).clone());
                        SP_PUT!(1, SP_REF!(-1).clone());
                        SP_GO!(2);
                    }
                    OpCode::DUP3 => {
                        // Stack operation: [a, b, c, ^] -> [a, b, c, a, b, c, ^] where ^ is stack top
                        // Duplicates the top three values on the stack
                        SP_PUT!(0, SP_REF!(-3).clone());
                        SP_PUT!(1, SP_REF!(-2).clone());
                        SP_PUT!(2, SP_REF!(-1).clone());
                        SP_GO!(3);
                    }
                    OpCode::DUP1 => {
                        // Stack operation: [a, b, ^] -> [a, a, b, ^] where ^ is stack top
                        // Duplicates the second value from top and places it below top
                        SP_PUT!(0, SP_TAKE!(-1));
                        SP_PUT!(-1, SP_REF!(-2).clone());
                        SP_GO!(1);
                    }
                    OpCode::INSERT2 => {
                        // Stack operation: [obj, a, ^] -> [a, obj, a, ^] where ^ is stack top
                        // Duplicates top value and inserts it under second position
                        SP_PUT!(0, SP_TAKE!(-1));
                        SP_PUT!(-1, SP_TAKE!(-2));
                        SP_PUT!(-2, SP_REF!(0).clone());
                        SP_GO!(1);
                    }
                    OpCode::INSERT3 => {
                        // Stack operation: [obj, prop, a, ^] -> [a, obj, prop, a, ^] where ^ is stack top
                        // Duplicates top value and inserts it under third position
                        // Uses array rotation for efficient stack manipulation
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 4) };
                        lst.rotate_right(1);

                        // After rotation, set SP![-3] to value in SP![0] duplicated
                        SP_PUT!(-3, SP_REF!(0).clone());

                        // SP![0] = SP![-1];
                        // SP![-1] = SP![-2];
                        // SP![-2] = SP![-3];
                        // SP![-3] = SP![0].clone();

                        SP_GO!(1);
                    }
                    OpCode::INSERT4 => {
                        // Stack operation: [this, obj, prop, a, ^] -> [a, this, obj, prop, a, ^] where ^ is stack top
                        // Duplicates top value and inserts it under fourth position
                        // Uses array rotation for efficient stack manipulation
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 5) };
                        lst.rotate_right(1);

                        // After rotation, SP![0] contains the value to duplicate and insert
                        SP_PUT!(-4, SP_REF!(0).clone());

                        // SP![0] = SP![-1];
                        // SP![-1] = SP![-2];
                        // SP![-2] = SP![-3];
                        // SP![-3] = SP![-4];
                        // SP![-4] = SP![0].clone();

                        SP_GO!(1); // move sp
                    }
                    OpCode::ROT3L => {
                        // Stack operation: [x, a, b, ^] -> [a, b, x, ^] where ^ is stack top
                        // Performs a left rotation on top three values
                        // The leftmost value moves to the right, others shift left
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 3) };
                        lst.rotate_left(1);

                        // let tmp = SP![-3];
                        // SP![-3] = SP![-2];
                        // SP![-2] = SP![-1];
                        // SP![0] = tmp;
                    }
                    OpCode::ROT4L => {
                        // Stack operation: [x, a, b, c, ^] -> [a, b, c, x, ^] where ^ is stack top
                        // Performs a left rotation on top four values
                        // The leftmost value moves to the right, others shift left
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 4) };
                        lst.rotate_left(1);

                        // let tmp = SP![-4];
                        // SP![-4] = SP![-3];
                        // SP![-3] = SP![-2];
                        // SP![-2] = SP![-1];
                        // SP![-1] = tmp;
                    }
                    OpCode::ROT5L => {
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-5), 5) };
                        lst.rotate_left(1);

                        // let tmp = SP![-5];
                        // SP![-5] = SP![-4];
                        // SP![-4] = SP![-3];
                        // SP![-3] = SP![-2];
                        // SP![-2] = SP![-1];
                        // SP![-1] = tmp;
                    }
                    OpCode::ROT3R => {
                        // Stack operation: [a, b, x, ^] -> [x, a, b, ^] where ^ is stack top
                        // Performs a right rotation on top three values
                        // The rightmost value moves to the left, others shift right
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 3) };
                        lst.rotate_right(1);

                        // let tmp = SP![-1];
                        // SP![-1] = SP![-2];
                        // SP![-2] = SP![-1];
                        // SP![-3] = tmp;
                    }
                    OpCode::PERM3 => {
                        // Stack operation: [obj, a, fixed, ^] -> [a, obj, fixed, ^] where ^ is stack top
                        // Swaps the second and third values from top, keeping the fixed value in place
                        unsafe {
                            std::ptr::swap(sp.offset(-2), sp.offset(-3));
                        }
                        // let tmp = SP![-2];
                        // SP![-2] = SP![-3];
                        // SP![-3] = tmp;
                    }
                    OpCode::PERM4 => {
                        // Stack operation: [obj, prop, a, fixed, ^] -> [a, obj, prop, fixed, ^] where ^ is stack top
                        // Performs a right rotation on three values before the fixed value
                        // The rightmost value moves left, others shift right, fixed value stays in place
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 3) };
                        lst.rotate_right(1);

                        // let tmp = SP![-2];
                        // SP![-2] = SP![-3];
                        // SP![-3] = SP![-4];
                        // SP![-4] = tmp;
                    }
                    OpCode::PERM5 => {
                        // Stack operation: [this, obj, prop, a, fixed, ^] -> [a, this, obj, prop, fixed, ^] where ^ is stack top
                        // Performs a right rotation on four values before the fixed value
                        // The rightmost value moves left, others shift right, fixed value stays in place
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-5), 4) };
                        lst.rotate_right(1);

                        // let tmp = SP![-2];
                        // SP![-2] = SP![-3];
                        // SP![-3] = SP![-4];
                        // SP![-4] = SP![-5];
                        // SP![-5] = tmp;
                    }
                    OpCode::SWAP => {
                        // Stack operation: [a, b, ^] -> [b, a, ^] where ^ is stack top
                        // Swaps the top two values on the stack
                        unsafe {
                            std::ptr::swap(sp.offset(-1), sp.offset(-2));
                        }
                    }
                    OpCode::SWAP2 => {
                        // Stack operation: [a, b, c, d, ^] -> [c, d, a, b, ^] where ^ is stack top
                        // Swaps pairs of values: first with third, second with fourth
                        // Effectively exchanges the top two pairs of values
                        unsafe {
                            std::ptr::swap(sp.offset(-1), sp.offset(-3));
                            std::ptr::swap(sp.offset(-2), sp.offset(-4));
                        }

                        // let tmp1 = SP![-4];
                        // let tmp2 = SP![-3];
                        // SP![-4] = SP![-2];
                        // SP![-3] = SP![-1];
                        // SP![-2] = tmp1;
                        // SP![-1] = tmp2;
                    }
                    OpCode::FCLOSURE => {
                        // [^] -> [closure_func, ^]
                        let id = PC_FETCH!(u32);
                        if let JSValue::Bytecode(bc) = CPOOL![id] {
                            SP_PUT!(
                                0,
                                bc.make_closure(ctx, closure_var_refs, Some(sframe))
                                    .unwrap_or(JSValue::EXCEPTION)
                            );
                            SP_GO!(1);
                            if SP_REF!(-1).is_exception() {
                                break State::Exception;
                            }
                        } else {
                            unreachable!()
                        }
                    }

                    #[cfg(feature = "short_opcode")]
                    OpCode::CALL0..=OpCode::CALL3 => {
                        todo!();
                    }

                    OpCode::CALL | OpCode::TAIL_CALL => {
                        let call_argc = PC_FETCH!(u16);
                        let call_argv = unsafe { sp.offset(-(call_argc as isize)) };

                        sframe.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        let ret_val = JS_CallInternal(
                            ctx,
                            unsafe { &*call_argv.offset(-1) },
                            &JSValue::UNDEFINED,
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if ret_val.is_exception() {
                            break State::Exception;
                        } else if opcode == OpCode::TAIL_CALL {
                            break State::Done(ret_val, false);
                        } else {
                            for i in -1..call_argc as isize {
                                drop(unsafe {
                                    std::ptr::replace(call_argv.offset(i), JSValue::UNDEFINED)
                                });
                            }
                            SP_GO!(-(call_argc as i32 + 1));

                            SP_PUT!(0, ret_val);
                            SP_GO!(1);
                        }
                    }
                    OpCode::CALL_CONSTRUCTOR => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        sframe.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        let ret_val = JS_CallConstructorInternal(
                            ctx,
                            unsafe { call_argv.offset(-2).as_ref().unwrap() },
                            unsafe { std::ptr::replace(call_argv.offset(-1), JSValue::UNDEFINED) },
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if ret_val.is_exception() {
                            break State::Exception;
                        } else {
                            for i in -2..call_argc {
                                drop(unsafe {
                                    std::ptr::replace(
                                        call_argv.offset(i as isize),
                                        JSValue::UNDEFINED,
                                    )
                                });
                            }
                            SP_GO!(-(call_argc + 2));

                            SP_PUT!(0, ret_val);
                            SP_GO!(1);
                        }
                    }
                    OpCode::CALL_METHOD | OpCode::TAIL_CALL_METHOD => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };

                        sframe.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        let ret_val = JS_CallInternal(
                            ctx,
                            unsafe { &*call_argv.offset(-1) },
                            unsafe { &*call_argv.offset(-2) },
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if ret_val.is_exception() {
                            break State::Exception;
                        }

                        if opcode == OpCode::TAIL_CALL_METHOD {
                            break State::Done(ret_val, false);
                        } else {
                            for i in -2..call_argc as isize {
                                drop(unsafe {
                                    std::ptr::replace(call_argv.offset(i), JSValue::UNDEFINED)
                                });
                            }
                            SP_GO!(-(call_argc + 2));

                            SP_PUT!(0, ret_val);
                            SP_GO!(1);
                        }
                    }
                    OpCode::ARRAY_FROM => {
                        // el1, el2 ... elN ^ -> array ^
                        let call_argc = PC_FETCH!(u16) as i32;

                        if let Ok(arr) = ctx.create_array() {
                            // push elements to array
                            let call_argv = unsafe { sp.offset(-call_argc as isize) };
                            for i in 0..call_argc as isize {
                                let val = unsafe {
                                    std::ptr::replace(call_argv.offset(i), JSValue::UNDEFINED)
                                };
                                let ret = arr.define_property_value(
                                    ctx,
                                    JSAtom::new_u32(i as u32),
                                    val,
                                    JS_PROP_C_W_E | JS_PROP_THROW,
                                );
                                if ret.is_err() {
                                    break 'opcodes State::Exception;
                                }
                            }
                            SP_GO!(-call_argc);

                            // push array on stack top
                            SP_PUT!(0, arr);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::APPLY => {
                        let magic = PC_FETCH!(u16) as i32;

                        let op_3 = SP_TAKE!(-3);
                        let op_2 = SP_TAKE!(-2);
                        let op_1 = SP_TAKE!(-1);
                        SP_GO!(-3);

                        if let Ok(res) = js_function_apply(
                            ctx,
                            CallContext::new(ctx, op_3, &[op_2, op_1], magic, None),
                        ) {
                            SP_PUT!(0, res);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::RETURN => {
                        SP_GO!(-1);
                        break State::Done(SP_TAKE!(0), false);
                    }
                    OpCode::RETURN_UNDEF => {
                        break State::Done(JSValue::UNDEFINED, false);
                    }
                    OpCode::CHECK_CTOR_RETURN => {
                        // return TRUE if 'this' should be returned
                        if !SP_REF!(-1).is_object() {
                            if !SP_REF!(-1).is_undefined() {
                                caller_ctx.throw_type_error(
                                    "derived class constructor must return an object or undefined",
                                );
                                break State::Exception;
                            } else {
                                SP_PUT!(0, JSValue::Bool(true));
                                SP_GO!(1);
                            }
                        } else {
                            SP_PUT!(0, JSValue::Bool(false));
                            SP_GO!(1);
                        }
                    }
                    OpCode::CHECK_CTOR => {
                        if new_target.is_undefined() {
                            ctx.throw_type_error("class constructors must be invoked with 'new'");
                            break State::Exception;
                        }
                    }
                    OpCode::CHECK_BRAND => {
                        if let Ok(b) = SP_REF!(-2).check_brand(ctx, SP_REF!(-1)) {
                            if !b {
                                ctx.throw_type_error("invalid brand on object");
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::ADD_BRAND => {
                        // [func_obj, home_obj, ^] -> [^]
                        let func_obj = SP_TAKE!(-2);
                        let home_object = SP_TAKE!(-1);
                        SP_GO!(-2);
                        if func_obj.add_brand(ctx, &home_object).is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::THROW => {
                        SP_GO!(-1);
                        ctx.throw(SP_TAKE!(0));
                        break State::Exception;
                    }
                    OpCode::THROW_ERROR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let type_ = PC_FETCH!(u8);

                        match type_ {
                            JS_THROW_VAR_RO => {
                                ctx.JS_ThrowTypeErrorReadOnly(JS_PROP_THROW, atom);
                            }
                            JS_THROW_VAR_REDECL => {
                                JS_ThrowSyntaxErrorVarRedeclaration(ctx, atom);
                            }
                            JS_THROW_VAR_UNINITIALIZED => {
                                JS_ThrowReferenceErrorUninitialized(ctx, atom);
                            }
                            JS_THROW_ERROR_DELETE_SUPER => {
                                JS_ThrowReferenceError(ctx, "unsupported reference to 'super'");
                            }
                            JS_THROW_ERROR_ITERATOR_THROW => {
                                ctx.throw_type_error("iterator does not have a throw method");
                            }
                            _ => {
                                ctx.throw_internal_error(&format!(
                                    "invalid throw var type {type_}"
                                ));
                            }
                        }
                        break State::Exception;
                    }
                    OpCode::EVAL => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        // scope_idx will be -1 for top of scope chain
                        let scope_idx = PC_FETCH!(u16) as i32 - 1;

                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        sframe.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        // function val
                        let func_val = unsafe { &*call_argv.offset(-1) };

                        // eval: call function
                        let ret_val = if func_val.is_same(&ctx.0.borrow().eval_func) {
                            if call_argc >= 1 {
                                // in case `eval(obj, ...)`
                                // where call_argv[0] is the `obj` arg, call_argv[-1] is `eval` func
                                let obj = unsafe { &*call_argv };
                                obj.eval_object(
                                    ctx,
                                    JSValue::UNDEFINED,
                                    JS_EVAL_TYPE_DIRECT,
                                    scope_idx as _,
                                )
                            } else {
                                // eval() returns undefined
                                JSValue::UNDEFINED
                            }
                        } else {
                            JS_CallInternal(
                                ctx,
                                func_val,
                                &JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                },
                                0,
                            )
                        };

                        // pop and drop values on stack since `func_val` position
                        {
                            for i in -1..call_argc as isize {
                                unsafe {
                                    drop(std::ptr::replace(
                                        call_argv.offset(i),
                                        JSValue::UNDEFINED,
                                    ));
                                }
                            }
                            sp = unsafe { sp.offset(-call_argc as isize - 1) };
                        }

                        if ret_val.is_exception() {
                            break State::Exception;
                        } else {
                            // push result value on stack
                            SP_PUT!(0, ret_val);
                            SP_GO!(1);
                        }
                    }
                    OpCode::APPLY_EVAL => {
                        let scope_idx = PC_FETCH!(u16) as i32 - 1; // NOTE: could be minus
                        let func_val = SP_REF!(-2);

                        if let Ok(arg_list) = build_arg_list(ctx, SP_REF!(-1)) {
                            let ret_val = if func_val.is_same(&ctx.0.borrow().eval_func) {
                                if !arg_list.is_empty() {
                                    // eval(obj, ...)
                                    arg_list[0].eval_object(
                                        ctx,
                                        JSValue::UNDEFINED,
                                        JS_EVAL_TYPE_DIRECT,
                                        scope_idx,
                                    )
                                } else {
                                    // eval() returns undefined
                                    JSValue::UNDEFINED
                                }
                            } else {
                                JS_Call(ctx, func_val, &JSValue::UNDEFINED, &arg_list)
                            };

                            if ret_val.is_exception() {
                                break State::Exception;
                            } else {
                                SP_TAKE!(-2);
                                SP_TAKE!(-1);
                                SP_GO!(-2);
                                SP_PUT!(0, ret_val);
                                SP_GO!(1);
                            }
                        } else {
                            break State::Exception;
                        };
                    }
                    OpCode::REGEXP => {
                        let res = unsafe {
                            js_regexp_constructor_internal(
                                ctx,
                                JSValue::UNDEFINED,
                                SP_TAKE!(-2),
                                SP_TAKE!(-1),
                            )
                        };
                        SP_PUT!(-2, res);
                        SP_GO!(-1);
                    }
                    OpCode::GET_SUPER => {
                        if let Ok(proto) = SP_REF!(-1).prototype(ctx) {
                            SP_PUT!(-1, proto);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::IMPORT => {
                        if let Ok(res) = js_dynamic_import(ctx, SP_REF!(-1)) {
                            SP_PUT!(-1, res);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::CHECK_VAR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        if let Ok(b) = ctx.check_global_var(atom) {
                            SP_PUT!(0, JSValue::Bool(b));
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_VAR_UNDEF | OpCode::GET_VAR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let val = ctx.get_global_var(atom, opcode == OpCode::GET_VAR);
                        if val.is_exception() {
                            break State::Exception;
                        } else {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        }
                    }
                    OpCode::PUT_VAR | OpCode::PUT_VAR_INIT => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let res = ctx.set_global_var(
                            atom,
                            SP_TAKE!(-1),
                            (opcode as u8 - OpCode::PUT_VAR as u8) as _,
                        );
                        SP_GO!(-1);
                        if res.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_VAR_STRICT => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        // SP[-2] is TRUE or FALSE
                        if let JSValue::Bool(false) = SP_TAKE!(-2) {
                            JS_ThrowReferenceErrorNotDefined(ctx, atom);
                            break State::Exception;
                        } else {
                            let ret = ctx.set_global_var(atom, SP_TAKE!(-1), 2);
                            SP_GO!(-2);
                            if ret.is_err() {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::CHECK_DEFINE_VAR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        if ctx.define_global_var_checked(atom, flags as _).is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_VAR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        if ctx.define_global_var(atom, flags as _).is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_FUNC => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        let op_1 = SP_TAKE!(-1);
                        SP_GO!(-1);
                        if ctx.define_global_function(atom, &op_1, flags as _).is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_LOC => {
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, VAR_REF!(idx).clone());
                        SP_GO!(1);
                    }
                    OpCode::PUT_LOC => {
                        // [value, ^] -> [^]
                        let idx = PC_FETCH!(u16);
                        let _ = std::mem::replace(VAR_REF![idx], SP_TAKE!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::SET_LOC => {
                        let idx = PC_FETCH!(u16);
                        let _ = std::mem::replace(VAR_REF![idx], SP_REF!(-1).clone());
                    }
                    OpCode::SET_LOC_UNINITIALIZED => {
                        let idx = PC_FETCH!(u16);
                        let _ = std::mem::replace(VAR_REF![idx], JSValue::UNINITIALIZED);
                    }
                    OpCode::GET_ARG => {
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, ARG_REF!(idx).clone());
                        SP_GO!(1);
                    }
                    OpCode::PUT_ARG => {
                        let idx = PC_FETCH!(u16);
                        let _ = std::mem::replace(ARG_REF![idx], SP_TAKE!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::SET_ARG => {
                        let idx = PC_FETCH!(u16);
                        let _ = std::mem::replace(ARG_REF![idx], SP_REF!(-1).clone());
                    }

                    #[cfg(feature = "short_opcode")]
                    OpCode::GET_LOC8..=OpCode::SET_VAR_REF3 => {
                        todo!()
                    }

                    OpCode::GET_VAR_REF => {
                        // get value from var_ref, duplicate and push it on stack
                        // Stack operation: [^] -> [value, ^] where ^ is stack top
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, closure_var_refs[idx as usize].value().clone());
                        SP_GO!(1);
                    }
                    OpCode::PUT_VAR_REF => {
                        // [value, ^] -> [^]
                        // consume and store stack top value to var_ref
                        let idx = PC_FETCH!(u16);
                        closure_var_refs[idx as usize].set_value(SP_TAKE!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::SET_VAR_REF => {
                        let idx = PC_FETCH!(u16);
                        closure_var_refs[idx as usize].set_value(SP_REF!(-1).clone());
                    }
                    OpCode::GET_VAR_REF_CHECK | OpCode::PUT_VAR_REF_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let val = closure_var_refs[idx as usize].value();
                        if val.is_uninitialized() {
                            JS_ThrowReferenceErrorUninitialized2(ctx, self, idx, true);
                            break State::Exception;
                        }

                        if opcode == OpCode::GET_VAR_REF_CHECK {
                            // GET_VAR_REF_CHECK
                            SP_PUT!(0, val.clone());
                            SP_GO!(1);
                        } else {
                            // PUT_VAR_REF_CHECK
                            closure_var_refs[idx as usize].set_value(SP_TAKE!(-1));
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_VAR_REF_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        if !closure_var_refs[idx as usize].value().is_uninitialized() {
                            JS_ThrowReferenceErrorUninitialized2(ctx, self, idx, true);
                            break State::Exception;
                        } else {
                            closure_var_refs[idx as usize].set_value(SP_TAKE!(-1));
                            SP_GO!(-1);
                        }
                    }

                    OpCode::GET_LOC_CHECK | OpCode::GET_LOC_CHECKTHIS => {
                        let idx = PC_FETCH!(u16);
                        let val = VAR_REF![idx];
                        if val.is_uninitialized() {
                            JS_ThrowReferenceErrorUninitialized2(
                                if opcode == OpCode::GET_LOC_CHECK {
                                    ctx
                                } else {
                                    caller_ctx
                                },
                                self,
                                idx,
                                false,
                            );
                            break State::Exception;
                        } else {
                            SP_PUT!(0, val.clone());
                            SP_GO!(1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK | OpCode::PUT_LOC_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if pval.is_uninitialized() {
                            if opcode == OpCode::PUT_LOC_CHECK {
                                JS_ThrowReferenceErrorUninitialized2(ctx, self, idx, false);
                            } else {
                                JS_ThrowReferenceError(ctx, "'this' can be initialized only once");
                            }
                            break State::Exception;
                        } else {
                            *pval = SP_TAKE!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::CLOSE_LOC => {
                        let idx = PC_FETCH!(u16);
                        sframe.close_lexical_var(ctx, idx, false);
                    }
                    OpCode::MAKE_LOC_REF | OpCode::MAKE_ARG_REF | OpCode::MAKE_VAR_REF_REF => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(
                            0,
                            ctx.create_base_object_from_proto(JSValue::NULL)
                                .map_or(JSValue::EXCEPTION, JSValue::Object)
                        );
                        SP_GO!(1);
                        if SP_REF!(-1).is_exception() {
                            break State::Exception;
                        }

                        let var_ref = if opcode == OpCode::MAKE_VAR_REF_REF {
                            Ok(closure_var_refs[idx as usize].clone())
                        } else {
                            sframe.get_var_ref(ctx, idx, opcode == OpCode::MAKE_ARG_REF)
                        };
                        match var_ref {
                            Ok(vr) => unsafe {
                                if SP_REF!(-1)
                                    .object_unchecked()
                                    .add_shape_property_then(
                                        ctx,
                                        atom,
                                        JS_PROP_WRITABLE | JS_PROP_VARREF,
                                        |pr| {
                                            *pr = JSProperty::VarRef(vr);
                                            SP_PUT!(0, atom.to_value(ctx, false));
                                            SP_GO!(1);
                                        },
                                    )
                                    .is_err()
                                {
                                    break State::Exception;
                                };
                            },
                            Err(_) => {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::MAKE_VAR_REF => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        if let Ok(vs) = ctx.build_global_varref(atom) {
                            if let Some((v0, v1)) = vs {
                                SP_PUT!(0, v0);
                                SP_PUT!(1, v1);
                            }
                            SP_GO!(2);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::GOTO => {
                        let n = unsafe { read_from_ptr::<i32>(pc) }; // could be minus
                        PC_GO!(n);

                        if ctx.poll_interrupts().is_err() {
                            break State::Exception;
                        }
                    }

                    #[cfg(feature = "short_opcode")]
                    OpCode::GOTO8 | OpCode::GOTO16 => {
                        pc = pc.add(if opcode == OpCode::GOTO8 {
                            get_u8(pc)
                        } else {
                            read_from_ptr::<u16>(pc)
                        } as usize);

                        if js_poll_interrupts(ctx) != 0 {
                            state = State::Exception;
                        }
                    }

                    OpCode::IF_TRUE | OpCode::IF_FALSE => {
                        let b = opcode == OpCode::IF_TRUE;
                        let op_1 = SP_TAKE!(-1);
                        SP_GO!(-1);

                        // skip jump offset
                        PC_GO!(4);

                        let res = match op_1 {
                            JSValue::Int(i) => i != 0,
                            JSValue::Bool(b) => b,
                            JSValue::UNDEFINED | JSValue::NULL => false,
                            _ => {
                                if let Ok(x) = op_1.coerce_to_bool(ctx) {
                                    x
                                } else {
                                    break State::Exception;
                                }
                            }
                        };

                        if b == res {
                            // jump pc
                            pc = unsafe {
                                pc.offset((read_from_ptr::<i32>(pc.offset(-4)) - 4) as isize)
                            };
                        }

                        if ctx.poll_interrupts().is_err() {
                            break State::Exception;
                        }
                    }

                    #[cfg(feature = "short_opcode")]
                    OpCode::IF_TRUE8 | OpCode::IF_FALSE8 => {
                        todo!()
                    }

                    OpCode::CATCH => {
                        let diff = unsafe { read_from_ptr::<i32>(pc) };
                        SP_PUT!(
                            0,
                            JSValue::CatchOffset(pc.offset_from(pc_base) as i32 + diff)
                        );
                        SP_GO!(1);
                        PC_GO!(4);
                    }
                    OpCode::GOSUB => {
                        let diff = unsafe { read_from_ptr::<i32>(pc) }; // <-- PC ptr unchanged
                        // XXX: should have a different tag to avoid security flaw
                        SP_PUT!(0, JSValue::Int(pc.offset_from(pc_base) as i32 + 4));
                        SP_GO!(1);
                        PC_GO!(diff);
                    }
                    OpCode::RET => {
                        if let JSValue::Int(i) = SP_REF!(-1) {
                            let pos = *i;
                            if pos >= bytecodes.len() as i32 {
                                ctx.throw_internal_error("invalid ret value");
                                break State::Exception;
                            } else {
                                SP_TAKE!(-1);
                                SP_GO!(-1);
                                // jump pc
                                pc = unsafe { pc_base.offset(pos as isize) };
                            }
                        } else {
                            ctx.throw_internal_error("invalid ret value");
                            break State::Exception;
                        }

                        // let op1 = SP![-1];
                        // if JS_VALUE_GET_TAG(op1) != JS_TAG_INT {
                        //     ctx.JS_ThrowInternalError(c"invalid ret value");
                        //     break State::Exception;
                        // } else {
                        //     let pos = JS_VALUE_GET_INT(op1);
                        //     if pos >= bytecodes.len() as i32 {
                        //         ctx.JS_ThrowInternalError(c"invalid ret value");
                        //         break State::Exception;
                        //     } else {
                        //         SP_GO!(-1);
                        //         pc = pc_base.offset(pos as isize);
                        //     }
                        // }
                    }
                    OpCode::FOR_IN_START => {
                        if js_for_in_start(ctx, sp).is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::FOR_IN_NEXT => {
                        if unsafe { js_for_in_next(ctx, sp) } != 0 {
                            break State::Exception;
                        }
                        SP_GO!(2);
                    }
                    OpCode::FOR_OF_START => {
                        if js_for_of_start(ctx, sp, false) != 0 {
                            break State::Exception;
                        }
                        SP_GO!(1);
                        SP_PUT!(0, JSValue::CatchOffset(0));
                        SP_GO!(1);
                    }
                    OpCode::FOR_OF_NEXT => {
                        let offset = -3 - PC_FETCH!(u8) as i32;
                        if unsafe { js_for_of_next(ctx, sp, offset).is_err() } {
                            break State::Exception;
                        }
                        SP_GO!(2);
                    }
                    OpCode::FOR_AWAIT_OF_START => {
                        if js_for_of_start(ctx, sp, true) != 0 {
                            break State::Exception;
                        } else {
                            SP_GO!(1);
                            SP_PUT!(0, JSValue::CatchOffset(0));
                            SP_GO!(1);
                        }
                    }
                    OpCode::ITERATOR_GET_VALUE_DONE => {
                        if unsafe { js_iterator_get_value_done(ctx, sp) } != 0 {
                            break State::Exception;
                        } else {
                            SP_GO!(1);
                        }
                    }
                    OpCode::ITERATOR_CHECK_OBJECT => {
                        if !SP_REF!(-1).is_object() {
                            ctx.throw_type_error("iterator must return an object");
                            break State::Exception;
                        }
                    }
                    OpCode::ITERATOR_CLOSE => {
                        // [iter_obj, next, catch_offset, ^] -> [^]
                        SP_TAKE!(-1); // drop the catch offset (to avoid getting caught by exception)
                        SP_TAKE!(-2); // drop the next method
                        SP_GO!(-2);

                        // SP[-1] is now iter_obj
                        if !SP_REF!(-1).is_undefined() {
                            if JS_IteratorClose(ctx, SP_REF!(-1), false).is_err() {
                                // leave iter_obj in stack
                                break State::Exception;
                            } else {
                                SP_TAKE!(-1);
                            }
                        }
                        // pop iter_obj
                        SP_GO!(-1);
                    }
                    OpCode::NIP_CATCH => {
                        // [catch_offset, ..., ret_val, ^] -> [ret_eval, ^]
                        SP_GO!(-1);
                        // keep ret_val
                        let ret_val = SP_TAKE!(0);
                        // drop ... values
                        while sp > sp_base && !matches!(SP_REF!(-1), JSValue::CatchOffset(_)) {
                            SP_GO!(-1);
                            drop(SP_TAKE!(0));
                        }
                        if sp == sp_base {
                            ctx.throw_internal_error("nip_catch");
                            break State::Exception;
                        } else {
                            SP_PUT!(-1, ret_val);
                        }
                    }
                    OpCode::ITERATOR_NEXT => {
                        // [iter_obj, next, catch_offset, val, ^] -> [iter_obj, next, catch_offset, val2, ^]
                        // [      -4,   -3,           -2,  -1, 0]
                        let ret = JS_Call(
                            ctx,
                            SP_REF!(-3),                       // next
                            SP_REF!(-4),                       // iter_obj
                            std::slice::from_ref(SP_REF!(-1)), // val
                        );
                        if ret.is_exception() {
                            break State::Exception;
                        } else {
                            SP_PUT!(-1, ret);
                        }
                    }
                    OpCode::ITERATOR_CALL => {
                        // [iter_obj, next, catch_offset, val, ^] -> [iter_obj, next, catch_offset, val2, bool, ^]
                        // [      -4,   -3,           -2,  -1, 0]
                        let flags = PC_FETCH!(u8);

                        let method = SP_REF!(-4).get_property(
                            ctx,
                            if flags & 1 != 0 {
                                JS_ATOM_throw
                            } else {
                                JS_ATOM_return
                            },
                        );
                        if method.is_exception() {
                            break State::Exception;
                        }

                        let ret_flag = if method.is_undefined_or_null() {
                            true
                        } else {
                            let ret = JS_Call(
                                ctx,
                                &method,
                                SP_REF!(-4),
                                if flags & 2 != 0 {
                                    &[]
                                } else {
                                    std::slice::from_ref(SP_REF!(-1))
                                },
                            );
                            if ret.is_exception() {
                                break State::Exception;
                            } else {
                                SP_PUT!(-1, ret);
                            }
                            false
                        };

                        SP_PUT!(0, JSValue::Bool(ret_flag));
                        SP_GO!(1);
                    }
                    OpCode::LNOT => {
                        let op_1 = SP_REF!(-1);
                        let b = match op_1 {
                            JSValue::Int(i) => *i != 0,
                            JSValue::Bool(b) => *b,
                            JSValue::NULL | JSValue::UNDEFINED => false,
                            _ => {
                                if let Ok(b) = op_1.coerce_to_bool(ctx) {
                                    b
                                } else {
                                    break State::Exception;
                                }
                            }
                        };
                        SP_PUT!(-1, JSValue::Bool(!b)); // not
                    }
                    OpCode::GET_FIELD | OpCode::GET_FIELD2 => {
                        // get_field: [obj, ^] -> [value, ^]
                        // get_field2: [obj, ^] -> [obj, value, ^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        let val = SP_REF!(-1).get_property(ctx, atom);
                        if val.is_exception() {
                            break State::Exception;
                        } else if opcode == OpCode::GET_FIELD {
                            SP_PUT!(-1, val);
                        } else {
                            SP_PUT!(0, val);
                            SP_GO!(1)
                        }
                    }
                    OpCode::PUT_FIELD => {
                        // [this_obj, value, ^] -> [^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        let this_obj = SP_TAKE!(-2);
                        let value = SP_TAKE!(-1);
                        SP_GO!(-2);

                        if JSValue::set_property_internal(
                            ctx,
                            &this_obj,
                            atom,
                            value,
                            &this_obj,
                            JS_PROP_THROW_STRICT,
                        )
                        .is_err()
                        {
                            break State::Exception;
                        }
                    }
                    OpCode::PRIVATE_SYMBOL => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        if let Ok(val) = ctx.create_symbol_from_atom(atom, AtomType::Private) {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_PRIVATE_FIELD => {
                        let op_2 = SP_TAKE!(-2);
                        let op_1 = SP_TAKE!(-1);
                        let val = op_2
                            .get_private_field(ctx, &op_1)
                            .unwrap_or(JSValue::EXCEPTION);
                        SP_PUT!(-2, val.clone());
                        SP_GO!(-1);

                        if val.is_exception() {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [^]
                        let ret = SP_TAKE!(-3).set_private_field(ctx, &SP_TAKE!(-1), SP_TAKE!(-2));
                        SP_GO!(-3);
                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [this_obj, ^]
                        let ret =
                            SP_REF!(-3).define_private_field(ctx, &SP_TAKE!(-2), SP_TAKE!(-1));
                        SP_GO!(-2);
                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_FIELD => {
                        // [this_obj, value, ^] -> [this_obj, ^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        let ret = SP_REF!(-2).define_property_value(
                            ctx,
                            atom,
                            SP_TAKE!(-1),
                            JS_PROP_C_W_E | JS_PROP_THROW,
                        );
                        SP_GO!(-1);
                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_NAME => {
                        // [this_obj, ^] -> [this_obj, ^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        let ret = unsafe {
                            JS_DefineObjectName(ctx, SP_REF!(-1), atom, JS_PROP_CONFIGURABLE)
                        };
                        if ret < 0 {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_NAME_COMPUTED => {
                        // [this_obj, name, ^] -> [this_obj, name, ^]
                        let ret = unsafe {
                            JS_DefineObjectNameComputed(
                                ctx,
                                SP_REF!(-1),
                                SP_REF!(-2),
                                JS_PROP_CONFIGURABLE,
                            )
                        };
                        if ret < 0 {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_PROTO => {
                        // [this_obj, proto, ^] -> [this_obj, ^]
                        let proto = SP_TAKE!(-1);
                        if proto.is_object() || proto.is_null() {
                            if JS_SetPrototypeInternal(ctx, SP_REF!(-2), proto, true).is_err() {
                                break State::Exception;
                            }
                            SP_GO!(-1);
                        }
                    }
                    OpCode::SET_HOME_OBJECT => {
                        // [home_obj, func, ^] -> [home_obj, func, ^]
                        js_method_set_home_object(SP_REF!(-1), SP_REF!(-2).object());
                    }
                    OpCode::DEFINE_METHOD | OpCode::DEFINE_METHOD_COMPUTED => {
                        // DEFINE_METHOD_COMPUTED:
                        // [this_obj, name, value, ^] -> [this_obj, ^]
                        // DEFINE_METHOD:
                        // [this_obj, value, ^] -> [this_obj, ^]
                        let (is_computed, mut name, op_flags) =
                            if opcode == OpCode::DEFINE_METHOD_COMPUTED {
                                // format u8
                                (true, JS_ATOM_NULL, PC_FETCH!(u8))
                            } else {
                                // format atom_u8
                                (false, JSAtom(PC_FETCH!(u32)), PC_FETCH!(u8))
                            };

                        if name == JS_ATOM_NULL {
                            // compute name from value SP[-2]
                            name = if let Ok(a) = SP_REF!(-2).coerce_to_atom(ctx) {
                                a
                            } else {
                                break State::Exception;
                            };
                        }

                        let mut flags = JS_PROP_HAS_CONFIGURABLE
                            | JS_PROP_CONFIGURABLE
                            | JS_PROP_HAS_ENUMERABLE
                            | JS_PROP_THROW;

                        if op_flags & OP_DEFINE_METHOD_ENUMERABLE != 0 {
                            flags |= JS_PROP_ENUMERABLE
                        }

                        let this_obj = SP_REF!(if is_computed { -3 } else { -2 });

                        let func_obj: &JSValue;
                        let mut value = JSValue::UNDEFINED;
                        let mut getter = JSValue::UNDEFINED;
                        let mut setter = JSValue::UNDEFINED;

                        match op_flags & (OP_DEFINE_METHOD_GETTER | OP_DEFINE_METHOD_SETTER) {
                            OP_DEFINE_METHOD_METHOD => {
                                value = SP_TAKE!(-1);
                                func_obj = &value;
                                flags |=
                                    JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE;
                            }
                            OP_DEFINE_METHOD_GETTER => {
                                getter = SP_TAKE!(-1);
                                func_obj = &getter;
                                flags |= JS_PROP_HAS_GET;
                            }
                            OP_DEFINE_METHOD_SETTER => {
                                setter = SP_TAKE!(-1);
                                func_obj = &setter;
                                flags |= JS_PROP_HAS_SET;
                            }
                            _ => unreachable!(),
                        }
                        let mut ret =
                            js_method_set_properties(ctx, func_obj, name, flags, this_obj);
                        if ret >= 0 {
                            ret = this_obj
                                .define_property(ctx, name, &value, &getter, &setter, flags)
                                .map_or(-1, |b| b as i32);
                        }

                        if is_computed {
                            JS_FreeAtom(ctx, name);
                            SP_TAKE!(-2);
                            SP_GO!(-2);
                        } else {
                            SP_GO!(-1);
                        }

                        if ret < 0 {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_CLASS | OpCode::DEFINE_CLASS_COMPUTED => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let class_flags = PC_FETCH!(u8) as u32;

                        if js_op_define_class(
                            ctx,
                            sp,
                            atom,
                            class_flags as _,
                            closure_var_refs,
                            sframe,
                            opcode == OpCode::DEFINE_CLASS_COMPUTED,
                        )
                        .is_err()
                        {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL => {
                        // [array, prop, ^] -> [value, ^]
                        let val = SP_REF!(-2).get_property_value(ctx, &SP_TAKE!(-1));
                        SP_PUT!(-2, val.clone());
                        SP_GO!(-1);
                        if val.is_exception() {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL2 => {
                        // [array, prop, ^] -> [array, value, ^]
                        let val = SP_REF!(-2).get_property_value(ctx, SP_REF!(-1));
                        SP_PUT!(-1, val);
                        if SP_REF!(-1).is_exception() {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_REF_VALUE => {
                        // [obj, prop, ^] -> [obj, value, ^]
                        if SP_REF!(-2).is_undefined() {
                            if let Ok(atom) = SP_REF!(-1).coerce_to_atom(ctx) {
                                JS_ThrowReferenceErrorNotDefined(ctx, atom);
                                JS_FreeAtom(ctx, atom);
                            }
                            break State::Exception;
                        } else {
                            let val = SP_REF!(-2).get_property_value(ctx, SP_REF!(-1));
                            if val.is_exception() {
                                break State::Exception;
                            } else {
                                SP_PUT!(0, val);
                                SP_GO!(1);
                            }
                        }
                    }
                    OpCode::GET_SUPER_VALUE => {
                        if let Ok(atom) = SP_REF!(-1).coerce_to_atom(ctx) {
                            let val =
                                SP_REF!(-2).get_property_internal(ctx, atom, SP_REF!(-3), false);
                            JS_FreeAtom(ctx, atom);
                            if val.is_exception() {
                                break State::Exception;
                            }

                            SP_TAKE!(-1);
                            SP_TAKE!(-2);
                            SP_PUT!(-3, val);
                            SP_GO!(-2);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_ARRAY_EL => {
                        // [obj, idx_prop, value, ^] -> [^]
                        let ret = SP_TAKE!(-3).set_property_value(
                            ctx,
                            &SP_TAKE!(-2),
                            SP_TAKE!(-1),
                            JS_PROP_THROW_STRICT,
                        );
                        SP_GO!(-3);
                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_REF_VALUE => {
                        // [obj, prop, val, ^] -> [^]
                        let mut obj = SP_TAKE!(-3);
                        let prop = SP_TAKE!(-2);
                        let val = SP_TAKE!(-1);
                        SP_GO!(-3);

                        let mut flags = JS_PROP_THROW_STRICT;

                        if obj.is_undefined() {
                            if is_strict_mode(ctx) {
                                if let Ok(atom) = prop.coerce_to_atom(ctx) {
                                    JS_ThrowReferenceErrorNotDefined(ctx, atom);
                                    JS_FreeAtom(ctx, atom);
                                }
                                break State::Exception;
                            }
                            obj = JSValue::Object(ctx.global_obj().clone());
                        } else {
                            if is_strict_mode(ctx) {
                                flags |= JS_PROP_NO_ADD;
                            }
                        }

                        let ret = obj.set_property_value(ctx, &prop, val, flags);
                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_SUPER_VALUE => {
                        // [this_obj, obj, prop, val, ^] -> [^]
                        let this_obj = SP_TAKE!(-4);
                        let obj = SP_TAKE!(-3);
                        let prop = SP_TAKE!(-2);
                        let val = SP_TAKE!(-1);
                        SP_GO!(-4);

                        if !obj.is_object() {
                            ctx.throw_type_error_not_an_object();
                            break State::Exception;
                        }

                        if let Ok(atom) = prop.coerce_to_atom(ctx) {
                            let ret = JSValue::set_property_internal(
                                ctx,
                                &obj,
                                atom,
                                val,
                                &this_obj,
                                JS_PROP_THROW_STRICT,
                            );
                            JS_FreeAtom(ctx, atom);
                            if ret.is_err() {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_ARRAY_EL => {
                        // [array, prop, value, ^] -> [array, prop, ^]
                        let ret = SP_REF!(-3).define_property_value_value(
                            ctx,
                            SP_REF!(-2).clone(),
                            SP_TAKE!(-1),
                            JS_PROP_C_W_E | JS_PROP_THROW,
                        );
                        SP_GO!(-1);
                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::APPEND => {
                        // [obj, start_pos, src, ^] -> [obj, end_pos, ^]
                        // all elements from `src` are appended to `dest` array, staring from `start_pos` index
                        // 操作完成后, `SP[-3]`数组中加入了来自`SP[-1]`的全部元素，
                        // `SP[-1]`没有变化，`SP[-2]`更新为结束位置index。
                        if unsafe { js_append_enumerate(ctx, sp).is_err() } {
                            break State::Exception;
                        } else {
                            // pop `src`
                            SP_TAKE!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::COPY_DATA_PROPERTIES => {
                        /* target source excludeList */
                        /* stack offsets (-1 based):
                        2 bits for target,
                        3 bits for source,
                        2 bits for exclusionList */
                        let mask = PC_FETCH!(u8) as i32;
                        if SP_REF!(-1 - (mask & 3))
                            .copy_properties_from(
                                ctx,
                                SP_REF!(-1 - ((mask >> 2) & 7)),
                                SP_REF!(-1 - ((mask >> 5) & 7)),
                                false,
                            )
                            .is_err()
                        {
                            break State::Exception;
                        }
                    }

                    OpCode::ADD
                    | OpCode::SUB
                    | OpCode::MUL
                    | OpCode::DIV
                    | OpCode::MOD
                    | OpCode::POW => {
                        let op1 = SP_TAKE!(-2);
                        let op2 = SP_TAKE!(-1);
                        let res = match opcode {
                            OpCode::ADD => JSValue::arith_add(ctx, op1, op2),
                            OpCode::SUB => JSValue::arith_sub(ctx, op1, op2),
                            OpCode::MUL => JSValue::arith_mul(ctx, op1, op2),
                            OpCode::DIV => JSValue::arith_div(ctx, op1, op2),
                            OpCode::MOD => JSValue::arith_mod(ctx, op1, op2),
                            OpCode::POW => JSValue::arith_pow(ctx, op1, op2),
                            _ => unreachable!(),
                        };

                        if let Ok(val) = res {
                            SP_PUT!(-2, val);
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::ADD_LOC => {
                        let idx = PC_FETCH!(u8) as i32;
                        let op1 = SP_TAKE!(-1);
                        SP_GO!(-1);

                        let pv = VAR_REF![idx];
                        if let Ok(val) = JSValue::arith_add(ctx, pv.clone(), op1) {
                            *pv = val;
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PLUS | OpCode::NEG | OpCode::INC | OpCode::DEC => {
                        let op1 = SP_TAKE!(-1);
                        if opcode == OpCode::PLUS
                            && matches!(op1, JSValue::Int(_) | JSValue::Float64(_))
                        {
                            // + before an int or float: do nothing
                        } else {
                            if let Ok(res) =
                                UnaryOp::new(ctx, op1, true).map(|unary| match opcode {
                                    OpCode::PLUS => unary.plus(),
                                    OpCode::NEG => unary.neg(),
                                    OpCode::INC => unary.inc(),
                                    OpCode::DEC => unary.dec(),
                                    _ => unreachable!(),
                                })
                            {
                                SP_PUT!(-1, res);
                            } else {
                                SP_PUT!(-1, JSValue::UNDEFINED);
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::POST_INC | OpCode::POST_DEC => {
                        let op1 = SP_TAKE!(-1);
                        if let Ok(n1) = op1.coerce_to_number(ctx) {
                            SP_PUT!(-1, n1.clone());
                            if let Ok(res) = UnaryOp::new(ctx, n1, true).map(|unary| match opcode {
                                OpCode::POST_INC => unary.inc(),
                                OpCode::POST_DEC => unary.dec(),
                                _ => unreachable!(),
                            }) {
                                SP_PUT!(0, res);
                                SP_GO!(1);
                            } else {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::INC_LOC | OpCode::DEC_LOC => {
                        let idx = PC_FETCH!(u8) as i32;

                        let op1 = VAR_REF![idx];

                        // must duplicate `op1` otherwise the variable value may be destroyed before JS code accesses it
                        if let Ok(res) =
                            UnaryOp::new(ctx, op1.clone(), true).map(|unary| match opcode {
                                OpCode::INC_LOC => unary.inc(),
                                OpCode::DEC_LOC => unary.dec(),
                                _ => unreachable!(),
                            })
                        {
                            *op1 = res;
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::NOT => {
                        if let Ok(res) =
                            UnaryOp::new_int(ctx, SP_TAKE!(-1)).map(|unary| unary.not())
                        {
                            SP_PUT!(-1, res);
                        } else {
                            SP_PUT!(-1, JSValue::UNDEFINED);
                            break State::Exception;
                        }
                    }
                    OpCode::SHL
                    | OpCode::SHR
                    | OpCode::SAR
                    | OpCode::AND
                    | OpCode::OR
                    | OpCode::XOR => {
                        if let Ok(new_sp) = op_binary_logic(ctx, opcode, sp) {
                            sp = new_sp;
                        } else {
                            break State::Exception;
                        }
                    }

                    OpCode::LT | OpCode::LTE | OpCode::GT | OpCode::GTE => {
                        let op1 = SP_TAKE!(-2);
                        let op2 = SP_TAKE!(-1);

                        if let Ok(rel) = op1.arith_compare(ctx, &op2) {
                            let b = match (rel, opcode) {
                                (Some(std::cmp::Ordering::Less), OpCode::LT | OpCode::LTE)
                                | (Some(std::cmp::Ordering::Greater), OpCode::GT | OpCode::GTE)
                                | (Some(std::cmp::Ordering::Equal), OpCode::LTE | OpCode::GTE) => {
                                    true
                                }
                                _ => false,
                            };
                            SP_PUT!(-2, JSValue::Bool(b));
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::EQ | OpCode::NEQ => {
                        let op1 = SP_TAKE!(-2);
                        let op2 = SP_TAKE!(-1);
                        if let Ok(b) = js_eq(ctx, op1, op2) {
                            SP_PUT!(
                                -2,
                                JSValue::Bool(if opcode == OpCode::NEQ { !b } else { b })
                            );
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::STRICT_EQ | OpCode::STRICT_NEQ => {
                        let mut b = SP_TAKE!(-2).strict_eq(&SP_TAKE!(-1));
                        if opcode == OpCode::STRICT_NEQ {
                            b = !b;
                        }
                        SP_PUT!(-2, JSValue::Bool(b));
                        SP_GO!(-1);
                    }

                    OpCode::IN => {
                        if js_operator_in(ctx, sp).is_err() {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PRIVATE_IN => {
                        if js_operator_private_in(ctx, sp).is_err() {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::INSTANCEOF => {
                        if js_operator_instanceof(ctx, sp).is_err() {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::TYPEOF => {
                        let op1 = SP_PUT!(-1, JSValue::UNDEFINED);
                        let atom = op1.type_of();
                        SP_PUT!(-1, atom.to_value(ctx, true));
                    }
                    OpCode::DELETE => {
                        if js_operator_delete(ctx, sp) != 0 {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::DELETE_VAR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        match ctx.global_obj().delete_property(ctx, atom) {
                            Ok(ret) => {
                                SP_PUT!(0, JSValue::Bool(ret));
                                SP_GO!(1);
                            }
                            Err(_) => {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::TO_OBJECT => {
                        if !SP_REF!(-1).is_object() {
                            if let Ok(o) = SP_REF!(-1).coerce_to_object(ctx) {
                                SP_PUT!(-1, o.into());
                            } else {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::TO_PROPKEY | OpCode::TO_PROPKEY2 => {
                        if opcode == OpCode::TO_PROPKEY2 {
                            // must be tested first
                            if SP_REF!(-2).is_undefined_or_null() {
                                ctx.throw_type_error("value has no property");
                                break State::Exception;
                            }
                        }

                        match SP_REF!(-1) {
                            JSValue::Int(_) | JSValue::String(_) | JSValue::Symbol(_) => {}
                            _ => {
                                if let Ok(prop_key) = SP_REF!(-1).coerce_to_propkey(ctx) {
                                    SP_PUT!(-1, prop_key);
                                } else {
                                    break State::Exception;
                                }
                            }
                        }
                    }
                    OpCode::WITH_GET_VAR
                    | OpCode::WITH_PUT_VAR
                    | OpCode::WITH_DELETE_VAR
                    | OpCode::WITH_MAKE_REF
                    | OpCode::WITH_GET_REF
                    | OpCode::WITH_GET_REF_UNDEF => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let diff = PC_FETCH!(i32);
                        let is_with = PC_FETCH!(u8);

                        let obj = SP_REF!(-1);

                        if let Ok(ret) = obj.has_property(ctx, atom) {
                            let has_with = if ret {
                                if is_with != 0 {
                                    let r = js_has_unscopable(ctx, obj, atom);
                                    if r < 0 {
                                        break State::Exception;
                                    }

                                    if r != 0 {
                                        false // not with
                                    } else {
                                        true
                                    }
                                } else {
                                    true
                                }
                            } else {
                                false // not with
                            };

                            if has_with {
                                match opcode {
                                    OpCode::WITH_GET_VAR => {
                                        let val = obj.get_property(ctx, atom);
                                        if val.is_exception() {
                                            break State::Exception;
                                        } else {
                                            SP_PUT!(-1, val);
                                        }
                                    }
                                    OpCode::WITH_PUT_VAR => {
                                        // XXX: check if strict mode
                                        let ret = JSValue::set_property_internal(
                                            ctx,
                                            obj,
                                            atom,
                                            SP_TAKE!(-2),
                                            obj,
                                            JS_PROP_THROW_STRICT,
                                        );
                                        SP_TAKE!(-1);
                                        SP_GO!(-2);
                                        if ret.is_err() {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_DELETE_VAR => {
                                        if let Ok(ret) =
                                            obj.delete_property(ctx, atom, JS_PROP_THROW)
                                        {
                                            SP_PUT!(-1, JSValue::Bool(ret));
                                        } else {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_MAKE_REF => {
                                        // produce a pair object/propname on the stack
                                        SP_PUT!(0, atom.to_value(ctx, false));
                                        SP_GO!(1);
                                    }
                                    OpCode::WITH_GET_REF => {
                                        // produce a pair object/method on the stack
                                        let val = obj.get_property(ctx, atom);
                                        if val.is_exception() {
                                            break State::Exception;
                                        } else {
                                            SP_PUT!(0, val);
                                            SP_GO!(1);
                                        }
                                    }
                                    OpCode::WITH_GET_REF_UNDEF => {
                                        // produce a pair undefined/function on the stack
                                        let val = obj.get_property(ctx, atom);
                                        if val.is_exception() {
                                            break State::Exception;
                                        } else {
                                            SP_PUT!(-1, JSValue::UNDEFINED);
                                            SP_PUT!(0, val);
                                            SP_GO!(1);
                                        }
                                    }
                                    _ => {
                                        unreachable!()
                                    }
                                }
                                PC_GO!(diff - 5);
                            } else {
                                // no with
                                // if not jumping, drop the object argument
                                SP_TAKE!(-1);
                                SP_GO!(-1);
                            }
                        } else {
                            break State::Exception;
                        }
                    }

                    OpCode::AWAIT => {
                        break State::Done(JSValue::Int(FUNC_RET_AWAIT), true);
                    }
                    OpCode::YIELD => {
                        break State::Done(JSValue::Int(FUNC_RET_YIELD), true);
                    }
                    OpCode::YIELD_STAR | OpCode::ASYNC_YIELD_STAR => {
                        break State::Done(JSValue::Int(FUNC_RET_YIELD_STAR), true);
                    }
                    OpCode::RETURN_ASYNC => {
                        break State::Done(JSValue::UNDEFINED, true);
                    }
                    OpCode::INITIAL_YIELD => {
                        break State::Done(JSValue::Int(FUNC_RET_INITIAL_YIELD), true);
                    }

                    OpCode::IS_UNDEFINED_OR_NULL => {
                        SP_PUT!(-1, JSValue::Bool(SP_REF!(-1).is_undefined_or_null()));
                    }
                    OpCode::NOP => {}

                    #[cfg(feature = "short_opcode")]
                    OpCode::IS_UNDEFINED => {
                        todo!()
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::IS_NULL => {
                        todo!()
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::TYPEOF_IS_UNDEFINED => {
                        todo!()
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::TYPEOF_IS_FUNCTION => {
                        todo!()
                    }

                    OpCode::INVALID | _ => {
                        ctx.throw_internal_error(&format!(
                            "invalid opcode: pc={}, opcode=0x{:02x}",
                            unsafe { pc.offset_from(pc_base) - 1 },
                            opcode as u8
                        ));
                        break State::Exception;
                    }
                } // match opcode
            }; // opcodes loop

            // process state
            match state {
                State::Exception => {
                    // calc offset index from pc and sp pointers
                    let (pc_offset, sp_offset) = unsafe {
                        (
                            pc.offset_from(pc_base) as usize,
                            sp.offset_from(sp_base) as usize,
                        )
                    };

                    if let Some((new_pc, new_sp)) =
                        sframe.handle_exception(ctx, pc_offset, sp_offset)
                    {
                        // continue running, update pc and sp pointer, and jump
                        unsafe {
                            pc = pc_base.offset(new_pc as _);
                            sp = sp_base.offset(new_sp as _);
                        }
                        continue 'restart;
                    } else {
                        // abort with sp=0, pc unchanged
                        sframe.handle_exit(
                            ctx,
                            pc_offset,
                            0,
                            self.0.borrow().func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL,
                        );
                        return JSValue::EXCEPTION;
                    }
                }

                State::Done(ret_val, is_async) => {
                    sframe.handle_exit(
                        ctx,
                        unsafe { pc.offset_from(pc_base) as usize },
                        unsafe { sp.offset_from(sp_base) as usize },
                        is_async,
                    );
                    return ret_val;
                }
            }
        } // opcodes loop
    }
}
