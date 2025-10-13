use crate::{
    atom::{
        AtomTypeHint, JS_ATOM_NULL, JS_FreeAtom, JSAtom,
        consts::{JS_ATOM_next, JS_ATOM_return, JS_ATOM_throw},
    },
    branch_hints::unlikely,
    bytecode::{
        OpCode, SpecialObject,
        arithmatic::{UnaryOp, op_binary_logic},
        operators::{
            js_append_enumerate, js_for_in_next, js_for_in_start, js_for_of_next,
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
        JS_THROW_VAR_RO, JS_THROW_VAR_UNINITIALIZED, JS_ThrowReferenceErrorNotDefined,
        JS_ThrowReferenceErrorUninitialized, JS_ThrowReferenceErrorUninitialized2,
        JS_ThrowSyntaxErrorVarRedeclaration,
    },
    eval::JS_EVAL_TYPE_DIRECT,
    function::{
        Bytecode, CallContext, JS_CallConstructorInternal, JS_CallInternal, JSFunctionKindEnum,
        JSVarRef, build_arg_list, js_build_arguments, js_build_mapped_arguments, js_function_apply,
    },
    generator::{FUNC_RET_AWAIT, FUNC_RET_INITIAL_YIELD, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR},
    iterator::ObjectIter,
    module::{js_dynamic_import, js_import_meta},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
        JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD, JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_VARREF,
        JS_PROP_WRITABLE, JSObject, JSProperty,
    },
    regexp::RegExp,
    stack_frame::StackFrame,
    utils::read_from_ptr,
    value::JSValue,
};

