use std::{ffi::CStr, ptr::NonNull};

use crate::{
    array::JS_NewArray,
    atom::{
        consts::{JS_ATOM_return, JS_ATOM_throw},
        JSAtom, JSAtomTypeHint, JS_FreeAtom, JS_ValueToAtom, JS_ATOM_NULL,
    },
    bytecode::{
        arithmatic::{js_eq, op_binary_logic, relational, BinaryOp, UnaryOp},
        operators::{
            js_append_enumerate, js_for_in_next, js_for_in_start, js_for_of_next, js_for_of_start,
            js_has_unscopable, js_iterator_get_value_done, js_method_set_home_object,
            js_method_set_properties, js_op_define_class, js_operator_delete, js_operator_in,
            js_operator_instanceof, js_operator_private_in, js_operator_typeof,
        },
        OPSpecialObjectEnum, OpCode,
    },
    class::{
        OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD,
        OP_DEFINE_METHOD_SETTER,
    },
    context::{is_strict_mode, JSContext, JS_MODE_STRICT},
    error::{
        JS_ThrowReferenceError, JS_ThrowReferenceErrorNotDefined,
        JS_ThrowReferenceErrorUninitialized, JS_ThrowReferenceErrorUninitialized2,
        JS_ThrowSyntaxErrorVarRedeclaration, JS_THROW_ERROR_DELETE_SUPER,
        JS_THROW_ERROR_ITERATOR_THROW, JS_THROW_VAR_REDECL, JS_THROW_VAR_RO,
        JS_THROW_VAR_UNINITIALIZED,
    },
    eval::{JS_EvalObject, JS_EVAL_TYPE_DIRECT},
    function::{
        build_arg_list, js_build_arguments, js_build_mapped_arguments, js_build_rest, js_closure,
        js_function_apply, js_poll_interrupts, CallArgs, JSFunctionBytecode, JSFunctionKindEnum,
        JSVarRef, JS_Call, JS_CallConstructorInternal, JS_CallFree, JS_CallInternal,
    },
    generator::{FUNC_RET_AWAIT, FUNC_RET_INITIAL_YIELD, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR},
    global::{
        JS_CheckDefineGlobalVar, JS_CheckGlobalVar, JS_DefineGlobalFunction, JS_DefineGlobalVar,
        JS_GetGlobalVar, JS_GetGlobalVarRef, JS_SetGlobalVar,
    },
    iterator::JS_IteratorClose,
    module::{js_dynamic_import, js_import_meta},
    regexp::js_regexp_constructor_internal,
    stack_frame::JSStackFrame,
    utils::{get_u16, get_u32},
    value::{
        object::{
            JSObject, JS_AddBrand, JS_CheckBrand, JS_DefineObjectName, JS_DefineObjectNameComputed,
            JS_DefinePrivateField, JS_GetPrivateField, JS_GetPrototype, JS_NewObject,
            JS_NewObjectProto, JS_SetPrivateField, JS_SetPrototypeInternal, JS_ToObject,
            JS_ToPropertyKey, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_ENUMERABLE,
            JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
            JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD, JS_PROP_THROW,
            JS_PROP_THROW_STRICT, JS_PROP_VARREF, JS_PROP_WRITABLE,
        },
        set_value, JSTag, JSValue, JS_FreeValue, JS_IsUninitialized, JS_NewBool, JS_NewCatchOffset,
        JS_NewSymbolFromAtom, JS_ToBoolFree, JS_ToNumberFree, HINT_NONE, JS_TAG_CATCH_OFFSET,
        JS_TAG_INT, JS_TAG_NULL, JS_TAG_OBJECT, JS_TAG_STRING, JS_TAG_SYMBOL, JS_TAG_UNDEFINED,
        JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_TAG,
    },
    JS_MKPTR,
};