impl Bytecode {
    /// run bytecodes.
    /// before running, runtime current stack frame will be set to `sframe`.
    /// when exited, runtime current stack frame will be restored back to the previous frame.
    pub fn run(
        &self,
        caller_ctx: &JSContext, // Context of the calling environment
        func_home_obj: Option<&JSObject>,
        stack_frame: &mut StackFrame, // The stack frame for execution
        this_obj: &JSValue,           // The 'this' binding for the function execution
        new_target: JSValue,          // The new.target value for constructor calls
        pc_offset: usize,             // Current instruction pointer (program counter) offset
        sp_offset: usize,             // Current stack pointer offset
        closure_var_refs: &[JSVarRef],
        args: &[JSValue], // actual calling arguments pass in by caller, not padded length
        throwed: bool,    // Indicates if an exception has been thrown
    ) -> JSValue {
        let mode = stack_frame.js_mode;

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

        let sp_base = stack_frame.stack_base_ptr().as_ptr();
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

            if let Some((new_pc, new_sp)) = stack_frame.handle_exception(ctx, cur_pc, cur_sp) {
                // continue execution: update pc and sp.
                unsafe {
                    pc = pc_base.offset(new_pc as _);
                    sp = sp_base.offset(new_sp as _);
                }
            } else {
                // abort execution: sp=0, pc unchanged
                stack_frame.handle_exit(
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
        /// equivalent to SP_PUT!(idx, JSValue::UNDEFINED)
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
                unsafe {
                    stack_frame
                        .arg_base_ptr
                        .offset(($idx) as _)
                        .as_mut()
                        .unwrap()
                }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! ARG_REF {
            ($idx:expr) => {
                unsafe { &mut *stack_frame.arg_base_ptr.offset(($idx) as _) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (stack_frame.var_base_ptr.offset(($idx) as isize).as_mut().unwrap()) }
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (&mut *stack_frame.var_base_ptr.offset(($idx) as isize)) }
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

        #[cfg(all(debug_assertions, feature = "dump_bytecode"))]
        let mut debug_setting: Option<crate::function::BytecodeDebugSetting> = None;

        'restart: loop {
            let state: State = loop {
                #[cfg(all(debug_assertions, feature = "dump_bytecode"))]
                {
                    if unlikely(debug_setting.is_none()) {
                        let b = stack_frame
                            .cur_func
                            .as_object()
                            .and_then(|o| {
                                o.payload().func().and_then(|f| {
                                    f.bytecode().map(|bc| bc.with(|bc| bc.debug.debug_setting))
                                })
                            })
                            .unwrap_or(crate::function::BytecodeDebugSetting::default());
                        debug_setting.replace(b);
                    }

                    let dr = debug_setting.map_or(0, |s| s.dump_run);
                    if unlikely(dr != 0) {
                        super::dump::dump_run_instruction(
                            ctx,
                            self,
                            unsafe { pc.offset_from(pc_base) as usize },
                            if dr == 2 { sp_base } else { std::ptr::null() },
                            if dr == 2 { sp } else { std::ptr::null() },
                        );
                    }
                }

                let opcode = unsafe { std::mem::transmute::<u8, OpCode>(PC_FETCH!(u8)) };

                match opcode {
                    OpCode::PUSH_I32 => {
                        // [^] -> [int_value, ^]
                        let a = PC_FETCH!(i32);
                        SP_PUT!(0, JSValue::Int(a));
                        SP_GO!(1);
                    }
                    OpCode::PUSH_CONST => {
                        // [^] -> [const_val, ^]
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
                        SP_PUT!(
                            0,
                            JSAtom(a)
                                .string_or_symbol(ctx)
                                .unwrap_or(JSValue::EXCEPTION)
                        );
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
                        // only called at the start of a function.
                        // could be EXCEPTION.
                        let val = if mode & JS_MODE_STRICT == 0 {
                            match this_obj {
                                JSValue::Object(_) => this_obj.clone(),
                                JSValue::NULL | JSValue::UNDEFINED => {
                                    JSValue::Object(ctx.global_obj().clone())
                                }
                                _ => {
                                    if let Ok(v) = this_obj.clone().objectify(ctx) {
                                        v.into()
                                    } else {
                                        break State::Exception;
                                    }
                                }
                            }
                        } else {
                            this_obj.clone()
                        };

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
                        // [^] -> [obj, ^]
                        SP_PUT!(
                            0,
                            ctx.create_base_object()
                                .map_or(JSValue::EXCEPTION, JSValue::Object)
                        );
                        SP_GO!(1);
                        if unlikely(SP_REF!(-1).is_exception()) {
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
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::MappedArguments => {
                                let ma = js_build_mapped_arguments(
                                    ctx,
                                    args,
                                    stack_frame,
                                    std::cmp::min(
                                        args.len() as i32,
                                        self.0.borrow().arg_count as i32,
                                    ),
                                )
                                .map_or(JSValue::EXCEPTION, JSValue::Object);
                                SP_PUT!(0, ma);
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::ThisFunc => {
                                SP_PUT!(0, stack_frame.cur_func.clone());
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
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::ImportMeta => {
                                SP_PUT!(
                                    0,
                                    js_import_meta(ctx).map_or(JSValue::EXCEPTION, JSValue::Object)
                                );
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                        }
                    }
                    OpCode::REST => {
                        let first = PC_FETCH!(u16);
                        SP_PUT!(
                            0,
                            ctx.build_rest(first as _, args)
                                .unwrap_or(JSValue::EXCEPTION)
                        );
                        SP_GO!(1);
                        if unlikely(SP_REF!(-1).is_exception()) {
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
                    OpCode::DUP1 => {
                        // Stack operation: [a, b, ^] -> [a, a, b, ^]
                        // Duplicates the second value from top and places it below top
                        SP_PUT!(0, SP_TAKE!(-1));
                        SP_PUT!(-1, SP_REF!(-2).clone());
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
                    OpCode::INSERT2 => {
                        // Stack operation: [obj, a, ^] -> [a, obj, a, ^]
                        // Duplicates top value and inserts it under second position
                        SP_PUT!(0, SP_TAKE!(-1));
                        SP_PUT!(-1, SP_TAKE!(-2));
                        SP_PUT!(-2, SP_REF!(0).clone());
                        SP_GO!(1);
                    }
                    OpCode::INSERT3 => {
                        // Stack operation: [obj, prop, a, ^] -> [a, obj, prop, a, ^]
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
                        // [obj, prop, a, fixed, ^] -> [a, obj, prop, fixed, ^]
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
                        // [^] -> [func_obj, ^]
                        let id = PC_FETCH!(u32);
                        if let JSValue::Bytecode(bc) = CPOOL![id] {
                            SP_PUT!(
                                0,
                                bc.make_closure(ctx, closure_var_refs, Some(stack_frame), None)
                                    .map_or(JSValue::EXCEPTION, |f| f.into())
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
                        // [func_obj, arg1, ..., argN, ^] -> [ret_val, ^]
                        let call_argc = PC_FETCH!(u16);
                        let call_argv = unsafe { sp.offset(-(call_argc as isize)) };

                        stack_frame.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        // could have resursive call. tail_call optimization needed.
                        let ret_val = JS_CallInternal(
                            ctx,
                            unsafe { &*call_argv.offset(-1) },
                            &JSValue::UNDEFINED,
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if unlikely(ret_val.is_exception()) {
                            break State::Exception;
                        } else if unlikely(opcode == OpCode::TAIL_CALL) {
                            break State::Done(ret_val, false);
                        } else {
                            for i in -1..call_argc as isize {
                                unsafe {
                                    let _ =
                                        std::ptr::replace(call_argv.offset(i), JSValue::UNDEFINED);
                                }
                            }
                            SP_GO!(-(call_argc as i32 + 1));

                            SP_PUT!(0, ret_val);
                            SP_GO!(1);
                        }
                    }
                    OpCode::CALL_CONSTRUCTOR => {
                        // [func, new_target, arg1 .., argN, ^] -> [instance, ^]
                        // [                  |<-- argc -->|  ]
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        stack_frame.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        let ret_val = JS_CallConstructorInternal(
                            ctx,
                            unsafe { call_argv.offset(-2).as_ref().unwrap() },
                            unsafe { std::ptr::replace(call_argv.offset(-1), JSValue::UNDEFINED) },
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if unlikely(ret_val.is_exception()) {
                            break State::Exception;
                        } else {
                            for i in -2..call_argc {
                                unsafe {
                                    std::ptr::replace(
                                        call_argv.offset(i as isize),
                                        JSValue::UNDEFINED,
                                    )
                                };
                            }
                            SP_GO!(-(call_argc + 2));

                            SP_PUT!(0, ret_val);
                            SP_GO!(1);
                        }
                    }
                    OpCode::CALL_METHOD | OpCode::TAIL_CALL_METHOD => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };

                        stack_frame.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        // could have recursive call.
                        let ret_val = JS_CallInternal(
                            ctx,
                            unsafe { &*call_argv.offset(-1) },
                            unsafe { &*call_argv.offset(-2) },
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if unlikely(ret_val.is_exception()) {
                            break State::Exception;
                        }

                        if unlikely(opcode == OpCode::TAIL_CALL_METHOD) {
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
                        // [el1, el2 ... elN, ^] -> [array, ^]
                        let call_argc = PC_FETCH!(u16) as i32;

                        let mut list = Vec::<JSValue>::new();
                        if list.try_reserve_exact(call_argc as usize).is_ok() {
                            // collect elements
                            let call_argv = unsafe { sp.offset(-call_argc as isize) };
                            for i in 0..call_argc as isize {
                                let val = unsafe {
                                    std::ptr::replace(call_argv.offset(i), JSValue::UNDEFINED)
                                };
                                list.push(val);
                            }

                            // create array with collected elements
                            if let Ok(arr) = ctx.create_array_with_values(list) {
                                SP_GO!(-call_argc);
                                // push array on stack top
                                SP_PUT!(0, arr.into());
                                SP_GO!(1);
                            } else {
                                break State::Exception;
                            }
                        } else {
                            ctx.throw_out_of_memory();
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
                            if unlikely(!SP_REF!(-1).is_undefined()) {
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
                        if unlikely(new_target.is_undefined()) {
                            ctx.throw_type_error("class constructors must be invoked with 'new'");
                            break State::Exception;
                        }
                    }
                    OpCode::CHECK_BRAND => {
                        if let Ok(b) = SP_REF!(-2).check_brand(ctx, SP_REF!(-1)) {
                            if unlikely(!b) {
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
                        if unlikely(func_obj.add_brand(ctx, &home_object).is_err()) {
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
                                ctx.throw_type_error_read_only(atom);
                            }
                            JS_THROW_VAR_REDECL => {
                                JS_ThrowSyntaxErrorVarRedeclaration(ctx, atom);
                            }
                            JS_THROW_VAR_UNINITIALIZED => {
                                JS_ThrowReferenceErrorUninitialized(ctx, atom);
                            }
                            JS_THROW_ERROR_DELETE_SUPER => {
                                ctx.throw_reference_error("unsupported reference to 'super'");
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
                        stack_frame.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

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
                                Ok(JSValue::UNDEFINED)
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
                            .err()
                        };

                        // pop and drop values on stack since `func_val` position
                        {
                            for i in -1..call_argc as isize {
                                unsafe {
                                    std::ptr::replace(call_argv.offset(i), JSValue::UNDEFINED);
                                }
                            }
                            sp = unsafe { sp.offset(-call_argc as isize - 1) };
                        }

                        if let Ok(v) = ret_val {
                            // push result value on stack
                            SP_PUT!(0, v);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
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
                                    Ok(JSValue::UNDEFINED)
                                }
                            } else {
                                func_val.call(ctx, &JSValue::UNDEFINED, &arg_list)
                            };

                            if let Ok(v) = ret_val {
                                SP_TAKE!(-2);
                                SP_TAKE!(-1);
                                SP_GO!(-2);
                                SP_PUT!(0, v);
                                SP_GO!(1);
                            } else {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        };
                    }
                    OpCode::REGEXP => {
                        // [pattern, pre_compiled_id, ^] -> [regexp, ^]
                        if let JSValue::Int(re_id) = SP_TAKE!(-1) {
                            let re = Box::new(ctx.rt().regexp_precompiled[re_id as usize].clone());
                            let regexp = RegExp::from(ctx, &JSValue::UNDEFINED, re)
                                .map_or(JSValue::EXCEPTION, |o| o.into());
                            SP_PUT!(-2, regexp);
                            SP_GO!(-1);
                        };
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
                        // [^] -> [value, ^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        if let Ok(val) = ctx.get_global_var(atom, opcode == OpCode::GET_VAR) {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_VAR | OpCode::PUT_VAR_INIT => {
                        // [val, ^] -> [^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        let res = ctx.set_global_var(
                            atom,
                            SP_TAKE!(-1),
                            (opcode as u8 - OpCode::PUT_VAR as u8) as _,
                        );
                        SP_GO!(-1);
                        if unlikely(res.is_err()) {
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
                            if unlikely(ret.is_err()) {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::CHECK_DEFINE_VAR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        let r = ctx.define_global_var_checked(atom, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_VAR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        let r = ctx.define_global_var(atom, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_FUNC => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);

                        let func = SP_TAKE!(-1);
                        SP_GO!(-1);

                        let r = ctx.define_global_function(atom, func, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_LOC => {
                        // [^] -> [value, ^]
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
                        // operation: [^] -> [value, ^] where ^ is stack top
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
                        if unlikely(val.is_uninitialized()) {
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
                        if unlikely(!closure_var_refs[idx as usize].value().is_uninitialized()) {
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
                        if unlikely(val.is_uninitialized()) {
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
                    OpCode::PUT_LOC_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(pval.is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(ctx, self, idx, false);
                            break State::Exception;
                        } else {
                            *pval = SP_TAKE!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(!pval.is_uninitialized()) {
                            ctx.throw_reference_error("'this' can be initialized only once");
                            break State::Exception;
                        } else {
                            *pval = SP_TAKE!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::CLOSE_LOC => {
                        let idx = PC_FETCH!(u16);
                        stack_frame.close_lexical_var(ctx, idx, false);
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
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break State::Exception;
                        }

                        let var_ref = if opcode == OpCode::MAKE_VAR_REF_REF {
                            Ok(closure_var_refs[idx as usize].clone())
                        } else {
                            stack_frame.get_var_ref(ctx, idx, opcode == OpCode::MAKE_ARG_REF)
                        };
                        match var_ref {
                            Ok(vr) => unsafe {
                                if SP_REF!(-1)
                                    .as_object_unchecked()
                                    .add_own_property(
                                        ctx,
                                        atom,
                                        JS_PROP_WRITABLE | JS_PROP_VARREF,
                                        JSProperty::VarRef(vr),
                                    )
                                    .is_err()
                                {
                                    break State::Exception;
                                } else {
                                    SP_PUT!(
                                        0,
                                        atom.string_or_symbol(ctx).unwrap_or(JSValue::EXCEPTION)
                                    );
                                    SP_GO!(1);
                                }
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

                        if unlikely(ctx.poll_interrupts().is_err()) {
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
                                if let Ok(x) = op_1.to_bool() {
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

                        if unlikely(ctx.poll_interrupts().is_err()) {
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
                            if unlikely(pos >= bytecodes.len() as i32) {
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
                    }
                    OpCode::FOR_IN_START => {
                        if js_for_in_start(ctx, sp).is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::FOR_IN_NEXT => {
                        if js_for_in_next(ctx, sp).is_err() {
                            break State::Exception;
                        }
                        SP_GO!(2);
                    }
                    OpCode::FOR_OF_START | OpCode::FOR_AWAIT_OF_START => {
                        // [obj, ^] -> [obj[Symbol.iterator], next_method, catchoffset(0), ^]
                        // [obj, ^] -> [obj[Symbol.asyncIterator], next_method, catchoffset(0), ^]
                        let is_async = opcode == OpCode::FOR_AWAIT_OF_START;

                        if let Ok((iter_obj, next_method)) = SP_REF!(-1)
                            .get_iterator_must(ctx, is_async)
                            .and_then(|o| o.get_property(ctx, JS_ATOM_next).map(|m| (o, m)))
                        {
                            SP_PUT!(-1, iter_obj.into());
                            SP_PUT!(0, next_method);
                            SP_GO!(1);
                            SP_PUT!(0, JSValue::CatchOffset(0)); // mark for_of_start
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }

                        // let r = js_for_of_start(ctx, sp, is_async);
                        // if unlikely(r.is_err()) {
                        //     break State::Exception;
                        // } else {
                        //     SP_GO!(1);
                        //     SP_PUT!(0, JSValue::CatchOffset(0)); // mark for_of_start
                        //     SP_GO!(1);
                        // }
                    }
                    OpCode::FOR_OF_NEXT => {
                        let offset = -3 - PC_FETCH!(u8) as i32;
                        let r = unsafe { js_for_of_next(ctx, sp, offset) };
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                        SP_GO!(2);
                    }
                    OpCode::ITERATOR_GET_VALUE_DONE => {
                        if unlikely(unsafe { js_iterator_get_value_done(ctx, sp) } != 0) {
                            break State::Exception;
                        } else {
                            SP_GO!(1);
                        }
                    }
                    OpCode::ITERATOR_CHECK_OBJECT => {
                        if unlikely(!SP_REF!(-1).is_object()) {
                            ctx.throw_type_error("iterator must return an object");
                            break State::Exception;
                        }
                    }
                    OpCode::ITERATOR_CLOSE => {
                        // [iter_obj, next, catch_offset, ^] -> [^]
                        SP_TAKE!(-1); // drop the catch offset (to avoid getting caught by exception)
                        SP_TAKE!(-2); // drop the next method
                        SP_GO!(-2);

                        // SP[-1] is iter_obj
                        if let JSValue::Object(o) = SP_REF!(-1) {
                            if unlikely(ObjectIter::do_close(ctx, o, false).is_err()) {
                                // leave iter_obj in stack
                                break State::Exception;
                            } else {
                                SP_TAKE!(-1);
                            }
                        }
                        SP_GO!(-1); // pop iter_obj
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
                        if unlikely(sp == sp_base) {
                            ctx.throw_internal_error("nip_catch");
                            break State::Exception;
                        } else {
                            SP_PUT!(-1, ret_val);
                        }
                    }
                    OpCode::ITERATOR_NEXT => {
                        // [iter_obj, next, catch_offset, val, ^] -> [iter_obj, next, catch_offset, val2, ^]
                        // [      -4,   -3,           -2,  -1, 0]
                        //
                        // SP[-3]: next_method, SP[-4]: iter_obj, SP[-1]: val
                        if let Ok(ret) =
                            SP_REF!(-3).call(ctx, SP_REF!(-4), std::slice::from_ref(SP_REF!(-1)))
                        {
                            SP_PUT!(-1, ret);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::ITERATOR_CALL => {
                        // [iter_obj, next, catch_offset, val, ^] -> [iter_obj, next, catch_offset, val2, bool, ^]
                        // [      -4,   -3,           -2,  -1, 0]
                        let flags = PC_FETCH!(u8);

                        if let Ok(method) = SP_REF!(-4).get_property(
                            ctx,
                            if flags & 1 != 0 {
                                JS_ATOM_throw
                            } else {
                                JS_ATOM_return
                            },
                        ) {
                            let ret_flag = if unlikely(method.is_undefined_or_null()) {
                                true
                            } else {
                                if let Ok(ret) = method.call(
                                    ctx,
                                    SP_REF!(-4),
                                    if flags & 2 != 0 {
                                        &[]
                                    } else {
                                        std::slice::from_ref(SP_REF!(-1))
                                    },
                                ) {
                                    SP_PUT!(-1, ret);
                                } else {
                                    break State::Exception;
                                }
                                false
                            };

                            SP_PUT!(0, JSValue::Bool(ret_flag));
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::LNOT => {
                        let op_1: &JSValue = SP_REF!(-1);
                        let b = match op_1 {
                            JSValue::Int(i) => *i != 0,
                            JSValue::Bool(b) => *b,
                            JSValue::NULL | JSValue::UNDEFINED => false,
                            _ => {
                                if let Ok(b) = op_1.to_bool() {
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

                        if let Ok(val) = SP_REF!(-1).get_property(ctx, atom) {
                            if opcode == OpCode::GET_FIELD {
                                SP_PUT!(-1, val);
                            } else {
                                SP_PUT!(0, val);
                                SP_GO!(1)
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_FIELD => {
                        // [obj, value, ^] -> [^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        let obj = SP_TAKE!(-2);
                        let value = SP_TAKE!(-1);
                        SP_GO!(-2);

                        let r =
                            obj.set_property_internal(ctx, atom, value, None, JS_PROP_THROW_STRICT);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::PRIVATE_SYMBOL => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        if let Ok(val) = ctx.create_symbol_from_atom(atom, AtomTypeHint::Private) {
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
                        SP_PUT!(-2, val);
                        SP_GO!(-1);

                        // because has done go(-1), so SP[-1] is orginal SP[-2]
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [^]
                        let ret = SP_TAKE!(-3).set_private_field(ctx, &SP_TAKE!(-1), SP_TAKE!(-2));
                        SP_GO!(-3);
                        if unlikely(ret.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [this_obj, ^]
                        let ret =
                            SP_REF!(-3).define_private_field(ctx, &SP_TAKE!(-2), SP_TAKE!(-1));
                        SP_GO!(-2);
                        if unlikely(ret.is_err()) {
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
                        if unlikely(ret.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_NAME => {
                        // [obj, ^] -> [obj, ^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        let r = SP_REF!(-1)
                            .as_object()
                            .ok_or(-1)
                            .and_then(|o| o.define_name(ctx, atom, JS_PROP_CONFIGURABLE));
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_NAME_COMPUTED => {
                        // [name, value, ^] -> [name, value, ^]
                        let r = SP_REF!(-1).as_object().unwrap().define_name_computed(
                            ctx,
                            SP_REF!(-2),
                            JS_PROP_CONFIGURABLE,
                        );
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_PROTO => {
                        // [obj, proto, ^] -> [obj, ^]
                        let proto = SP_REF!(-1);
                        if proto.is_object() || proto.is_null() {
                            let r = SP_REF!(-2).set_prototype_internal(ctx, proto.clone(), true);
                            if unlikely(r.is_err()) {
                                break State::Exception;
                            }
                        }
                        SP_TAKE!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::SET_HOME_OBJECT => {
                        // [home_obj, func, ^] -> [home_obj, func, ^]
                        js_method_set_home_object(SP_REF!(-1), SP_REF!(-2).as_object());
                    }
                    OpCode::DEFINE_METHOD | OpCode::DEFINE_METHOD_COMPUTED => {
                        // DEFINE_METHOD: [this_obj, value, ^] -> [this_obj, ^]
                        // DEFINE_METHOD_COMPUTED: [this_obj, name, value, ^] -> [this_obj, ^]
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
                            name = if let Ok(a) = SP_REF!(-2).to_atom(ctx) {
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

                        let this_obj: &JSValue = SP_REF!(if is_computed { -3 } else { -2 });

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
                        if ret.is_ok() {
                            ret = this_obj
                                .define_property(ctx, name, value, getter, setter, flags)
                                .map(|_| ());
                        }

                        if is_computed {
                            JS_FreeAtom(ctx, name);
                            SP_TAKE!(-2);
                            SP_GO!(-2);
                        } else {
                            SP_GO!(-1);
                        }

                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_CLASS | OpCode::DEFINE_CLASS_COMPUTED => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        let class_flags = PC_FETCH!(u8) as u32;
                        let r = js_op_define_class(
                            ctx,
                            sp,
                            atom,
                            class_flags as _,
                            closure_var_refs,
                            stack_frame,
                            opcode == OpCode::DEFINE_CLASS_COMPUTED,
                        );
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL => {
                        // [array, prop, ^] -> [value, ^]
                        let val = SP_REF!(-2)
                            .get_property_value(ctx, &SP_TAKE!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(-2, val);
                        SP_GO!(-1);

                        if unlikely(is_err) {
                            break State::Exception;
                        }

                        // let array = SP_REF!(-2);
                        // let prop = SP_TAKE!(-1);
                        // if unlikely(array.is_undefined_or_null()) {
                        //     ctx.throw_type_error(&format!(
                        //         "cannot read property of {}",
                        //         if array.is_undefined() {
                        //             "undefined"
                        //         } else {
                        //             "null"
                        //         }
                        //     ));
                        //     SP_PUT!(-2, JSValue::EXCEPTION);
                        //     SP_GO!(-1);
                        //     break State::Exception;
                        // } else {
                        //     let val = array
                        //         .get_property_value(ctx, &prop)
                        //         .unwrap_or(JSValue::EXCEPTION);
                        //     let is_err = val.is_exception();
                        //     SP_PUT!(-2, val);
                        //     SP_GO!(-1);
                        //     if unlikely(is_err) {
                        //         break State::Exception;
                        //     }
                        // }
                    }
                    OpCode::GET_ARRAY_EL2 => {
                        // [array, prop, ^] -> [array, value, ^]
                        let val = SP_REF!(-2)
                            .get_property_value(ctx, SP_REF!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(-1, val);

                        if unlikely(is_err) {
                            break State::Exception;
                        }

                        // let array = SP_REF!(-2);
                        // if unlikely(array.is_undefined_or_null()) {
                        //     ctx.throw_type_error(&format!(
                        //         "cannot read property of {}",
                        //         if array.is_undefined() {
                        //             "undefined"
                        //         } else {
                        //             "null"
                        //         }
                        //     ));
                        //     SP_PUT!(-1, JSValue::EXCEPTION);
                        //     break State::Exception;
                        // } else {
                        //     let val = array
                        //         .get_property_value(ctx, SP_REF!(-1))
                        //         .unwrap_or(JSValue::EXCEPTION);
                        //     let is_err = val.is_exception();
                        //     SP_PUT!(-1, val);
                        //     if unlikely(is_err) {
                        //         break State::Exception;
                        //     }
                        // }
                    }
                    OpCode::GET_REF_VALUE => {
                        // [obj, prop, ^] -> [obj, value, ^]
                        if unlikely(SP_REF!(-2).is_undefined()) {
                            if let Ok(atom) = SP_REF!(-1).to_atom(ctx) {
                                JS_ThrowReferenceErrorNotDefined(ctx, atom);
                                JS_FreeAtom(ctx, atom);
                            }
                            break State::Exception;
                        } else {
                            if let Ok(val) = SP_REF!(-2).get_property_value(ctx, SP_REF!(-1)) {
                                SP_PUT!(0, val);
                                SP_GO!(1);
                            } else {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::GET_SUPER_VALUE => {
                        if let Ok(atom) = SP_REF!(-1).to_atom(ctx) {
                            let val =
                                SP_REF!(-2).get_property_internal(ctx, atom, SP_REF!(-3), false);
                            JS_FreeAtom(ctx, atom);

                            if let Ok(val) = val {
                                SP_TAKE!(-1);
                                SP_TAKE!(-2);
                                SP_PUT!(-3, val);
                                SP_GO!(-2);
                            } else {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_ARRAY_EL => {
                        // [obj, prop, value, ^] -> [^]
                        let arr = SP_TAKE!(-3);
                        let ret = arr.set_property_value(
                            ctx,
                            &SP_TAKE!(-2),
                            SP_TAKE!(-1),
                            JS_PROP_THROW_STRICT,
                        );
                        SP_GO!(-3);
                        if unlikely(ret.is_err()) {
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

                        if unlikely(obj.is_undefined()) {
                            if is_strict_mode(ctx) {
                                if let Ok(atom) = prop.to_atom(ctx) {
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

                        if unlikely(!obj.is_object()) {
                            ctx.throw_type_error_not_an_object();
                            break State::Exception;
                        }

                        if let Ok(atom) = prop.to_atom(ctx) {
                            let ret = obj.set_property_internal(
                                ctx,
                                atom,
                                val,
                                if let JSValue::Object(o) = &this_obj {
                                    Some(o)
                                } else {
                                    unreachable!()
                                },
                                JS_PROP_THROW_STRICT,
                            );
                            JS_FreeAtom(ctx, atom);
                            if unlikely(ret.is_err()) {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_ARRAY_EL => {
                        // [obj, prop, value, ^] -> [obj, prop, ^]

                        let obj = SP_REF!(-3);
                        let prop = SP_REF!(-2);
                        let value = SP_TAKE!(-1);

                        // FIXME: test262 fail
                        // converting `prop` to atom may invoke toString/valueOf method, which may modify `value`.
                        // this bytecode has cloned value on stack, this may break the spec.
                        // a solution is to emit bytecode like this:
                        // ...
                        // object
                        // get_var key
                        // to_propkey <-- insert this
                        // get_var value
                        // define_array_el

                        let ret = prop.to_atom(ctx).and_then(|a| {
                            let r = obj.define_property_value(
                                ctx,
                                a,
                                value,
                                JS_PROP_C_W_E | JS_PROP_THROW,
                            );
                            JS_FreeAtom(ctx, a);
                            r
                        });

                        SP_GO!(-1);
                        if unlikely(ret.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::APPEND => {
                        // [obj, start_pos, src, ^] -> [obj, end_pos, ^]
                        // all elements from `src` are appended to `dest` array, staring from `start_pos` index
                        // 操作完成后, `SP[-3]`数组中加入了来自`SP[-1]`的全部元素，
                        // `SP[-1]`没有变化，`SP[-2]`更新为结束位置index。
                        if unlikely(unsafe { js_append_enumerate(ctx, sp).is_err() }) {
                            break State::Exception;
                        } else {
                            // pop `src`
                            SP_TAKE!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::COPY_DATA_PROPERTIES => {
                        // target source excludeList
                        // stack offsets (-1 based):
                        // 2 bits for target: SP[-1 - (mask & 3)],
                        // 3 bits for source: SP[SP_REF!(-1 - ((mask >> 2) & 7)],
                        // 2 bits for exclusionList: SP[-1 - ((mask >> 5) & 7)]
                        let mask = PC_FETCH!(u8) as i32;
                        let r = SP_REF!(-1 - (mask & 3)).copy_data_properties_from(
                            ctx,
                            SP_REF!(-1 - ((mask >> 2) & 7)),
                            SP_REF!(-1 - ((mask >> 5) & 7)),
                            false,
                        );
                        if unlikely(r.is_err()) {
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
                            OpCode::ADD => JSValue::arith_add(ctx, &op1, &op2),
                            OpCode::SUB => JSValue::arith_sub(ctx, &op1, &op2),
                            OpCode::MUL => JSValue::arith_mul(ctx, &op1, &op2),
                            OpCode::DIV => JSValue::arith_div(ctx, &op1, &op2),
                            OpCode::MOD => JSValue::arith_mod(ctx, &op1, &op2),
                            OpCode::POW => JSValue::arith_pow(ctx, &op1, &op2),
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
                        if let Ok(val) = JSValue::arith_add(ctx, pv, &op1) {
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
                            SP_PUT!(-1, op1);
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
                        if let Ok(n1) = op1.to_number(ctx) {
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

                        if let Ok(rel) = op1.arith_cmp(ctx, &op2) {
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
                        if let Ok(b) = op1.arith_eq(ctx, &op2) {
                            SP_PUT!(-2, JSValue::Bool(b ^ (opcode == OpCode::NEQ)));
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
                        // [prop, obj, ^] -> [bool, ^]
                        if let Ok(b) = js_operator_in(ctx, SP_REF!(-1), SP_REF!(-2)) {
                            SP_PUT!(-2, JSValue::Bool(b));
                            SP_TAKE!(-1);
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PRIVATE_IN => {
                        if unlikely(js_operator_private_in(ctx, sp).is_err()) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::INSTANCEOF => {
                        if unlikely(js_operator_instanceof(ctx, sp).is_err()) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::TYPEOF => {
                        let op1 = SP_PUT!(-1, JSValue::UNDEFINED);
                        let atom = op1.type_of();
                        SP_PUT!(
                            -1,
                            atom.force_string(ctx)
                                .map_or(JSValue::EXCEPTION, JSValue::String)
                        );
                    }
                    OpCode::DELETE => {
                        if unlikely(js_operator_delete(ctx, sp) != 0) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::DELETE_VAR => {
                        let atom = JSAtom(PC_FETCH!(u32));
                        match ctx.global_obj().delete_property(ctx, atom, 0) {
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
                        // [val, ^] -> [object, ^]
                        if !SP_REF!(-1).is_object() {
                            if let Ok(o) = SP_REF!(-1).clone().objectify(ctx) {
                                SP_PUT!(-1, o.into());
                            } else {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::TO_PROPKEY | OpCode::TO_PROPKEY2 => {
                        // TO_PROPKEY: [value, ^] -> [string_or_symbol, ^]
                        // TOP_PROPKEY2: [target, value, ^] -> [target, string_or_symbol, ^]
                        if opcode == OpCode::TO_PROPKEY2 {
                            // must be tested first
                            if unlikely(SP_REF!(-2).is_undefined_or_null()) {
                                ctx.throw_type_error("value has no property");
                                break State::Exception;
                            }
                        }

                        match SP_REF!(-1) {
                            JSValue::Int(_) | JSValue::String(_) | JSValue::Symbol(_) => {}
                            _ => {
                                if let Ok(pk) = SP_REF!(-1).to_propkey(ctx) {
                                    SP_PUT!(-1, pk);
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
                        // [env_obj, ^] -> [^]
                        let atom = JSAtom(PC_FETCH!(u32));
                        let diff = PC_FETCH!(i32);
                        let is_with = PC_FETCH!(u8) != 0;

                        let env_obj = SP_REF!(-1);

                        if let Ok(ret) = env_obj.has_property(ctx, atom) {
                            let in_env_obj = if ret {
                                if is_with {
                                    if let Ok(b) = js_has_unscopable(ctx, env_obj, atom) {
                                        !b
                                    } else {
                                        break State::Exception;
                                    }
                                } else {
                                    true
                                }
                            } else {
                                false // not in with env obj
                            };

                            if in_env_obj {
                                // get atom in with env obj.
                                match opcode {
                                    OpCode::WITH_GET_VAR => {
                                        if let Ok(val) = env_obj.get_property(ctx, atom) {
                                            SP_PUT!(-1, val);
                                        } else {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_PUT_VAR => {
                                        // XXX: check if strict mode
                                        let ret = env_obj.set_property_internal(
                                            ctx,
                                            atom,
                                            SP_TAKE!(-2),
                                            None,
                                            JS_PROP_THROW_STRICT,
                                        );
                                        SP_TAKE!(-1);
                                        SP_GO!(-2);
                                        if unlikely(ret.is_err()) {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_DELETE_VAR => {
                                        if let Ok(b) = env_obj.delete_property(ctx, atom, 0) {
                                            SP_PUT!(-1, JSValue::Bool(b));
                                        } else {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_MAKE_REF => {
                                        // produce a pair object/propname on the stack
                                        SP_PUT!(
                                            0,
                                            atom.string_or_symbol(ctx)
                                                .unwrap_or(JSValue::EXCEPTION)
                                        );
                                        SP_GO!(1);
                                    }
                                    OpCode::WITH_GET_REF => {
                                        // produce a pair object/method on the stack
                                        if let Ok(val) = env_obj.get_property(ctx, atom) {
                                            SP_PUT!(0, val);
                                            SP_GO!(1);
                                        } else {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_GET_REF_UNDEF => {
                                        // produce a pair undefined/function on the stack
                                        if let Ok(val) = env_obj.get_property(ctx, atom) {
                                            SP_PUT!(-1, JSValue::UNDEFINED);
                                            SP_PUT!(0, val);
                                            SP_GO!(1);
                                        } else {
                                            break State::Exception;
                                        }
                                    }
                                    _ => {
                                        #[cfg(debug_assertions)]
                                        unreachable!();
                                        #[cfg(not(debug_assertions))]
                                        unsafe {
                                            std::hint::unreachable_unchecked();
                                        }
                                    }
                                }

                                PC_GO!(diff - 5);
                            } else {
                                // if not jumping, drop the env_obj in stack
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
                            "invalid opcode: 0x{:02x}, pc={}",
                            opcode as u8,
                            unsafe { pc.offset_from(pc_base) - 1 },
                        ));
                        break State::Exception;
                    }
                } // match opcode

                #[cfg(all(debug_assertions, feature = "dump_bytecode"))]
                if unlikely(debug_setting.map_or(0, |s| s.dump_run) == 2) {
                    let oi = super::opcode_info_final(opcode as u8);
                    if oi.n_push > 0 {
                        super::dump::dump_run_stack(
                            ctx,
                            unsafe {
                                std::slice::from_raw_parts(
                                    sp.offset(-(oi.n_push as isize)),
                                    oi.n_push as usize,
                                )
                            },
                            false,
                            oi.n_push as _,
                            '+',
                        );
                    }
                }
            }; // opcodes loop

            // process state
            match state {
                State::Done(ret_val, is_generator) => {
                    stack_frame.handle_exit(
                        ctx,
                        unsafe { pc.offset_from(pc_base) as usize },
                        unsafe { sp.offset_from(sp_base) as usize },
                        is_generator,
                    );
                    return ret_val;
                }

                State::Exception => {
                    if let Some((pc2, sp2)) =
                        self.exception(ctx, stack_frame, pc, pc_base, sp, sp_base)
                    {
                        pc = pc2;
                        sp = sp2;
                        continue 'restart;
                    } else {
                        return JSValue::EXCEPTION;
                    }
                }
            }
        } // opcodes loop
    }

    #[cold]
    #[inline(never)]
    fn exception(
        &self,
        ctx: &JSContext,
        stack_frame: &mut StackFrame,
        pc: *const u8,
        pc_base: *const u8,
        sp: *mut JSValue,
        sp_base: *mut JSValue,
    ) -> Option<(*const u8, *mut JSValue)> {
        // calc offset index from pc and sp pointers
        let (pc_offset, sp_offset) = unsafe {
            (
                pc.offset_from(pc_base) as usize,
                sp.offset_from(sp_base) as usize,
            )
        };

        if let Some((new_pc, new_sp)) = stack_frame.handle_exception(ctx, pc_offset, sp_offset) {
            // continue running, update pc and sp pointer, and jump
            unsafe { Some((pc_base.offset(new_pc as _), sp_base.offset(new_sp as _))) }
        } else {
            // abort with sp=0, pc unchanged
            let is_generator = self.0.borrow().func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL;
            stack_frame.handle_exit(ctx, pc_offset, 0, is_generator);
            None
        }
    }
}