pub unsafe fn run_bytecodes(
    b: &JSFunctionBytecode,
    func_home_obj: *mut JSObject,
    sf: &mut JSStackFrame, // Execution stack frame for managing function call state
    caller_ctx: &mut JSContext, // Context of the calling environment
    this_obj: JSValue,     // The 'this' value for the function execution
    new_target: JSValue,   // The new.target value for constructor calls
    bytecodes: &[u8],      // Array of bytecode instructions to execute
    pc_offset: usize,      // Current instruction pointer (program counter) offset
    sp_offset: usize,      // Current stack pointer offset
    var_refs: &[NonNull<JSVarRef>],
    argc: i32,
    argv: *mut JSValue,
    mode: i32,
    throwed: bool, // Indicates if an exception has been thrown
                   //local_buf: &mut [JSValue],
) -> JSValue {
    debug_assert!(pc_offset < bytecodes.len());

    let cpools = b.cpools_slice();
    let ctx = b.realm.as_mut().unwrap();
    let rt = ctx.rt.as_mut();

    let sp_base = sf.stack_base_ptr().as_ptr();
    let pc_base = bytecodes.as_ptr();
    let mut pc = pc_base.add(pc_offset); // convert index to pointer
    let mut sp = sp_base.add(sp_offset); // convert index to pointer

    if throwed {
        // has thrown current exception, handle it before execution.
        let pc_offset = pc.offset_from(pc_base) as usize;
        let sp_offset = sp.offset_from(sp_base) as usize;

        if let Some((new_pc, new_sp)) = sf.handle_exception(ctx, pc_offset, sp_offset) {
            // continue execution: update pc and sp.
            pc = pc_base.offset(new_pc as _);
            sp = sp_base.offset(new_sp as _);
        } else {
            // abort execution: sp=0, pc unchanged
            sf.handle_exit(
                ctx,
                pc_offset,
                0,
                b.func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL,
            );
            return JSValue::EXCEPTION;
        }
    }

    macro_rules! SP {
        [0] => {
            (*sp)
        };
        [$idx:expr] => {
            (*sp.offset(($idx) as isize))
        };
    }
    macro_rules! SP_GO {
        ($step:expr) => {
            sp = sp.offset(($step) as isize)
        };
    }
    macro_rules! PC_GO {
        ($step:expr) => {
            pc = pc.offset(($step) as isize)
        };
    }
    macro_rules! ARG_REF {
        [$idx:expr] => {
            sf.arg_base_ptr.offset(($idx) as _)
        };
    }
    macro_rules! ARG {
        [$idx:expr] => {
            *ARG_REF![$idx]
        };
    }
    macro_rules! VAR_PTR {
        [$idx:expr] => {
            sf.var_base_ptr.offset(($idx) as isize)
        };
    }

    macro_rules! VAR {
        [$idx:expr] => {
            (*sf.var_base_ptr.offset(($idx) as isize))
        };
    }
    macro_rules! CPOOL {
        [$idx:expr] => {
            {
                debug_assert!(($idx  as usize) < cpools.as_ref().unwrap().len());
                cpools.as_ref().unwrap()[($idx) as usize]
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
            #[cfg(feature = "dump_bytecode")]
            {
                let pos = pc.offset_from(pc_base) as usize;
                print!("[O.o] RUN {:5}: ", pos);
                crate::bytecode::dump::dump_opcode_one(ctx, b, pos);
                println!();
            }

            let opcode = std::mem::transmute::<u8, OpCode>(*pc);
            PC_GO!(1);

            match opcode {
                OpCode::PUSH_I32 => {
                    SP![0] = JSValue::from_i32(get_u32(pc) as i32);
                    sp = sp.offset(1);
                    PC_GO!(4);
                }
                OpCode::PUSH_CONST => {
                    let id = get_u32(pc);
                    PC_GO!(4);
                    SP![0] = CPOOL![id].dup();
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
                    let a = get_u32(pc);
                    PC_GO!(4);
                    SP![0] = JSAtom(a).to_value(ctx, false);
                    SP_GO!(1);
                }
                OpCode::UNDEFINED => {
                    SP![0] = JSValue::UNDEFINED;
                    SP_GO!(1);
                }
                OpCode::NULL => {
                    SP![0] = JSValue::NULL;
                    SP_GO!(1);
                }
                OpCode::PUSH_THIS => {
                    // only called at the start of a function
                    let val = if mode & JS_MODE_STRICT == 0 {
                        match JS_VALUE_GET_TAG(this_obj) {
                            JS_TAG_OBJECT => this_obj.dup(),
                            JS_TAG_NULL | JS_TAG_UNDEFINED => ctx.global_obj.dup(),
                            _ => JS_ToObject(ctx, this_obj),
                        }
                    } else {
                        this_obj.dup()
                    };
                    if val.is_exception() {
                        break State::Exception;
                    } else {
                        SP![0] = val;
                        SP_GO!(1);
                    }
                }
                OpCode::PUSH_FALSE => {
                    SP![0] = JSValue::FALSE;
                    SP_GO!(1);
                }
                OpCode::PUSH_TRUE => {
                    SP![0] = JSValue::TRUE;
                    SP_GO!(1);
                }
                OpCode::OBJECT => {
                    SP![0] = JS_NewObject(ctx);
                    SP_GO!(1);
                    if SP![-1].is_exception() {
                        break State::Exception;
                    }
                }
                OpCode::SPECIAL_OBJECT => {
                    let arg = std::mem::transmute::<u8, OPSpecialObjectEnum>(*pc);
                    PC_GO!(1);

                    match arg {
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_ARGUMENTS => {
                            SP![0] = js_build_arguments(ctx, argc, argv);
                            SP_GO!(1);
                            if SP![-1].is_exception() {
                                break State::Exception;
                            }
                        }
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_MAPPED_ARGUMENTS => {
                            SP![0] = js_build_mapped_arguments(
                                ctx,
                                argc,
                                argv,
                                sf,
                                std::cmp::min(argc, b.arg_count as i32),
                            );
                            SP_GO!(1);
                            if SP![-1].is_exception() {
                                break State::Exception;
                            }
                        }
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_THIS_FUNC => {
                            SP![0] = sf.cur_func.dup();
                            SP_GO!(1);
                        }
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_NEW_TARGET => {
                            SP![0] = new_target.dup();
                            SP_GO!(1);
                        }
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_HOME_OBJECT => {
                            SP![0] = if func_home_obj.is_null() {
                                JSValue::UNDEFINED
                            } else {
                                JS_MKPTR!(JSTag::Object, func_home_obj.cast()).dup()
                            };
                            SP_GO!(1);
                        }
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_VAR_OBJECT => {
                            SP![0] = JS_NewObjectProto(ctx, JSValue::NULL);
                            SP_GO!(1);
                            if SP![-1].is_exception() {
                                break State::Exception;
                            }
                        }
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_IMPORT_META => {
                            SP![0] = js_import_meta(ctx);
                            SP_GO!(1);
                            if SP![-1].is_exception() {
                                break State::Exception;
                            }
                        }
                    }
                }
                OpCode::REST => {
                    let first = get_u16(pc);
                    PC_GO!(2);

                    SP![0] = js_build_rest(ctx, first as _, argc, argv);
                    SP_GO!(1);
                    if SP![-1].is_exception() {
                        break State::Exception;
                    }
                }
                OpCode::DROP => {
                    JS_FreeValue(ctx, SP![-1]);
                    SP_GO!(-1);
                }
                OpCode::NIP => {
                    // Stack operation: [a, b, ^] -> [b, ^] where ^ is stack top
                    // Removes the second value from top and moves top value down
                    JS_FreeValue(ctx, SP![-2]);
                    SP![-2] = SP![-1];
                    SP_GO!(-1);
                }
                OpCode::NIP1 => {
                    // Stack operation: [a, b, c, ^] -> [b, c, ^] where ^ is stack top
                    // Removes the third value from top and shifts remaining values down
                    JS_FreeValue(ctx, SP![-3]);
                    SP![-3] = SP![-2];
                    SP![-2] = SP![-1];
                    SP_GO!(-1);
                }
                OpCode::DUP => {
                    // Stack operation: [a, ^] -> [a, a, ^] where ^ is stack top
                    // Duplicates the top value on the stack
                    SP![0] = SP![-1].dup();
                    SP_GO!(1);
                }
                OpCode::DUP2 => {
                    // Stack operation: [a, b, ^] -> [a, b, a, b, ^] where ^ is stack top
                    // Duplicates the top two values on the stack
                    SP![0] = SP![-2].dup();
                    SP![1] = SP![-1].dup();
                    SP_GO!(2);
                }
                OpCode::DUP3 => {
                    // Stack operation: [a, b, c, ^] -> [a, b, c, a, b, c, ^] where ^ is stack top
                    // Duplicates the top three values on the stack
                    SP![0] = SP![-3].dup();
                    SP![1] = SP![-2].dup();
                    SP![2] = SP![-1].dup();
                    SP_GO!(3);
                }
                OpCode::DUP1 => {
                    // Stack operation: [a, b, ^] -> [a, a, b, ^] where ^ is stack top
                    // Duplicates the second value from top and places it below top
                    SP![0] = SP![-1];
                    SP![-1] = SP![-2].dup();
                    SP_GO!(1);
                }
                OpCode::INSERT2 => {
                    // Stack operation: [obj, a, ^] -> [a, obj, a, ^] where ^ is stack top
                    // Duplicates top value and inserts it under second position
                    SP![0] = SP![-1];
                    SP![-1] = SP![-2];
                    SP![-2] = SP![0].dup();
                    SP_GO!(1);
                }
                OpCode::INSERT3 => {
                    // Stack operation: [obj, prop, a, ^] -> [a, obj, prop, a, ^] where ^ is stack top
                    // Duplicates top value and inserts it under third position
                    // Uses array rotation for efficient stack manipulation
                    let lst = std::slice::from_raw_parts_mut(sp.offset(-3), 4);
                    lst.rotate_right(1);

                    // After rotation, SP![0] contains the value to duplicate and insert
                    SP![-3] = SP![0].dup();

                    // SP![0] = SP![-1];
                    // SP![-1] = SP![-2];
                    // SP![-2] = SP![-3];
                    // SP![-3] = SP![0].dup();

                    SP_GO!(1);
                }
                OpCode::INSERT4 => {
                    // Stack operation: [this, obj, prop, a, ^] -> [a, this, obj, prop, a, ^] where ^ is stack top
                    // Duplicates top value and inserts it under fourth position
                    // Uses array rotation for efficient stack manipulation
                    let lst = std::slice::from_raw_parts_mut(sp.offset(-4), 5);
                    lst.rotate_right(1);

                    // After rotation, SP![0] contains the value to duplicate and insert
                    SP![-4] = SP![0].dup();

                    // SP![0] = SP![-1];
                    // SP![-1] = SP![-2];
                    // SP![-2] = SP![-3];
                    // SP![-3] = SP![-4];
                    // SP![-4] = SP![0].dup();

                    SP_GO!(1); // move sp
                }
                OpCode::PERM3 => {
                    // Stack operation: [obj, a, fixed, ^] -> [a, obj, fixed, ^] where ^ is stack top
                    // Swaps the second and third values from top, keeping the fixed value in place
                    std::mem::swap(
                        sp.offset(-2).as_mut().unwrap(),
                        sp.offset(-3).as_mut().unwrap(),
                    );

                    // let tmp = SP![-2];
                    // SP![-2] = SP![-3];
                    // SP![-3] = tmp;
                }
                OpCode::ROT3L => {
                    // Stack operation: [x, a, b, ^] -> [a, b, x, ^] where ^ is stack top
                    // Performs a left rotation on top three values
                    // The leftmost value moves to the right, others shift left
                    let lst = std::slice::from_raw_parts_mut(sp.offset(-3), 3);
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
                    let lst = std::slice::from_raw_parts_mut(sp.offset(-4), 4);
                    lst.rotate_left(1);

                    // let tmp = SP![-4];
                    // SP![-4] = SP![-3];
                    // SP![-3] = SP![-2];
                    // SP![-2] = SP![-1];
                    // SP![-1] = tmp;
                }
                OpCode::ROT5L => {
                    let lst = std::slice::from_raw_parts_mut(sp.offset(-5), 5);
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
                    let lst = std::slice::from_raw_parts_mut(sp.offset(-3), 3);
                    lst.rotate_right(1);

                    // let tmp = SP![-1];
                    // SP![-1] = SP![-2];
                    // SP![-2] = SP![-1];
                    // SP![-3] = tmp;
                }
                OpCode::PERM4 => {
                    // Stack operation: [obj, prop, a, fixed, ^] -> [a, obj, prop, fixed, ^] where ^ is stack top
                    // Performs a right rotation on three values before the fixed value
                    // The rightmost value moves left, others shift right, fixed value stays in place
                    let lst = std::slice::from_raw_parts_mut(sp.offset(-4), 3);
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
                    let lst = std::slice::from_raw_parts_mut(sp.offset(-5), 4);
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
                    std::mem::swap(
                        sp.offset(-1).as_mut().unwrap(),
                        sp.offset(-2).as_mut().unwrap(),
                    );
                }
                OpCode::SWAP2 => {
                    // Stack operation: [a, b, c, d, ^] -> [c, d, a, b, ^] where ^ is stack top
                    // Swaps pairs of values: first with third, second with fourth
                    // Effectively exchanges the top two pairs of values
                    std::mem::swap(
                        sp.offset(-1).as_mut().unwrap(),
                        sp.offset(-3).as_mut().unwrap(),
                    );
                    std::mem::swap(
                        sp.offset(-2).as_mut().unwrap(),
                        sp.offset(-4).as_mut().unwrap(),
                    );

                    // let tmp1 = SP![-4];
                    // let tmp2 = SP![-3];
                    // SP![-4] = SP![-2];
                    // SP![-3] = SP![-1];
                    // SP![-2] = tmp1;
                    // SP![-1] = tmp2;
                }
                OpCode::FCLOSURE => {
                    let id = get_u32(pc);
                    PC_GO!(4);

                    let bfunc = CPOOL![id].dup();
                    SP![0] = js_closure(ctx, bfunc, var_refs, sf);
                    SP_GO!(1);
                    if SP![-1].is_exception() {
                        break State::Exception;
                    }
                }

                #[cfg(feature = "short_opcode")]
                OpCode::CALL0..=OpCode::CALL3 => {
                    todo!();
                }

                OpCode::CALL | OpCode::TAIL_CALL => {
                    let call_argc = get_u16(pc) as i32;
                    PC_GO!(2);

                    // has_call_argc:
                    let call_argv = sp.offset(-(call_argc as isize));
                    sf.cur_pc_offset = pc.offset_from(pc_base) as _;
                    let ret_val = JS_CallInternal(
                        ctx,
                        *call_argv.offset(-1),
                        JSValue::UNDEFINED,
                        JSValue::UNDEFINED,
                        call_argc as _,
                        call_argv,
                        0,
                    );
                    if ret_val.is_exception() {
                        break State::Exception;
                    } else if opcode == OpCode::TAIL_CALL {
                        break State::Done(ret_val, false);
                    } else {
                        for i in -1..call_argc as i32 {
                            JS_FreeValue(ctx, *call_argv.offset(i as isize));
                        }
                        SP_GO!(-(call_argc + 1));
                        SP![0] = ret_val;
                        SP_GO!(1);
                    }
                }
                OpCode::CALL_CONSTRUCTOR => {
                    let call_argc = get_u16(pc) as i32;
                    pc = pc.add(2);
                    let call_argv = sp.offset(-call_argc as isize);
                    sf.cur_pc_offset = pc.offset_from(pc_base) as _;
                    let ret_val = JS_CallConstructorInternal(
                        ctx,
                        *call_argv.offset(-2),
                        *call_argv.offset(-1),
                        call_argc,
                        call_argv,
                        0,
                    );
                    if ret_val.is_exception() {
                        break State::Exception;
                    } else {
                        for i in -2..call_argc {
                            JS_FreeValue(ctx, *call_argv.offset(i as isize));
                        }
                        SP_GO!(-(call_argc + 2));
                        SP![0] = ret_val;
                        SP_GO!(1);
                    }
                }
                OpCode::CALL_METHOD | OpCode::TAIL_CALL_METHOD => {
                    let call_argc = get_u16(pc) as i32;
                    PC_GO!(2);

                    let call_argv = sp.offset(-call_argc as isize);
                    sf.cur_pc_offset = pc.offset_from(pc_base) as _;

                    let ret_val = JS_CallInternal(
                        ctx,
                        *call_argv.offset(-1),
                        *call_argv.offset(-2),
                        JSValue::UNDEFINED,
                        call_argc,
                        call_argv,
                        0,
                    );

                    if ret_val.is_exception() {
                        break State::Exception;
                    } else if opcode == OpCode::TAIL_CALL_METHOD {
                        break State::Done(ret_val, false);
                    } else {
                        for i in -2..call_argc {
                            JS_FreeValue(ctx, *call_argv.offset(i as isize));
                        }
                        SP_GO!(-(call_argc + 2));
                        SP![0] = ret_val;
                        SP_GO!(1);
                    }
                }
                OpCode::ARRAY_FROM => {
                    // el1, el2 ... elN ^ -> array ^
                    let call_argc = get_u16(pc) as i32;
                    PC_GO!(2);

                    // create array
                    let arr = JS_NewArray(ctx);
                    if arr.is_exception() {
                        break State::Exception;
                    } else {
                        // push elements to array
                        let call_argv = sp.offset(-call_argc as isize);
                        for i in 0..call_argc {
                            let ret = arr.define_property_value(
                                ctx,
                                JSAtom::from_u32(i as u32),
                                *call_argv.offset(i as isize),
                                JS_PROP_C_W_E | JS_PROP_THROW,
                            );
                            *call_argv.offset(i as isize) = JSValue::UNDEFINED;
                            if ret.is_err() {
                                JS_FreeValue(ctx, arr);
                                break 'opcodes State::Exception;
                            }
                        }

                        SP_GO!(-call_argc);
                        SP![0] = arr;
                        SP_GO!(1);
                    }
                }
                OpCode::APPLY => {
                    let magic = get_u16(pc) as i32;
                    pc = pc.add(2);

                    // let ret_val = js_function_apply(ctx, SP![-3], 2, sp.offset(-2), magic);
                    // if ret_val.is_exception() {
                    //     break State::Exception;
                    // } else {
                    //     JS_FreeValue(ctx, SP![-3]);
                    //     JS_FreeValue(ctx, SP![-2]);
                    //     JS_FreeValue(ctx, SP![-1]);
                    //     SP_GO!(-3);
                    //     SP![0] = ret_val;
                    //     SP_GO!(1);
                    // }

                    let cc = CallArgs::new(
                        NonNull::new_unchecked(ctx),
                        SP![-3],
                        std::slice::from_raw_parts(sp.offset(-2), 2),
                        magic,
                        None,
                    );
                    let ret_val = js_function_apply(ctx, cc).unwrap_or(JSValue::EXCEPTION);
                    if ret_val.is_exception() {
                        break State::Exception;
                    } else {
                        JS_FreeValue(ctx, SP![-3]);
                        JS_FreeValue(ctx, SP![-2]);
                        JS_FreeValue(ctx, SP![-1]);
                        SP_GO!(-3);
                        SP![0] = ret_val;
                        SP_GO!(1);
                    }
                }
                OpCode::RETURN => {
                    SP_GO!(-1);
                    break State::Done(SP![0], false);
                }
                OpCode::RETURN_UNDEF => {
                    break State::Done(JSValue::UNDEFINED, false);
                }
                OpCode::CHECK_CTOR_RETURN => {
                    // return TRUE if 'this' should be returned
                    if !SP![-1].is_object() {
                        if !SP![-1].is_undefined() {
                            caller_ctx.JS_ThrowTypeError(
                                c"derived class constructor must return an object or undefined",
                            );
                            break State::Exception;
                        } else {
                            SP![0] = JSValue::TRUE;
                            SP_GO!(1);
                        }
                    } else {
                        SP![0] = JSValue::FALSE;
                        SP_GO!(1);
                    }
                }
                OpCode::CHECK_CTOR => {
                    if new_target.is_undefined() {
                        ctx.JS_ThrowTypeError(c"class constructors must be invoked with 'new'");
                        break State::Exception;
                    }
                }
                OpCode::CHECK_BRAND => {
                    if let Ok(b) = JS_CheckBrand(ctx, SP![-2], SP![-1]) {
                        if !b {
                            ctx.JS_ThrowTypeError(c"invalid brand on object");
                            break State::Exception;
                        }
                    } else {
                        break State::Exception;
                    }
                }
                OpCode::ADD_BRAND => {
                    if JS_AddBrand(ctx, SP![-2], SP![-1]) < 0 {
                        break State::Exception;
                    } else {
                        JS_FreeValue(ctx, SP![-2]);
                        JS_FreeValue(ctx, SP![-1]);
                        SP_GO!(-2);
                    }
                }
                OpCode::THROW => {
                    SP_GO!(-1);
                    ctx.JS_Throw(SP![0]);
                    break State::Exception;
                }
                OpCode::THROW_ERROR => {
                    let atom = JSAtom(get_u32(pc));
                    let type_ = *pc.offset(4);
                    PC_GO!(5);

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
                            JS_ThrowReferenceError(ctx, c"unsupported reference to 'super'");
                        }
                        JS_THROW_ERROR_ITERATOR_THROW => {
                            ctx.JS_ThrowTypeError(c"iterator does not have a throw method");
                        }
                        _ => {
                            ctx.JS_ThrowInternalError(CStr::from_ptr(
                                format!("invalid throw var type {type_}\0").as_ptr().cast(),
                            ));
                        }
                    }
                    break State::Exception;
                }
                OpCode::EVAL => {
                    let call_argc = get_u16(pc) as i32;
                    // scope_idx will be -1 for top of scope chain
                    let scope_idx = get_u16(pc.offset(2)) as i32 - 1;
                    PC_GO!(4);

                    let call_argv = sp.offset(-call_argc as isize);
                    sf.cur_pc_offset = pc.offset_from(pc_base) as _;

                    let ret_val = if (*call_argv.offset(-1)).is_same(ctx.eval_func) {
                        JS_EvalObject(
                            ctx,
                            JSValue::UNDEFINED,
                            if call_argc > 0 {
                                *call_argv
                            } else {
                                JSValue::UNDEFINED
                            },
                            JS_EVAL_TYPE_DIRECT,
                            scope_idx as _,
                        )
                    } else {
                        JS_CallInternal(
                            ctx,
                            *call_argv.offset(-1),
                            JSValue::UNDEFINED,
                            JSValue::UNDEFINED,
                            call_argc,
                            call_argv,
                            0,
                        )
                    };

                    if ret_val.is_exception() {
                        break State::Exception;
                    } else {
                        for i in -1..call_argc {
                            JS_FreeValue(ctx, *call_argv.offset(i as isize));
                        }
                        sp = sp.offset(-call_argc as isize - 1);
                        SP![0] = ret_val;
                        SP_GO!(1);
                    }
                }
                OpCode::APPLY_EVAL => {
                    let scope_idx = get_u16(pc) as i32 - 1; // could be minus
                    PC_GO!(2);

                    if let Ok(mut tab) = build_arg_list(ctx, SP![-1]) {
                        let ret_val = if SP![-2].is_same(ctx.eval_func) {
                            JS_EvalObject(
                                ctx,
                                JSValue::UNDEFINED,
                                if tab.len() > 0 {
                                    tab[0]
                                } else {
                                    JSValue::UNDEFINED
                                },
                                JS_EVAL_TYPE_DIRECT,
                                scope_idx,
                            )
                        } else {
                            JS_Call(
                                ctx,
                                SP![-2],
                                JSValue::UNDEFINED,
                                tab.len() as _,
                                tab.as_mut_ptr(),
                            )
                        };

                        for v in tab {
                            JS_FreeValue(ctx, v);
                        }

                        if ret_val.is_exception() {
                            break State::Exception;
                        } else {
                            JS_FreeValue(ctx, SP![-2]);
                            JS_FreeValue(ctx, SP![-1]);
                            SP_GO!(-2);
                            SP![0] = ret_val;
                            SP_GO!(1);
                        }
                    } else {
                        break State::Exception;
                    };
                }
                OpCode::REGEXP => {
                    SP![-2] =
                        js_regexp_constructor_internal(ctx, JSValue::UNDEFINED, SP![-2], SP![-1]);
                    SP_GO!(-1);
                }
                OpCode::GET_SUPER => {
                    let proto = JS_GetPrototype(ctx, SP![-1]);
                    if proto.is_exception() {
                        break State::Exception;
                    } else {
                        JS_FreeValue(ctx, SP![-1]);
                        SP![-1] = proto;
                    }
                }
                OpCode::IMPORT => {
                    let val = js_dynamic_import(ctx, SP![-1]);
                    if val.is_exception() {
                        break State::Exception;
                    } else {
                        JS_FreeValue(ctx, SP![-1]);
                        SP![-1] = val;
                    }
                }
                OpCode::CHECK_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    let ret = JS_CheckGlobalVar(ctx, atom);
                    if ret < 0 {
                        break State::Exception;
                    } else {
                        SP![0] = JS_NewBool(ret != 0);
                        SP_GO!(1);
                    }
                }
                OpCode::GET_VAR_UNDEF | OpCode::GET_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    let val = JS_GetGlobalVar(ctx, atom, opcode == OpCode::GET_VAR);
                    if val.is_exception() {
                        break State::Exception;
                    } else {
                        SP![0] = val;
                        SP_GO!(1);
                    }
                }
                OpCode::PUT_VAR | OpCode::PUT_VAR_INIT => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    let ret = JS_SetGlobalVar(
                        ctx,
                        atom,
                        SP![-1],
                        (opcode as u8 - OpCode::PUT_VAR as u8) as _,
                    );
                    SP_GO!(-1);
                    if ret < 0 {
                        break State::Exception;
                    }
                }
                OpCode::PUT_VAR_STRICT => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    // sp[-2] is TRUE or FALSE
                    if JS_VALUE_GET_INT(SP![-2]) == 0 {
                        JS_ThrowReferenceErrorNotDefined(ctx, atom);
                        break State::Exception;
                    }

                    let ret = JS_SetGlobalVar(ctx, atom, SP![-1], 2);
                    SP_GO!(-2);
                    if ret < 0 {
                        break State::Exception;
                    }
                }
                OpCode::CHECK_DEFINE_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    let flags = *pc.offset(4);
                    PC_GO!(5);
                    if JS_CheckDefineGlobalVar(ctx, atom, flags as _) != 0 {
                        break State::Exception;
                    }
                }
                OpCode::DEFINE_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    let flags = *pc.offset(4);
                    PC_GO!(5);
                    if JS_DefineGlobalVar(ctx, atom, flags as _) != 0 {
                        break State::Exception;
                    }
                }
                OpCode::DEFINE_FUNC => {
                    let atom = JSAtom(get_u32(pc));
                    let flags = *pc.offset(4);
                    PC_GO!(5);
                    if JS_DefineGlobalFunction(ctx, atom, SP![-1], flags as _) != 0 {
                        break State::Exception;
                    } else {
                        JS_FreeValue(ctx, SP![-1]);
                        SP_GO!(-1);
                    }
                }
                OpCode::GET_LOC => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    SP![0] = VAR![idx].dup();
                    SP_GO!(1);
                }
                OpCode::PUT_LOC => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    set_value(ctx, VAR_PTR![idx], SP![-1]);
                    SP_GO!(-1);
                }
                OpCode::SET_LOC => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    set_value(ctx, VAR_PTR![idx], SP![-1].dup());
                }
                OpCode::SET_LOC_UNINITIALIZED => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    set_value(ctx, VAR_PTR![idx], JSValue::UNINITIALIZED);
                }
                OpCode::GET_ARG => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    SP![0] = ARG![idx].dup();
                    SP_GO!(1);
                }
                OpCode::PUT_ARG => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    set_value(ctx, ARG_REF![idx], SP![-1]);
                    SP_GO!(-1);
                }
                OpCode::SET_ARG => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    set_value(ctx, ARG_REF![idx], SP![-1].dup());
                }

                #[cfg(feature = "short_opcode")]
                OpCode::GET_LOC8..=OpCode::SET_VAR_REF3 => {
                    todo!()
                }

                OpCode::GET_VAR_REF => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    let val = var_refs[idx as usize].as_ref().pvalue;
                    SP![0] = (*val).dup();
                    SP_GO!(1);
                }
                OpCode::PUT_VAR_REF => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    set_value(ctx, var_refs[idx as usize].as_ref().pvalue, SP![-1]);
                    SP_GO!(-1);
                }
                OpCode::SET_VAR_REF => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    set_value(ctx, var_refs[idx as usize].as_ref().pvalue, SP![-1].dup());
                }
                OpCode::GET_VAR_REF_CHECK | OpCode::PUT_VAR_REF_CHECK => {
                    let idx = get_u16(pc);
                    PC_GO!(2);

                    let pval = var_refs[idx as usize].as_ref().pvalue;
                    if JS_IsUninitialized(*pval) {
                        JS_ThrowReferenceErrorUninitialized2(ctx, b, idx, true);
                        break State::Exception;
                    } else {
                        if opcode == OpCode::GET_VAR_REF_CHECK {
                            // GET_VAR_REF_CHECK
                            SP![0] = (*pval).dup();
                            SP_GO!(1);
                        } else {
                            // PUT_VAR_REF_CHECK
                            set_value(ctx, pval, SP![-1]);
                            SP_GO!(-1);
                        }
                    }
                }
                OpCode::PUT_VAR_REF_CHECK_INIT => {
                    let idx = get_u16(pc);
                    PC_GO!(2);

                    let pval = var_refs[idx as usize].as_ref().pvalue;
                    if !JS_IsUninitialized(*pval) {
                        JS_ThrowReferenceErrorUninitialized2(ctx, b, idx, true);
                        break State::Exception;
                    } else {
                        set_value(ctx, pval, SP![-1]);
                        SP_GO!(-1);
                    }
                }

                OpCode::GET_LOC_CHECK | OpCode::GET_LOC_CHECKTHIS => {
                    let idx = get_u16(pc);
                    PC_GO!(2);

                    let val = VAR![idx];
                    if JS_IsUninitialized(val) {
                        JS_ThrowReferenceErrorUninitialized2(
                            if opcode == OpCode::GET_LOC_CHECK {
                                ctx
                            } else {
                                caller_ctx
                            },
                            b,
                            idx,
                            false,
                        );
                        break State::Exception;
                    } else {
                        SP![0] = val.dup();
                        SP_GO!(1);
                    }
                }
                OpCode::PUT_LOC_CHECK | OpCode::PUT_LOC_CHECK_INIT => {
                    let idx = get_u16(pc);
                    PC_GO!(2);

                    let pval = VAR_PTR![idx];
                    if JS_IsUninitialized(*pval) {
                        if opcode == OpCode::PUT_LOC_CHECK {
                            JS_ThrowReferenceErrorUninitialized2(ctx, b, idx, false);
                        } else {
                            JS_ThrowReferenceError(ctx, c"'this' can be initialized only once");
                        }
                        break State::Exception;
                    } else {
                        set_value(ctx, pval, SP![-1]);
                        SP_GO!(-1);
                    }
                }
                OpCode::CLOSE_LOC => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    (*sf).close_lexical_var(ctx, idx, false);
                }
                OpCode::MAKE_LOC_REF | OpCode::MAKE_ARG_REF | OpCode::MAKE_VAR_REF_REF => {
                    let atom = JSAtom(get_u32(pc));
                    let idx = get_u16(pc.add(4));
                    PC_GO!(6);

                    SP![0] = JS_NewObjectProto(ctx, JSValue::NULL);
                    SP_GO!(1);

                    if SP![-1].is_exception() {
                        break State::Exception;
                    } else {
                        let var_ref = if opcode == OpCode::MAKE_VAR_REF_REF {
                            let mut vr = var_refs[idx as usize];
                            vr.as_mut().u0.header.ref_count += 1;
                            Ok(vr)
                        } else {
                            sf.get_var_ref(ctx, idx, opcode == OpCode::MAKE_ARG_REF)
                        };

                        match var_ref {
                            Ok(vr) => {
                                if let Ok(pr) = SP![-1].object().add_property(
                                    ctx,
                                    atom,
                                    JS_PROP_WRITABLE | JS_PROP_VARREF,
                                ) {
                                    (*pr).u.var_ref = vr.as_ptr();
                                    SP![0] = atom.to_value(ctx, false);
                                    SP_GO!(1);
                                } else {
                                    if let Some(p) = Box::from_raw(vr.as_ptr()).free(rt) {
                                        Box::leak(p); // var ref is still alive, don't drop
                                    }
                                    break State::Exception;
                                };
                            }
                            Err(_) => {
                                break State::Exception;
                            }
                        }
                    }
                }
                OpCode::MAKE_VAR_REF => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    if JS_GetGlobalVarRef(ctx, atom, sp) != 0 {
                        break State::Exception;
                    } else {
                        SP_GO!(2);
                    }
                }
                OpCode::GOTO => {
                    let n = get_u32(pc) as i32; // plus or minus
                    PC_GO!(n);
                    if js_poll_interrupts(ctx) != 0 {
                        break State::Exception;
                    }
                }

                #[cfg(feature = "short_opcode")]
                OpCode::GOTO8 | OpCode::GOTO16 => {
                    pc = pc.add(if opcode == OpCode::GOTO8 {
                        get_u8(pc)
                    } else {
                        get_u16(pc)
                    } as usize);

                    if js_poll_interrupts(ctx) != 0 {
                        state = State::Exception;
                    }
                }

                OpCode::IF_TRUE | OpCode::IF_FALSE => {
                    let b = opcode == OpCode::IF_TRUE;
                    let op1 = SP![-1];
                    SP_GO!(-1);
                    PC_GO!(4);

                    let res = if JS_VALUE_GET_TAG(op1) <= JS_TAG_UNDEFINED {
                        JS_VALUE_GET_INT(op1) != 0
                    } else {
                        // FIXME: handle error
                        JS_ToBoolFree(ctx, op1).unwrap_or(true)
                    };

                    if b == res {
                        pc = pc.offset(get_u32(pc.offset(-4)) as isize - 4);
                    }

                    if js_poll_interrupts(ctx) != 0 {
                        break State::Exception;
                    }
                }

                #[cfg(feature = "short_opcode")]
                OpCode::IF_TRUE8 | OpCode::IF_FALSE8 => {
                    todo!()
                }

                OpCode::CATCH => {
                    let diff = get_u32(pc) as i32;
                    SP![0] = JS_NewCatchOffset(pc.offset_from(pc_base) as i32 + diff);
                    SP_GO!(1);
                    PC_GO!(4);
                }
                OpCode::GOSUB => {
                    let diff = get_u32(pc) as i32;
                    /* XXX: should have a different tag to avoid security flaw */
                    SP![0] = JSValue::from_i32(pc.offset_from(pc_base) as i32 + 4);
                    SP_GO!(1);
                    PC_GO!(diff);
                }
                OpCode::RET => {
                    let op1 = SP![-1];
                    if JS_VALUE_GET_TAG(op1) != JS_TAG_INT {
                        ctx.JS_ThrowInternalError(c"invalid ret value");
                        break State::Exception;
                    } else {
                        let pos = JS_VALUE_GET_INT(op1);
                        if pos >= bytecodes.len() as i32 {
                            ctx.JS_ThrowInternalError(c"invalid ret value");
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                            pc = pc_base.offset(pos as isize);
                        }
                    }
                }
                OpCode::FOR_IN_START => {
                    if js_for_in_start(ctx, sp) != 0 {
                        break State::Exception;
                    }
                }
                OpCode::FOR_IN_NEXT => {
                    if js_for_in_next(ctx, sp) != 0 {
                        break State::Exception;
                    }
                    SP_GO!(2);
                }
                OpCode::FOR_OF_START => {
                    if js_for_of_start(ctx, sp, false) != 0 {
                        break State::Exception;
                    }
                    SP_GO!(1);
                    SP![0] = JS_NewCatchOffset(0);
                    SP_GO!(1);
                }
                OpCode::FOR_OF_NEXT => {
                    let offset = -3 - *pc as i32;
                    PC_GO!(1);
                    if js_for_of_next(ctx, sp, offset) != 0 {
                        break State::Exception;
                    }
                    SP_GO!(2);
                }
                OpCode::FOR_AWAIT_OF_START => {
                    if js_for_of_start(ctx, sp, true) != 0 {
                        break State::Exception;
                    } else {
                        SP_GO!(1);
                        SP![0] = JS_NewCatchOffset(0);
                        SP_GO!(1);
                    }
                }
                OpCode::ITERATOR_GET_VALUE_DONE => {
                    if js_iterator_get_value_done(ctx, sp) != 0 {
                        break State::Exception;
                    } else {
                        SP_GO!(1);
                    }
                }
                OpCode::ITERATOR_CHECK_OBJECT => {
                    if !SP![-1].is_object() {
                        ctx.JS_ThrowTypeError(c"iterator must return an object");
                        break State::Exception;
                    }
                }
                OpCode::ITERATOR_CLOSE => {
                    // iter_obj next catch_offset ->
                    SP_GO!(-1); /* drop the catch offset to avoid getting caught by exception */
                    JS_FreeValue(ctx, SP![-1]); // drop the next method
                    SP_GO!(-1);
                    if !SP![-1].is_undefined() {
                        if JS_IteratorClose(ctx, SP![-1], false).is_err() {
                            break State::Exception;
                        } else {
                            JS_FreeValue(ctx, SP![-1]);
                        }
                    }
                    SP_GO!(-1);
                }
                OpCode::NIP_CATCH => {
                    // catch_offset ... ret_val -> ret_eval
                    SP_GO!(-1);
                    let ret_val = SP![0];
                    while sp > sp_base && JS_VALUE_GET_TAG(SP![-1]) != JS_TAG_CATCH_OFFSET {
                        SP_GO!(-1);
                        JS_FreeValue(ctx, SP![0]);
                    }
                    if sp == sp_base {
                        ctx.JS_ThrowInternalError(c"nip_catch");
                        JS_FreeValue(ctx, ret_val);
                        break State::Exception;
                    } else {
                        SP![-1] = ret_val;
                    }
                }
                OpCode::ITERATOR_NEXT => {
                    // stack: iter_obj next catch_offset val
                    let ret = JS_Call(ctx, SP![-3], SP![-4], 1, sp.offset(-1));
                    if ret.is_exception() {
                        break State::Exception;
                    } else {
                        JS_FreeValue(ctx, SP![-1]);
                        SP![-1] = ret;
                    }
                }
                OpCode::ITERATOR_CALL => {
                    // stack: iter_obj next catch_offset val
                    let flags = *pc as i32;
                    PC_GO!(1);

                    let method = SP![-4].get_property(
                        ctx,
                        if flags & 1 != 0 {
                            JS_ATOM_throw
                        } else {
                            JS_ATOM_return
                        },
                    );
                    if method.is_exception() {
                        break State::Exception;
                    } else {
                        let ret_flag = if method.is_undefined_or_null() {
                            true
                        } else {
                            let ret = if flags & 2 != 0 {
                                // no argument
                                JS_CallFree(ctx, method, SP![-4], 0, std::ptr::null_mut())
                            } else {
                                JS_CallFree(ctx, method, SP![-4], 1, sp.offset(-1))
                            };
                            if ret.is_exception() {
                                break State::Exception;
                            } else {
                                JS_FreeValue(ctx, SP![-1]);
                                SP![-1] = ret;
                            }
                            false
                        };

                        SP![0] = JS_NewBool(ret_flag);
                        SP_GO!(1);
                    }
                }
                OpCode::LNOT => {
                    let op1 = SP![-1];
                    let res = if JS_VALUE_GET_TAG(op1) <= JS_TAG_UNDEFINED {
                        JS_VALUE_GET_INT(op1) != 0
                    } else {
                        JS_ToBoolFree(ctx, op1).unwrap_or(true)
                    };
                    SP![-1] = JS_NewBool(!res);
                }
                OpCode::GET_FIELD | OpCode::GET_FIELD2 => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    let val = SP![-1].get_property(ctx, atom);
                    if val.is_exception() {
                        break State::Exception;
                    } else {
                        if opcode == OpCode::GET_FIELD {
                            JS_FreeValue(ctx, SP![-1]);
                            SP![-1] = val;
                        } else {
                            SP![0] = val;
                            SP_GO!(1)
                        }
                    }
                }
                OpCode::PUT_FIELD => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    let ret = SP![-2].set_property_internal(
                        ctx,
                        atom,
                        SP![-1],
                        SP![-2],
                        JS_PROP_THROW_STRICT,
                    );
                    JS_FreeValue(ctx, SP![-2]);
                    SP_GO!(-2);
                    if ret.is_err() {
                        break State::Exception;
                    }
                }
                OpCode::PRIVATE_SYMBOL => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    let val = JS_NewSymbolFromAtom(ctx, atom, JSAtomTypeHint::Private);
                    if val.is_exception() {
                        break State::Exception;
                    } else {
                        SP![0] = val;
                        SP_GO!(1);
                    }
                }
                OpCode::GET_PRIVATE_FIELD => {
                    let val = JS_GetPrivateField(ctx, SP![-2], SP![-1]);
                    JS_FreeValue(ctx, SP![-1]);
                    JS_FreeValue(ctx, SP![-2]);
                    SP![-2] = val;
                    SP_GO!(-1);
                    if val.is_exception() {
                        break State::Exception;
                    }
                }
                OpCode::PUT_PRIVATE_FIELD => {
                    let ret = JS_SetPrivateField(ctx, SP![-3], SP![-1], SP![-2]);
                    JS_FreeValue(ctx, SP![-3]);
                    JS_FreeValue(ctx, SP![-1]);
                    SP_GO!(-3);
                    if ret < 0 {
                        break State::Exception;
                    }
                }
                OpCode::DEFINE_PRIVATE_FIELD => {
                    let ret = JS_DefinePrivateField(ctx, SP![-3], SP![-2], SP![-1]);
                    JS_FreeValue(ctx, SP![-2]);
                    SP_GO!(-2);
                    if ret < 0 {
                        break State::Exception;
                    }
                }
                OpCode::DEFINE_FIELD => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    let ret = SP![-2].define_property_value(
                        ctx,
                        atom,
                        SP![-1],
                        JS_PROP_C_W_E | JS_PROP_THROW,
                    );
                    SP_GO!(-1);
                    if ret.is_err() {
                        break State::Exception;
                    }
                }
                OpCode::SET_NAME => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    let ret = JS_DefineObjectName(ctx, SP![-1], atom, JS_PROP_CONFIGURABLE);
                    if ret < 0 {
                        break State::Exception;
                    }
                }
                OpCode::SET_NAME_COMPUTED => {
                    let ret =
                        JS_DefineObjectNameComputed(ctx, SP![-1], SP![-2], JS_PROP_CONFIGURABLE);
                    if ret < 0 {
                        break State::Exception;
                    }
                }
                OpCode::SET_PROTO => {
                    let proto = SP![-1];
                    if proto.is_object() || proto.is_null() {
                        if JS_SetPrototypeInternal(ctx, SP![-2], proto, true).is_err() {
                            break State::Exception;
                        }
                        JS_FreeValue(ctx, proto);
                        SP_GO!(-1);
                    }
                }
                OpCode::SET_HOME_OBJECT => {
                    js_method_set_home_object(ctx, SP![-1], SP![-2]);
                }
                OpCode::DEFINE_METHOD | OpCode::DEFINE_METHOD_COMPUTED => {
                    let is_computed = opcode == OpCode::DEFINE_METHOD_COMPUTED;
                    let atom = if is_computed {
                        JS_ValueToAtom(ctx, SP![-2])
                    } else {
                        let a = JSAtom(get_u32(pc));
                        PC_GO!(4);
                        a
                    };
                    if atom == JS_ATOM_NULL {
                        break State::Exception;
                    } else {
                        let mut op_flags = *pc;
                        PC_GO!(1);

                        let obj = SP![if is_computed { -3 } else { -2 }];
                        let mut flags = JS_PROP_HAS_CONFIGURABLE
                            | JS_PROP_CONFIGURABLE
                            | JS_PROP_HAS_ENUMERABLE
                            | JS_PROP_THROW;
                        if op_flags & OP_DEFINE_METHOD_ENUMERABLE != 0 {
                            flags |= JS_PROP_ENUMERABLE;
                        }
                        op_flags &= 3;
                        let mut value = JSValue::UNDEFINED;
                        let mut getter = JSValue::UNDEFINED;
                        let mut setter = JSValue::UNDEFINED;

                        match op_flags {
                            OP_DEFINE_METHOD_METHOD => {
                                value = SP![-1];
                                flags |=
                                    JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE;
                            }
                            OP_DEFINE_METHOD_GETTER => {
                                getter = SP![-1];
                                flags |= JS_PROP_HAS_GET;
                            }
                            OP_DEFINE_METHOD_SETTER => {
                                setter = SP![-1];
                                flags |= JS_PROP_HAS_SET;
                            }
                            _ => unreachable!(),
                        }
                        let mut ret = js_method_set_properties(ctx, SP![-1], atom, flags, obj);
                        if ret >= 0 {
                            ret = obj
                                .define_property(ctx, atom, value, getter, setter, flags)
                                .map_or(-1, |b| b as i32);
                        }
                        JS_FreeValue(ctx, SP![-1]);
                        if is_computed {
                            JS_FreeAtom(ctx, atom);
                            JS_FreeValue(ctx, SP![-2]);
                        }
                        SP_GO!(if is_computed { -2 } else { -1 });
                        if ret < 0 {
                            break State::Exception;
                        }
                    }
                }
                OpCode::DEFINE_CLASS | OpCode::DEFINE_CLASS_COMPUTED => {
                    let atom = JSAtom(get_u32(pc));
                    let class_flags = *pc.add(4) as u32;
                    PC_GO!(5);

                    if js_op_define_class(
                        ctx,
                        sp,
                        atom,
                        class_flags as _,
                        var_refs,
                        sf,
                        opcode == OpCode::DEFINE_CLASS_COMPUTED,
                    )
                    .is_err()
                    {
                        break State::Exception;
                    }
                }
                OpCode::GET_ARRAY_EL => {
                    let val = SP![-2].get_property_value(ctx, SP![-1]);
                    JS_FreeValue(ctx, SP![-2]);
                    SP![-2] = val;
                    SP_GO!(-1);
                    if val.is_exception() {
                        break State::Exception;
                    }
                }
                OpCode::GET_ARRAY_EL2 => {
                    let val = SP![-2].get_property_value(ctx, SP![-1]);
                    SP![-1] = val;
                    if val.is_exception() {
                        break State::Exception;
                    }
                }
                OpCode::GET_REF_VALUE => {
                    if SP![-2].is_undefined() {
                        let atom = JS_ValueToAtom(ctx, SP![-1]);
                        if atom != JS_ATOM_NULL {
                            JS_ThrowReferenceErrorNotDefined(ctx, atom);
                            JS_FreeAtom(ctx, atom);
                        }
                        break State::Exception;
                    } else {
                        let val = SP![-2].get_property_value(ctx, SP![-1].dup());
                        if val.is_exception() {
                            break State::Exception;
                        } else {
                            SP![0] = val;
                            SP_GO!(1);
                        }
                    }
                }
                OpCode::GET_SUPER_VALUE => {
                    let atom = JS_ValueToAtom(ctx, SP![-1]);
                    if atom == JS_ATOM_NULL {
                        break State::Exception;
                    }

                    let val = SP![-2].get_property_internal(ctx, atom, SP![-3], false);
                    JS_FreeAtom(ctx, atom);
                    if val.is_exception() {
                        break State::Exception;
                    }

                    JS_FreeValue(ctx, SP![-1]);
                    JS_FreeValue(ctx, SP![-2]);
                    JS_FreeValue(ctx, SP![-3]);
                    SP![-3] = val;
                    SP_GO!(-2);
                }
                OpCode::PUT_ARRAY_EL => {
                    let ret =
                        SP![-3].set_property_value(ctx, SP![-2], SP![-1], JS_PROP_THROW_STRICT);
                    JS_FreeValue(ctx, SP![-3]);
                    SP_GO!(-3);
                    if ret.is_err() {
                        break State::Exception;
                    }
                }
                OpCode::PUT_REF_VALUE => {
                    let mut flags = JS_PROP_THROW_STRICT;
                    if SP![-3].is_undefined() {
                        if is_strict_mode(ctx) {
                            let atom = JS_ValueToAtom(ctx, SP![-2]);
                            if atom != JS_ATOM_NULL {
                                JS_ThrowReferenceErrorNotDefined(ctx, atom);
                                JS_FreeAtom(ctx, atom);
                            }
                            break State::Exception;
                        } else {
                            SP![-3] = ctx.global_obj.dup();
                        }
                    } else {
                        if is_strict_mode(ctx) {
                            flags |= JS_PROP_NO_ADD;
                        }
                    }

                    let ret = SP![-3].set_property_value(ctx, SP![-2], SP![-1], flags);
                    JS_FreeValue(ctx, SP![-3]);
                    SP_GO!(-3);
                    if ret.is_err() {
                        break State::Exception;
                    }
                }
                OpCode::PUT_SUPER_VALUE => {
                    if JS_VALUE_GET_TAG(SP![-3]) != JS_TAG_OBJECT {
                        ctx.JS_ThrowTypeErrorNotAnObject();
                        break State::Exception;
                    }

                    let atom = JS_ValueToAtom(ctx, SP![-2]);
                    if atom == JS_ATOM_NULL {
                        break State::Exception;
                    } else {
                        let ret = SP![-3].set_property_internal(
                            ctx,
                            atom,
                            SP![-1],
                            SP![-4],
                            JS_PROP_THROW_STRICT,
                        );
                        JS_FreeAtom(ctx, atom);
                        JS_FreeValue(ctx, SP![-4]);
                        JS_FreeValue(ctx, SP![-3]);
                        JS_FreeValue(ctx, SP![-2]);
                        SP_GO!(-4);
                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                }
                OpCode::DEFINE_ARRAY_EL => {
                    let ret = SP![-3].define_property_value_value(
                        ctx,
                        SP![-2].dup(),
                        SP![-1],
                        JS_PROP_C_W_E | JS_PROP_THROW,
                    );
                    SP_GO!(-1);
                    if ret.is_err() {
                        break State::Exception;
                    }
                }
                OpCode::APPEND => {
                    // dest start_pos src ^ -> dest end_pos ^
                    // all elements of `src` append to `dest` array staring from `start_pos` index
                    if js_append_enumerate(ctx, sp) != 0 {
                        break State::Exception;
                    } else {
                        // drop `src`
                        SP_GO!(-1);
                        JS_FreeValue(ctx, SP![0]);
                    }
                }
                OpCode::COPY_DATA_PROPERTIES => {
                    /* target source excludeList */
                    /* stack offsets (-1 based):
                    2 bits for target,
                    3 bits for source,
                    2 bits for exclusionList */
                    let mask = *pc as i32;
                    PC_GO!(1);

                    if SP![-1 - (mask & 3)]
                        .copy_properties_from(
                            ctx,
                            SP![-1 - ((mask >> 2) & 7)],
                            SP![-1 - ((mask >> 5) & 7)],
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
                    let is_add = opcode == OpCode::ADD;
                    let binop = BinaryOp::new_slow(
                        ctx,
                        SP![-2],
                        SP![-1],
                        if is_add { 3 } else { 2 }, // allow string for add
                        if is_add { Some(HINT_NONE) } else { None },
                    );

                    if let Ok(res) = binop.and_then(|bop| match opcode {
                        OpCode::ADD => bop.add(ctx),
                        OpCode::SUB => bop.sub(ctx),
                        OpCode::MUL => bop.mul(ctx),
                        OpCode::DIV => bop.div(ctx),
                        OpCode::MOD => bop.mod_(ctx),
                        OpCode::POW => bop.pow(ctx),
                        _ => unreachable!(),
                    }) {
                        SP![-2] = res;
                        SP_GO!(-1);
                    } else {
                        SP![-1] = JSValue::UNDEFINED; // value has been freed
                        SP![-2] = JSValue::UNDEFINED; // value has been freed
                        break State::Exception;
                    }
                }
                OpCode::ADD_LOC => {
                    let idx = *pc as i32;
                    PC_GO!(1);

                    let op1 = SP![-1];
                    SP_GO!(-1);

                    let pv = VAR_PTR![idx];

                    // *pv and op1 will be consumed.
                    // so we duplicate *pv in advance
                    if let Ok(val) = BinaryOp::new_slow(ctx, (*pv).dup(), op1, 3, Some(HINT_NONE))
                        .and_then(|bop| bop.add(ctx))
                    {
                        set_value(ctx, pv, val);
                    } else {
                        break State::Exception;
                    }
                }
                OpCode::PLUS | OpCode::NEG | OpCode::INC | OpCode::DEC => {
                    let op1 = SP![-1];

                    if opcode == OpCode::PLUS
                        && matches!(JS_VALUE_GET_NORM_TAG(op1), JSTag::Int | JSTag::Float64)
                    {
                        // do nothing
                    } else {
                        if let Ok(res) = UnaryOp::new(ctx, op1, true).map(|unary| match opcode {
                            OpCode::PLUS => unary.plus(),
                            OpCode::NEG => unary.neg(),
                            OpCode::INC => unary.inc(),
                            OpCode::DEC => unary.dec(),
                            _ => unreachable!(),
                        }) {
                            SP![-1] = res;
                        } else {
                            SP![-1] = JSValue::UNDEFINED;
                            break State::Exception;
                        }
                    }
                }
                OpCode::POST_INC | OpCode::POST_DEC => {
                    let op1 = SP![-1];
                    let n1 = JS_ToNumberFree(ctx, op1);
                    if n1.is_exception() {
                        SP![-1] = JSValue::UNDEFINED;
                        break State::Exception;
                    } else {
                        SP![-1] = n1;
                        if let Ok(res) = UnaryOp::new(ctx, n1.dup(), true).map(|unary| match opcode
                        {
                            OpCode::POST_INC => unary.inc(),
                            OpCode::POST_DEC => unary.dec(),
                            _ => unreachable!(),
                        }) {
                            SP![0] = res;
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                }
                OpCode::INC_LOC | OpCode::DEC_LOC => {
                    let idx = *pc as i32;
                    PC_GO!(1);

                    let op1 = VAR![idx];

                    // must duplicate `op1` otherwise the variable value may be destroyed before JS code accesses it
                    if let Ok(res) = UnaryOp::new(ctx, op1.dup(), true).map(|unary| match opcode {
                        OpCode::INC_LOC => unary.inc(),
                        OpCode::DEC_LOC => unary.dec(),
                        _ => unreachable!(),
                    }) {
                        set_value(ctx, VAR_PTR![idx], res);
                    } else {
                        break State::Exception;
                    }
                }
                OpCode::NOT => {
                    if let Ok(res) = UnaryOp::new_int(ctx, SP![-1].dup()).map(|unary| unary.not()) {
                        SP![-1] = res;
                    } else {
                        SP![-1] = JSValue::UNDEFINED;
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
                    if let Ok(b) = relational(ctx, SP![-2], SP![-1], opcode) {
                        SP![-2] = JS_NewBool(b);
                        SP_GO!(-1);
                    } else {
                        break State::Exception;
                    }
                }
                OpCode::EQ | OpCode::NEQ => {
                    if let Ok(b) = js_eq(ctx, sp, opcode == OpCode::NEQ) {
                        SP![-2] = JS_NewBool(b);
                        SP_GO!(-1);
                    } else {
                        break State::Exception;
                    }
                }
                OpCode::STRICT_EQ | OpCode::STRICT_NEQ => {
                    let mut b = SP![-2].strict_eq(SP![-1]);
                    if opcode == OpCode::STRICT_NEQ {
                        b = !b;
                    }

                    // free oprands
                    JS_FreeValue(ctx, SP![-2]);
                    JS_FreeValue(ctx, SP![-1]);

                    SP![-2] = JS_NewBool(b);
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
                    let op1 = SP![-1];
                    let atom = js_operator_typeof(ctx, op1);
                    JS_FreeValue(ctx, op1);
                    SP![-1] = atom.to_value(ctx, true);
                }
                OpCode::DELETE => {
                    if js_operator_delete(ctx, sp) != 0 {
                        break State::Exception;
                    } else {
                        SP_GO!(-1);
                    }
                }
                OpCode::DELETE_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    match ctx.global_obj.delete_property(ctx, atom, 0) {
                        Ok(ret) => {
                            SP![0] = JS_NewBool(ret);
                            SP_GO!(1);
                        }
                        Err(_) => {
                            break State::Exception;
                        }
                    }
                }
                OpCode::TO_PROPKEY => match JS_VALUE_GET_TAG(SP![-1]) {
                    JS_TAG_INT | JS_TAG_STRING | JS_TAG_SYMBOL => {}
                    _ => {
                        let ret_val = JS_ToPropertyKey(ctx, SP![-1]);
                        if ret_val.is_exception() {
                            break State::Exception;
                        } else {
                            JS_FreeValue(ctx, SP![-1]);
                            SP![-1] = ret_val;
                        }
                    }
                },

                OpCode::TO_PROPKEY2 => {
                    // must be tested first
                    if SP![-2].is_undefined_or_null() {
                        ctx.JS_ThrowTypeError(c"value has property");
                        break State::Exception;
                    } else {
                        match JS_VALUE_GET_TAG(SP![-1]) {
                            JS_TAG_INT | JS_TAG_STRING | JS_TAG_SYMBOL => {}
                            _ => {
                                let ret_val = JS_ToPropertyKey(ctx, SP![-1]);
                                if ret_val.is_exception() {
                                    break State::Exception;
                                } else {
                                    JS_FreeValue(ctx, SP![-1]);
                                    SP![-1] = ret_val;
                                }
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
                    let atom = JSAtom(get_u32(pc));
                    let diff = get_u32(pc.offset(4));
                    let is_with = *pc.offset(8);
                    PC_GO!(9);

                    let obj = SP![-1];
                    if let Ok(ret) = obj.has_property(ctx, atom) {
                        let has_with = if ret {
                            if is_with != 0 {
                                let r = js_has_unscopable(ctx, obj, atom);
                                if r < 0 {
                                    break State::Exception;
                                }

                                if r != 0 {
                                    false // no with
                                } else {
                                    true
                                }
                            } else {
                                true
                            }
                        } else {
                            false // no with
                        };

                        if has_with {
                            match opcode {
                                OpCode::WITH_GET_VAR => {
                                    let val = obj.get_property(ctx, atom);
                                    if val.is_exception() {
                                        break State::Exception;
                                    } else {
                                        set_value(ctx, sp.offset(-1), val);
                                    }
                                }
                                OpCode::WITH_PUT_VAR => {
                                    // XXX: check if strict mode
                                    let ret = obj.set_property_internal(
                                        ctx,
                                        atom,
                                        SP![-2],
                                        obj,
                                        JS_PROP_THROW_STRICT,
                                    );
                                    JS_FreeValue(ctx, SP![-1]);
                                    SP_GO!(-2);
                                    if ret.is_err() {
                                        break State::Exception;
                                    }
                                }
                                OpCode::WITH_DELETE_VAR => {
                                    if let Ok(ret) = obj.delete_property(ctx, atom, 0) {
                                        JS_FreeValue(ctx, SP![-1]);
                                        SP![-1] = JS_NewBool(ret);
                                    } else {
                                        break State::Exception;
                                    }
                                }
                                OpCode::WITH_MAKE_REF => {
                                    // produce a pair object/propname on the stack
                                    SP![0] = atom.to_value(ctx, false);
                                    SP_GO!(1);
                                }
                                OpCode::WITH_GET_REF => {
                                    // produce a pair object/method on the stack
                                    let val = obj.get_property(ctx, atom);
                                    if val.is_exception() {
                                        break State::Exception;
                                    } else {
                                        SP![0] = val;
                                        SP_GO!(1);
                                    }
                                }
                                OpCode::WITH_GET_REF_UNDEF => {
                                    // produce a pair undefined/function on the stack
                                    let val = obj.get_property(ctx, atom);
                                    if val.is_exception() {
                                        break State::Exception;
                                    } else {
                                        JS_FreeValue(ctx, SP![-1]);
                                        SP![-1] = JSValue::UNDEFINED;
                                        SP![0] = val;
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
                            JS_FreeValue(ctx, SP![-1]);
                            SP_GO!(-1);
                        }
                    } else {
                        break State::Exception;
                    }
                }

                OpCode::AWAIT => {
                    break State::Done(JSValue::from_i32(FUNC_RET_AWAIT), true);
                }
                OpCode::YIELD => {
                    break State::Done(JSValue::from_i32(FUNC_RET_YIELD), true);
                }
                OpCode::YIELD_STAR | OpCode::ASYNC_YIELD_STAR => {
                    break State::Done(JSValue::from_i32(FUNC_RET_YIELD_STAR), true);
                }
                OpCode::RETURN_ASYNC => {
                    break State::Done(JSValue::UNDEFINED, true);
                }
                OpCode::INITIAL_YIELD => {
                    break State::Done(JSValue::from_i32(FUNC_RET_INITIAL_YIELD), true);
                }

                OpCode::NOP => {}

                OpCode::IS_UNDEFINED_OR_NULL => {
                    if SP![-1].is_undefined_or_null() {
                        SP![-1] = JSValue::TRUE;
                    } else {
                        JS_FreeValue(ctx, SP![-1]);
                        SP![-1] = JSValue::FALSE;
                    }
                }

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
                    ctx.JS_ThrowInternalError(CStr::from_ptr(
                        format!(
                            "invalid opcode: pc={}, opcode=0x{:02x}\0",
                            pc.offset_from(pc_base) - 1,
                            opcode as u8
                        )
                        .as_ptr()
                        .cast(),
                    ));
                    break State::Exception;
                }
            } // match opcode
        }; // opcodes loop

        // process state
        match state {
            State::Exception => {
                // calc offset index from pc and sp pointers
                let pc_offset = pc.offset_from(pc_base) as usize;
                let sp_offset = sp.offset_from(sp_base) as usize;

                if let Some((new_pc, new_sp)) = sf.handle_exception(ctx, pc_offset, sp_offset) {
                    // continue running, update pc and sp pointer, and jump
                    pc = pc_base.offset(new_pc as _);
                    sp = sp_base.offset(new_sp as _);
                    continue 'restart;
                } else {
                    // abort with sp=0, pc unchanged
                    sf.handle_exit(
                        ctx,
                        pc_offset,
                        0,
                        b.func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL,
                    );
                    return JSValue::EXCEPTION;
                }
            }

            State::Done(ret_val, is_generator) => {
                sf.handle_exit(
                    ctx,
                    pc.offset_from(pc_base) as usize,
                    sp.offset_from(sp_base) as usize,
                    is_generator,
                );
                return ret_val;
            }
        }
    } // 'restart loop
}
