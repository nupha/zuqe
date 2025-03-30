use std::{ffi::CStr, ptr::NonNull};

use crate::{
    array::JS_NewArray,
    atom::{
        consts::{JS_ATOM_return, JS_ATOM_throw},
        JSAtom, JSAtomType, JSAtomTypeHint, JS_FreeAtom, JS_ValueToAtom, JS_ATOM_NULL,
    },
    bytecode::{
        arithmatic::{
            js_eq, js_same_value, js_strict_eq, op_binary_logic, relational, BinaryOp, UnaryOp,
        },
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
        JS_ThrowSyntaxErrorVarRedeclaration, JS_ThrowTypeErrorReadOnly,
        JS_THROW_ERROR_DELETE_SUPER, JS_THROW_ERROR_ITERATOR_THROW, JS_THROW_VAR_REDECL,
        JS_THROW_VAR_RO, JS_THROW_VAR_UNINITIALIZED,
    },
    eval::{JS_EvalObject, JS_EVAL_TYPE_DIRECT},
    function::{
        build_arg_list, free_arg_list, js_build_arguments, js_build_mapped_arguments,
        js_build_rest, js_closure, js_function_apply, js_poll_interrupts, JSFunctionBytecode,
        JSFunctionKindEnum, JSVarRef, JS_Call, JS_CallConstructorInternal, JS_CallFree,
        JS_CallInternal,
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
            JSObject, JS_AddBrand, JS_CheckBrand, JS_CopyDataProperties, JS_DefineObjectName,
            JS_DefinePrivateField, JS_DefineProperty, JS_DefinePropertyValue,
            JS_DefinePropertyValueValue, JS_DeleteProperty, JS_GetPrivateField, JS_GetProperty,
            JS_GetPropertyValue, JS_GetPrototype, JS_HasProperty, JS_NewObject, JS_NewObjectProto,
            JS_SetPrivateField, JS_SetPropertyInternal, JS_SetPropertyValue,
            JS_SetPrototypeInternal, JS_ToObject, JS_ToPropertyKey, JS_PROP_CONFIGURABLE,
            JS_PROP_C_W_E, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE,
            JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE,
            JS_PROP_NO_ADD, JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_VARREF, JS_PROP_WRITABLE,
        },
        set_value, JSTag, JSValue, JS_FreeValue, JS_IsObject, JS_IsUninitialized, JS_NewBool,
        JS_NewCatchOffset, JS_NewSymbolFromAtom, JS_ToBoolFree, JS_ToNumberFree, HINT_NONE,
        JS_TAG_CATCH_OFFSET, JS_TAG_INT, JS_TAG_NULL, JS_TAG_OBJECT, JS_TAG_STRING, JS_TAG_SYMBOL,
        JS_TAG_UNDEFINED, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_TAG,
    },
    JS_MKPTR,
};

enum GotoLabel {
    Exception,
    /// exit execution. reture value and is_generator flag
    Done(JSValue, bool),
}

pub unsafe fn run_bytecodes(
    b: &JSFunctionBytecode,
    func_home_obj: *mut JSObject,
    sf: &mut JSStackFrame,      // 执行栈帧
    caller_ctx: &mut JSContext, // 调用者context
    this_obj: JSValue,
    new_target: JSValue,
    bytecodes: &[u8],      // 字节码
    pc_offset: usize,      // 当前指令index地址
    sp_base: *mut JSValue, // stack 起始地址指针
    mut sp: *mut JSValue,  // 当前 stack 指针
    var_refs: &[NonNull<JSVarRef>],
    argc: i32,
    argv: *mut JSValue,
    mode: i32,
    throw: bool,
    local_buf: &mut [JSValue],
) -> JSValue {
    debug_assert!(pc_offset < bytecodes.len());

    let cpools = b.cpools_slice();
    let ctx = b.realm.as_mut().unwrap();
    let rt = ctx.rt.as_mut();

    let pc_base = bytecodes.as_ptr();
    let mut pc = pc_base.add(pc_offset); // convert index to pointer

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

    let mut goto: Option<GotoLabel> = if throw {
        Some(GotoLabel::Exception)
    } else {
        None
    };

    'restart: loop {
        // dispatch state label
        while let Some(label) = goto.take() {
            match label {
                GotoLabel::Exception => {
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

                GotoLabel::Done(ret_val, is_generator) => {
                    sf.handle_exit(
                        ctx,
                        pc.offset_from(pc_base) as usize,
                        sp.offset_from(sp_base) as usize,
                        is_generator,
                    );
                    return ret_val;
                }
            }
        }

        // loop bytecodes
        loop {
            #[cfg(feature = "dump_bytecode")]
            {
                print!("[O.o] RUN {:5}: ", pc.offset_from(pc_base));
                crate::bytecode::dump::dump_opcode_one(ctx, b, pc.offset_from(pc_base) as usize);
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
                    /* OP_push_this is only called at the start of a function */
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
                        goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::SPECIAL_OBJECT => {
                    let arg = std::mem::transmute::<u8, OPSpecialObjectEnum>(*pc);
                    PC_GO!(1);

                    match arg {
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_ARGUMENTS => {
                            SP![0] = js_build_arguments(ctx, argc, argv);
                            if SP![0].is_exception() {
                                goto = Some(GotoLabel::Exception);
                            }
                            SP_GO!(1);
                        }
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_MAPPED_ARGUMENTS => {
                            SP![0] = js_build_mapped_arguments(
                                ctx,
                                argc,
                                argv,
                                sf,
                                std::cmp::min(argc, b.arg_count as i32),
                            );
                            if SP![0].is_exception() {
                                goto = Some(GotoLabel::Exception);
                            }
                            SP_GO!(1);
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
                            if SP![0].is_exception() {
                                goto = Some(GotoLabel::Exception);
                            }
                            SP_GO!(1);
                        }
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_IMPORT_META => {
                            SP![0] = js_import_meta(ctx);
                            if SP![0].is_exception() {
                                goto = Some(GotoLabel::Exception);
                            }
                            SP_GO!(0);
                        }
                    }
                }
                OpCode::REST => {
                    let first = get_u16(pc);
                    PC_GO!(2);
                    SP![0] = js_build_rest(ctx, first as _, argc, argv);
                    if SP![0].is_exception() {
                        goto = Some(GotoLabel::Exception);
                    }
                    SP_GO!(1);
                }
                OpCode::DROP => {
                    SP_GO!(-1);
                    JS_FreeValue(ctx, *sp);
                }
                OpCode::NIP => {
                    // a b -> b
                    JS_FreeValue(ctx, SP![-2]);
                    SP![-2] = SP![-1];
                    SP_GO!(-1);
                }
                OpCode::NIP1 => {
                    // a b c ^ -> b c ^
                    JS_FreeValue(ctx, SP![-3]);
                    SP![-3] = SP![-2];
                    SP![-2] = SP![-1];
                    SP_GO!(-1);
                }
                OpCode::DUP => {
                    // a ^ -> a a ^
                    SP![0] = SP![-1].dup();
                    SP_GO!(1);
                }
                OpCode::DUP2 => {
                    // a b ^ -> a b a b ^
                    SP![0] = SP![-2].dup();
                    *sp.add(1) = SP![-1].dup();
                    SP_GO!(2);
                }
                OpCode::DUP3 => {
                    // a b c ^ -> a b c a b c ^
                    SP![0] = SP![-3].dup();
                    *sp.add(1) = SP![-2].dup();
                    *sp.add(2) = SP![-1].dup();
                    SP_GO!(3);
                }
                OpCode::DUP1 => {
                    // a b ^ -> a a b ^
                    SP![0] = SP![-1];
                    SP![-1] = SP![-2].dup();
                    SP_GO!(1);
                }
                OpCode::INSERT2 => {
                    // obj a ^ -> a obj a ^ (dup_x1)
                    SP![0] = SP![-1];
                    SP![-1] = SP![-2];
                    SP![-2] = SP![0].dup();
                    SP_GO!(1);
                }
                OpCode::INSERT3 => {
                    /* obj prop a ^ -> a obj prop a ^ (dup_x2) */
                    SP![0] = SP![-1];
                    SP![-1] = SP![-2];
                    SP![-2] = SP![-3];
                    SP![-3] = SP![0].dup();
                    SP_GO!(1);
                }
                OpCode::INSERT4 => {
                    /* this obj prop a -> a this obj prop a */
                    SP![0] = SP![-1];
                    SP![-1] = SP![-2];
                    SP![-2] = SP![-3];
                    SP![-3] = SP![-4];
                    SP![-4] = SP![0].dup();
                    SP_GO!(1);
                }
                OpCode::PERM3 => {
                    // obj a b ^ -> a obj b ^
                    let tmp = SP![-2];
                    SP![-2] = SP![-3];
                    SP![-3] = tmp;
                }
                OpCode::ROT3L => {
                    // rotary shift left on 3 oprands
                    // x a b ^ -> a b x ^
                    let tmp = SP![-3];
                    SP![-3] = SP![-2];
                    SP![-2] = SP![-1];
                    SP![0] = tmp;
                }
                OpCode::ROT4L => {
                    // rotary shift left on 4 oprands
                    // x a b c ^ -> a b c x ^
                    let tmp = SP![-4];
                    SP![-4] = SP![-3];
                    SP![-3] = SP![-2];
                    SP![-2] = SP![-1];
                    SP![-1] = tmp;
                }
                OpCode::ROT5L => {
                    let tmp = SP![-5];
                    SP![-5] = SP![-4];
                    SP![-4] = SP![-3];
                    SP![-3] = SP![-2];
                    SP![-2] = SP![-1];
                    SP![-1] = tmp;
                }
                OpCode::ROT3R => {
                    // rotary shift right on 3 oprands
                    // a b x ^ -> x a b
                    let tmp = SP![-1];
                    SP![-1] = SP![-2];
                    SP![-2] = SP![-1];
                    SP![-3] = tmp;
                }
                OpCode::PERM4 => {
                    // rotary shift right on 3 opeands that before the last operand.
                    // obj prop a b ^ -> a obj prop b ^
                    let tmp = SP![-2];
                    SP![-2] = SP![-3];
                    SP![-3] = SP![-4];
                    SP![-4] = tmp;
                }
                OpCode::PERM5 => {
                    // this obj prop a b ^ -> a this obj prop b ^
                    let tmp = SP![-2];
                    SP![-2] = SP![-3];
                    SP![-3] = SP![-4];
                    SP![-4] = SP![-5];
                    SP![-5] = tmp;
                }
                OpCode::SWAP => {
                    // a b ^ -> b a ^
                    let tmp = SP![-1];
                    SP![-2] = SP![-1];
                    SP![-1] = tmp;
                }
                OpCode::SWAP2 => {
                    // a b c d ^ -> c d a b ^
                    let tmp1 = SP![-4];
                    let tmp2 = SP![-3];
                    SP![-4] = SP![-2];
                    SP![-3] = SP![-1];
                    SP![-2] = tmp1;
                    SP![-1] = tmp2;
                }
                OpCode::FCLOSURE => {
                    let id = get_u32(pc);
                    PC_GO!(4);
                    let bfunc = CPOOL![id].dup();
                    SP![0] = js_closure(ctx, bfunc, var_refs, sf);
                    if SP![0].is_exception() {
                        goto = Some(GotoLabel::Exception);
                    }
                    SP_GO!(1);
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
                        goto = Some(GotoLabel::Exception);
                    } else if opcode == OpCode::TAIL_CALL {
                        goto = Some(GotoLabel::Done(ret_val, false));
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
                        goto = Some(GotoLabel::Exception);
                    } else {
                        for i in -2..call_argc {
                            JS_FreeValue(ctx, *call_argv.offset(i as isize));
                        }
                        sp = sp.offset(-call_argc as isize - 2);
                        SP![0] = ret_val;
                        SP_GO!(1);
                    }
                }
                OpCode::CALL_METHOD | OpCode::TAIL_CALL_METHOD => {
                    let call_argc = get_u16(pc) as i32;
                    PC_GO!(2);
                    debug_assert!(call_argc >= 0);

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
                        goto = Some(GotoLabel::Exception);
                    } else if opcode == OpCode::TAIL_CALL_METHOD {
                        goto = Some(GotoLabel::Done(ret_val, false));
                    } else {
                        for i in -2..call_argc {
                            JS_FreeValue(ctx, *call_argv.offset(i as isize));
                        }
                        sp = sp.offset(-call_argc as isize - 2);
                        SP![0] = ret_val;
                        SP_GO!(1);
                    }
                }
                OpCode::ARRAY_FROM => {
                    let call_argc = get_u16(pc) as i32;
                    PC_GO!(2);

                    let ret_val = JS_NewArray(ctx);
                    if ret_val.is_exception() {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        let call_argv = sp.offset(-call_argc as isize);
                        for i in 0..call_argc {
                            let ret = JS_DefinePropertyValue(
                                ctx,
                                ret_val,
                                JSAtom::from_u32(i as u32),
                                *call_argv.offset(i as isize),
                                JS_PROP_C_W_E | JS_PROP_THROW,
                            );
                            *call_argv.offset(i as isize) = JSValue::UNDEFINED;
                            if ret < 0 {
                                JS_FreeValue(ctx, ret_val);
                                goto = Some(GotoLabel::Exception);
                                break;
                            }
                        }

                        if goto.is_none() {
                            sp = sp.offset(-call_argc as isize);
                            SP![0] = ret_val;
                            SP_GO!(1);
                        }
                    }
                }
                OpCode::APPLY => {
                    let magic = get_u16(pc) as i32;
                    pc = pc.add(2);

                    let ret_val = js_function_apply(ctx, SP![-3], 2, sp.offset(-2), magic);
                    if ret_val.is_exception() {
                        goto = Some(GotoLabel::Exception);
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
                    goto = Some(GotoLabel::Done(*sp, false))
                }
                OpCode::RETURN_UNDEF => {
                    goto = Some(GotoLabel::Done(JSValue::UNDEFINED, false));
                }
                OpCode::CHECK_CTOR_RETURN => {
                    /* return TRUE if 'this' should be returned */
                    if !JS_IsObject(SP![-1]) {
                        if !SP![-1].is_undefined() {
                            caller_ctx.JS_ThrowTypeError(
                                c"derived class constructor must return an object or undefined",
                            );
                            goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::CHECK_BRAND => {
                    if let Ok(b) = JS_CheckBrand(ctx, SP![-2], SP![-1]) {
                        if !b {
                            ctx.JS_ThrowTypeError(c"invalid brand on object");
                            goto = Some(GotoLabel::Exception);
                        }
                    } else {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::ADD_BRAND => {
                    if JS_AddBrand(ctx, SP![-2], SP![-1]) < 0 {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        JS_FreeValue(ctx, SP![-2]);
                        JS_FreeValue(ctx, SP![-1]);
                        sp = sp.offset(-2);
                    }
                }
                OpCode::THROW => {
                    SP_GO!(-1);
                    ctx.JS_Throw(SP![0]);
                    goto = Some(GotoLabel::Exception);
                }
                OpCode::THROW_ERROR => {
                    let atom = JSAtom(get_u32(pc));
                    let type_ = *pc.offset(4);
                    PC_GO!(5);

                    match type_ {
                        JS_THROW_VAR_RO => {
                            JS_ThrowTypeErrorReadOnly(ctx, JS_PROP_THROW, atom);
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
                    goto = Some(GotoLabel::Exception);
                }
                OpCode::EVAL => {
                    let call_argc = get_u16(pc) as i32;
                    // scope_idx will be -1 for top of scope chain
                    let scope_idx = get_u16(pc.offset(2)) as i32 - 1;
                    PC_GO!(4);

                    let call_argv = sp.offset(-call_argc as isize);
                    sf.cur_pc_offset = pc.offset_from(pc_base) as _;

                    let ret_val = if js_same_value(ctx, *call_argv.offset(-1), ctx.eval_obj) {
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
                        goto = Some(GotoLabel::Exception);
                    } else {
                        for i in -1..call_argc {
                            JS_FreeValue(ctx, *call_argv.offset(i as isize));
                        }
                        sp = sp.offset(-call_argc as isize - 1);
                        SP![0] = ret_val;
                        sp = sp.offset(1);
                    }
                }
                OpCode::APPLY_EVAL => {
                    let scope_idx = get_u16(pc) - 1;
                    PC_GO!(2);

                    let mut len = 0;
                    let tab: *mut JSValue = build_arg_list(ctx, &mut len, SP![-1]);
                    if tab.is_null() {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        let ret_val = if js_same_value(ctx, SP![-2], ctx.eval_obj) {
                            JS_EvalObject(
                                ctx,
                                JSValue::UNDEFINED,
                                if len > 0 { *tab } else { JSValue::UNDEFINED },
                                JS_EVAL_TYPE_DIRECT,
                                scope_idx as _,
                            )
                        } else {
                            JS_Call(ctx, SP![-2], JSValue::UNDEFINED, len as _, tab)
                        };

                        free_arg_list(ctx, tab, len);
                        if ret_val.is_exception() {
                            goto = Some(GotoLabel::Exception);
                        } else {
                            JS_FreeValue(ctx, SP![-2]);
                            JS_FreeValue(ctx, SP![-1]);
                            sp = sp.offset(-2);
                            SP![0] = ret_val;
                            sp = sp.offset(1);
                        }
                    }
                }
                OpCode::REGEXP => {
                    SP![-2] =
                        js_regexp_constructor_internal(ctx, JSValue::UNDEFINED, SP![-2], SP![-1]);
                    SP_GO!(-1);
                }
                OpCode::GET_SUPER => {
                    let proto = JS_GetPrototype(ctx, SP![-1]);
                    if proto.is_exception() {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        JS_FreeValue(ctx, SP![-1]);
                        SP![-1] = proto;
                    }
                }
                OpCode::IMPORT => {
                    let val = js_dynamic_import(ctx, SP![-1]);
                    if val.is_exception() {
                        goto = Some(GotoLabel::Exception);
                        JS_FreeValue(ctx, SP![-1]);
                        SP![-1] = val;
                    }
                }
                OpCode::CHECK_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    let ret = JS_CheckGlobalVar(ctx, atom);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP![0] = JS_NewBool(ctx, ret != 0);
                        SP_GO!(1);
                    }
                }
                OpCode::GET_VAR_UNDEF | OpCode::GET_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    let val = JS_GetGlobalVar(ctx, atom, opcode == OpCode::GET_VAR);
                    if val.is_exception() {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP![0] = val;
                        sp = sp.offset(1);
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
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::PUT_VAR_STRICT => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    // sp[-2] is TRUE or FALSE
                    if JS_VALUE_GET_INT(SP![-2]) == 0 {
                        JS_ThrowReferenceErrorNotDefined(ctx, atom);
                        goto = Some(GotoLabel::Exception);
                    }

                    let ret = JS_SetGlobalVar(ctx, atom, SP![-1], 2);
                    sp = sp.offset(-2);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::CHECK_DEFINE_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    let flags = *pc.offset(4);
                    PC_GO!(5);
                    if JS_CheckDefineGlobalVar(ctx, atom, flags as _) != 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::DEFINE_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    let flags = *pc.offset(4);
                    PC_GO!(5);
                    if JS_DefineGlobalVar(ctx, atom, flags as _) != 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::DEFINE_FUNC => {
                    let atom = JSAtom(get_u32(pc));
                    let flags = *pc.offset(4);
                    PC_GO!(5);
                    if JS_DefineGlobalFunction(ctx, atom, SP![-1], flags as _) != 0 {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        JS_FreeValue(ctx, SP![-1]);
                        SP_GO!(-1);
                    }
                }
                OpCode::GET_LOC => {
                    let idx = get_u16(pc);
                    PC_GO!(2);
                    SP![0] = VAR![idx].dup();
                    sp = sp.offset(1);
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
                OpCode::GET_VAR_REF_CHECK
                | OpCode::PUT_VAR_REF_CHECK
                | OpCode::PUT_VAR_REF_CHECK_INIT => {
                    let idx = get_u16(pc);
                    PC_GO!(2);

                    let val = var_refs[idx as usize].as_ref().pvalue;
                    if JS_IsUninitialized(*val) {
                        JS_ThrowReferenceErrorUninitialized2(ctx, b, idx, true);
                        goto = Some(GotoLabel::Exception);
                    } else {
                        if opcode == OpCode::GET_VAR_REF_CHECK {
                            // GET_VAR_REF_CHECK
                            SP![0] = (*val).dup();
                            sp = sp.offset(1);
                        } else {
                            // PUT_VAR_REF_CHECK, PUT_VAR_REF_CHECK_INIT
                            set_value(ctx, val, SP![-1]);
                            SP_GO!(-1);
                        }
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
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP![0] = val.dup();
                        sp = sp.offset(1);
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
                        goto = Some(GotoLabel::Exception);
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
                    pc = pc.offset(6);

                    SP![0] = JS_NewObjectProto(ctx, JSValue::NULL);
                    sp = sp.offset(1);

                    if SP![-1].is_exception() {
                        goto = Some(GotoLabel::Exception);
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
                                if let Ok(pr) = (*JS_VALUE_GET_OBJ(SP![-1])).add_property(
                                    ctx,
                                    atom,
                                    JS_PROP_WRITABLE | JS_PROP_VARREF,
                                ) {
                                    (*pr).u.var_ref = vr.as_ptr();
                                    SP![0] = atom.to_value(ctx, false);
                                    SP_GO!(1);
                                } else {
                                    if let Some(vr) = Box::from_raw(vr.as_ptr()).free(rt) {
                                        Box::leak(vr); // var ref is still alive, don't drop
                                    }
                                    goto = Some(GotoLabel::Exception);
                                };
                            }
                            Err(_) => {
                                goto = Some(GotoLabel::Exception);
                            }
                        }
                    }
                }
                OpCode::MAKE_VAR_REF => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    if JS_GetGlobalVarRef(ctx, atom, sp) != 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                    sp = sp.offset(2);
                }
                OpCode::GOTO => {
                    let n = get_u32(pc) as i32; // plus or minus
                    PC_GO!(n);

                    if js_poll_interrupts(ctx) != 0 {
                        goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
                    }
                }

                #[cfg(feature = "short_opcode")]
                OpCode::IF_TRUE8 | OpCode::IF_FALSE8 => {
                    todo!()
                }

                OpCode::CATCH => {
                    let diff = get_u32(pc) as i32;
                    SP![0] = JS_NewCatchOffset(ctx, pc.offset_from(pc_base) as i32 + diff);
                    sp = sp.offset(1);
                    PC_GO!(4);
                }
                OpCode::GOSUB => {
                    let diff = get_u32(pc) as i32;
                    /* XXX: should have a different tag to avoid security flaw */
                    SP![0] = JSValue::from_i32(pc.offset_from(pc_base) as i32 + 4);
                    pc = pc.offset(diff as isize);
                }
                OpCode::RET => {
                    let op1 = SP![-1];
                    if JS_VALUE_GET_TAG(op1) != JS_TAG_INT {
                        ctx.JS_ThrowInternalError(c"invalid ret value");
                        goto = Some(GotoLabel::Exception);
                    } else {
                        let pos = JS_VALUE_GET_INT(op1);
                        if pos >= bytecodes.len() as i32 {
                            ctx.JS_ThrowInternalError(c"invalid ret value");
                            goto = Some(GotoLabel::Exception);
                        } else {
                            SP_GO!(-1);
                            pc = pc_base.offset(pos as isize);
                        }
                    }
                }
                OpCode::FOR_IN_START => {
                    if js_for_in_start(ctx, sp) != 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::FOR_IN_NEXT => {
                    if js_for_in_next(ctx, sp) != 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                    SP_GO!(2);
                }
                OpCode::FOR_OF_START => {
                    if js_for_of_start(ctx, sp, false) != 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                    SP_GO!(1);
                    SP![0] = JS_NewCatchOffset(ctx, 0);
                    SP_GO!(1);
                }
                OpCode::FOR_OF_NEXT => {
                    let offset = -3 - *pc as i32;
                    PC_GO!(1);
                    if js_for_of_next(ctx, sp, offset) != 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                    SP_GO!(2);
                }
                OpCode::FOR_AWAIT_OF_START => {
                    if js_for_of_start(ctx, sp, true) != 0 {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP_GO!(1);
                        SP![0] = JS_NewCatchOffset(ctx, 0);
                        SP_GO!(1);
                    }
                }
                OpCode::ITERATOR_GET_VALUE_DONE => {
                    if js_iterator_get_value_done(ctx, sp) != 0 {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP_GO!(1);
                    }
                }
                OpCode::ITERATOR_CHECK_OBJECT => {
                    if !JS_IsObject(SP![-1]) {
                        ctx.JS_ThrowTypeError(c"iterator must return an object");
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::ITERATOR_CLOSE => {
                    /* iter_obj next catch_offset -> */
                    SP_GO!(-1); /* drop the catch offset to avoid getting caught by exception */
                    JS_FreeValue(ctx, SP![-1]); /* drop the next method */
                    SP_GO!(-1);
                    if !SP![-1].is_undefined() {
                        if JS_IteratorClose(ctx, SP![-1], false).is_err() {
                            goto = Some(GotoLabel::Exception);
                        } else {
                            JS_FreeValue(ctx, SP![-1]);
                        }
                    }
                    SP_GO!(-1);
                }
                OpCode::NIP_CATCH => {
                    /* catch_offset ... ret_val -> ret_eval */
                    SP_GO!(-1);
                    let ret_val = *sp;
                    while sp > sp_base && JS_VALUE_GET_TAG(SP![-1]) != JS_TAG_CATCH_OFFSET {
                        SP_GO!(-1);
                        JS_FreeValue(ctx, *sp);
                    }
                    if sp == sp_base {
                        ctx.JS_ThrowInternalError(c"nip_catch");
                        JS_FreeValue(ctx, ret_val);
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP![-1] = ret_val;
                    }
                }
                OpCode::ITERATOR_NEXT => {
                    /* stack: iter_obj next catch_offset val */
                    let ret = JS_Call(ctx, SP![-3], SP![-4], 1, sp.offset(-1));
                    if ret.is_exception() {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        JS_FreeValue(ctx, SP![-1]);
                        SP![-1] = ret;
                    }
                }
                OpCode::ITERATOR_CALL => {
                    let flags = *pc as i32;
                    PC_GO!(1);
                    let method = JS_GetProperty(
                        ctx,
                        SP![-4],
                        if flags & 1 != 0 {
                            JS_ATOM_throw
                        } else {
                            JS_ATOM_return
                        },
                    );
                    if method.is_exception() {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        let ret_flag = if method.is_undefined() || method.is_null() {
                            true
                        } else {
                            let ret = if flags & 2 != 0 {
                                // no argument
                                JS_CallFree(ctx, method, SP![-4], 0, std::ptr::null_mut())
                            } else {
                                JS_CallFree(ctx, method, SP![-4], 1, sp.offset(-1))
                            };
                            if ret.is_exception() {
                                goto = Some(GotoLabel::Exception);
                            } else {
                                JS_FreeValue(ctx, SP![-1]);
                                SP![-1] = ret;
                            }
                            false
                        };

                        if goto.is_none() {
                            SP![0] = JS_NewBool(ctx, ret_flag);
                            SP_GO!(1);
                        }
                    }
                }
                OpCode::LNOT => {
                    let op1 = SP![-1];
                    let res = if JS_VALUE_GET_TAG(op1) <= JS_TAG_UNDEFINED {
                        JS_VALUE_GET_INT(op1) != 0
                    } else {
                        JS_ToBoolFree(ctx, op1).unwrap_or(true)
                    };
                    SP![-1] = JS_NewBool(ctx, !res);
                }
                OpCode::GET_FIELD | OpCode::GET_FIELD2 => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    let val = JS_GetProperty(ctx, SP![-1], atom);
                    if val.is_exception() {
                        goto = Some(GotoLabel::Exception);
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
                    let ret = JS_SetPropertyInternal(
                        ctx,
                        SP![-2],
                        atom,
                        SP![-1],
                        SP![-2],
                        JS_PROP_THROW_STRICT,
                    );
                    JS_FreeValue(ctx, SP![-2]);
                    sp = sp.offset(-2);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::PRIVATE_SYMBOL => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    let val = JS_NewSymbolFromAtom(ctx, atom, JSAtomTypeHint::Private);
                    if val.is_exception() {
                        goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::PUT_PRIVATE_FIELD => {
                    let ret = JS_SetPrivateField(ctx, SP![-3], SP![-1], SP![-2]);
                    JS_FreeValue(ctx, SP![-3]);
                    JS_FreeValue(ctx, SP![-1]);
                    SP_GO!(-3);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::DEFINE_PRIVATE_FIELD => {
                    let ret = JS_DefinePrivateField(ctx, SP![-3], SP![-2], SP![-1]);
                    JS_FreeValue(ctx, SP![-2]);
                    sp = sp.offset(-2);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::DEFINE_FIELD => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    let ret = JS_DefinePropertyValue(
                        ctx,
                        SP![-2],
                        atom,
                        SP![-1],
                        JS_PROP_C_W_E | JS_PROP_THROW,
                    );
                    SP_GO!(-1);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::SET_NAME => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);
                    let ret = JS_DefineObjectName(ctx, SP![-1], atom, JS_PROP_CONFIGURABLE);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::SET_PROTO => {
                    let proto = SP![-1];
                    if JS_IsObject(proto) || proto.is_null() {
                        if JS_SetPrototypeInternal(ctx, SP![-2], proto, true).is_err() {
                            goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
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
                            ret = JS_DefineProperty(ctx, obj, atom, value, getter, setter, flags);
                        }
                        JS_FreeValue(ctx, SP![-1]);
                        if is_computed {
                            JS_FreeAtom(ctx, atom);
                            JS_FreeValue(ctx, SP![-2]);
                        }
                        SP_GO!(if is_computed { -2 } else { -1 });
                        if ret < 0 {
                            goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::GET_ARRAY_EL | OpCode::GET_ARRAY_EL2 => {
                    let val = JS_GetPropertyValue(ctx, SP![-2], SP![-1]);
                    if opcode == OpCode::GET_ARRAY_EL {
                        JS_FreeValue(ctx, SP![-2]);
                        SP![-2] = val;
                        SP_GO!(-1);
                    } else {
                        SP![-1] = val;
                    }
                    if val.is_exception() {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::GET_REF_VALUE => {
                    if SP![-2].is_undefined() {
                        let atom = JS_ValueToAtom(ctx, SP![-1]);
                        if atom != JS_ATOM_NULL {
                            JS_ThrowReferenceErrorNotDefined(ctx, atom);
                            JS_FreeAtom(ctx, atom);
                        }
                        goto = Some(GotoLabel::Exception);
                    } else {
                        let val = JS_GetPropertyValue(ctx, SP![-2], SP![-1].dup());
                        if val.is_exception() {
                            goto = Some(GotoLabel::Exception);
                        } else {
                            SP![0] = val;
                            SP_GO!(1);
                        }
                    }
                }
                OpCode::PUT_ARRAY_EL => {
                    let ret =
                        JS_SetPropertyValue(ctx, SP![-3], SP![-2], SP![-1], JS_PROP_THROW_STRICT);
                    JS_FreeValue(ctx, SP![-3]);
                    SP_GO!(-3);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
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
                            goto = Some(GotoLabel::Exception);
                        } else {
                            if is_strict_mode(ctx) {
                                flags |= JS_PROP_NO_ADD;
                            }
                        }
                    }

                    if goto.is_none() {
                        let ret = JS_SetPropertyValue(ctx, SP![-3], SP![-2], SP![-1], flags);
                        JS_FreeValue(ctx, SP![-3]);
                        SP_GO!(-3);
                        if ret < 0 {
                            goto = Some(GotoLabel::Exception);
                        }
                    }
                }
                OpCode::PUT_SUPER_VALUE => {
                    if JS_VALUE_GET_TAG(SP![-3]) != JS_TAG_OBJECT {
                        ctx.JS_ThrowTypeErrorNotAnObject();
                        goto = Some(GotoLabel::Exception);
                    }
                    let atom = JS_ValueToAtom(ctx, SP![-2]);
                    if atom == JS_ATOM_NULL {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        let ret = JS_SetPropertyInternal(
                            ctx,
                            SP![-3],
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
                        if ret < 0 {
                            goto = Some(GotoLabel::Exception);
                        }
                    }
                }
                OpCode::DEFINE_ARRAY_EL => {
                    let ret = JS_DefinePropertyValueValue(
                        ctx,
                        SP![-3],
                        SP![-2].dup(),
                        SP![-1],
                        JS_PROP_C_W_E | JS_PROP_THROW,
                    );
                    SP_GO!(-1);
                    if ret < 0 {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::APPEND => {
                    if js_append_enumerate(ctx, sp) != 0 {
                        goto = Some(GotoLabel::Exception);
                    } else {
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

                    if JS_CopyDataProperties(
                        ctx,
                        SP![-1 - (mask & 3)],
                        SP![-1 - ((mask >> 2) & 7)],
                        SP![-1 - ((mask >> 5) & 7)],
                        false,
                    )
                    .is_err()
                    {
                        goto = Some(GotoLabel::Exception);
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
                        // fail
                        SP![-1] = JSValue::UNDEFINED; // original value was freed
                        SP![-2] = JSValue::UNDEFINED; // original value was freed
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::ADD_LOC => {
                    let idx = *pc as i32;
                    PC_GO!(1);

                    let op1 = SP![-1];
                    SP_GO!(-1);

                    let pv = VAR_PTR![idx];

                    // In case of exception, following expression frees *pv and op1,
                    // so we must duplicate *pv in advance
                    if let Ok(val) = BinaryOp::new_slow(ctx, (*pv).dup(), op1, 3, Some(HINT_NONE))
                        .and_then(|bop| bop.add(ctx))
                    {
                        set_value(ctx, pv, val);
                    } else {
                        goto = Some(GotoLabel::Exception);
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
                            goto = Some(GotoLabel::Exception);
                        }
                    }
                }
                OpCode::POST_INC | OpCode::POST_DEC => {
                    let op1 = SP![-1];
                    let n1 = JS_ToNumberFree(ctx, op1);
                    if n1.is_exception() {
                        SP![-1] = JSValue::UNDEFINED;
                        goto = Some(GotoLabel::Exception);
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
                            goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::NOT => {
                    if let Ok(res) = UnaryOp::new_int(ctx, SP![-1].dup()).map(|unary| unary.not()) {
                        SP![-1] = res;
                    } else {
                        SP![-1] = JSValue::UNDEFINED;
                        goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
                    }
                }

                OpCode::LT | OpCode::LTE | OpCode::GT | OpCode::GTE => {
                    if let Ok(b) = relational(ctx, SP![-2], SP![-1], opcode) {
                        SP![-2] = JS_NewBool(ctx, b);
                        SP_GO!(-1);
                    } else {
                        goto = Some(GotoLabel::Exception);
                    }
                }
                OpCode::EQ | OpCode::NEQ => {
                    if let Ok(b) = js_eq(ctx, sp, opcode == OpCode::NEQ) {
                        SP![-2] = JS_NewBool(ctx, b);
                        SP_GO!(-1);
                    } else {
                        goto = Some(GotoLabel::Exception)
                    }
                }
                OpCode::STRICT_EQ | OpCode::STRICT_NEQ => {
                    let mut b = js_strict_eq(ctx, SP![-2], SP![-1]);
                    if opcode == OpCode::STRICT_NEQ {
                        b = !b;
                    }
                    SP![-2] = JS_NewBool(ctx, b);
                    SP_GO!(-1);
                }

                OpCode::IN => {
                    if js_operator_in(ctx, sp).is_err() {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP_GO!(-1);
                    }
                }
                OpCode::PRIVATE_IN => {
                    if js_operator_private_in(ctx, sp).is_err() {
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP_GO!(-1);
                    }
                }
                OpCode::INSTANCEOF => {
                    if js_operator_instanceof(ctx, sp).is_err() {
                        goto = Some(GotoLabel::Exception);
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
                        goto = Some(GotoLabel::Exception);
                    } else {
                        SP_GO!(-1);
                    }
                }
                OpCode::DELETE_VAR => {
                    let atom = JSAtom(get_u32(pc));
                    PC_GO!(4);

                    match JS_DeleteProperty(ctx, ctx.global_obj, atom, 0) {
                        Ok(ret) => {
                            SP![0] = JS_NewBool(ctx, ret);
                            SP_GO!(1);
                        }
                        Err(_) => {
                            goto = Some(GotoLabel::Exception);
                        }
                    }
                }
                OpCode::TO_PROPKEY => match JS_VALUE_GET_TAG(SP![-1]) {
                    JS_TAG_INT | JS_TAG_STRING | JS_TAG_SYMBOL => {}
                    _ => {
                        let ret_val = JS_ToPropertyKey(ctx, SP![-1]);
                        if ret_val.is_exception() {
                            goto = Some(GotoLabel::Exception);
                        } else {
                            JS_FreeValue(ctx, SP![-1]);
                            SP![-1] = ret_val;
                        }
                    }
                },

                OpCode::TO_PROPKEY2 => {
                    // must be tested first
                    if SP![-2].is_undefined() || SP![-2].is_null() {
                        ctx.JS_ThrowTypeError(c"value has property");
                        goto = Some(GotoLabel::Exception);
                    } else {
                        match JS_VALUE_GET_TAG(SP![-1]) {
                            JS_TAG_INT | JS_TAG_STRING | JS_TAG_SYMBOL => {}
                            _ => {
                                let ret_val = JS_ToPropertyKey(ctx, SP![-1]);
                                if ret_val.is_exception() {
                                    goto = Some(GotoLabel::Exception);
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
                    if let Ok(ret) = JS_HasProperty(ctx, obj, atom) {
                        if ret {
                            let mut skip = false;
                            if is_with != 0 {
                                let r = js_has_unscopable(ctx, obj, atom);
                                if r < 0 {
                                    goto = Some(GotoLabel::Exception);
                                    skip = true;
                                } else if r != 0 {
                                    // no with
                                    /* if not jumping, drop the object argument */
                                    JS_FreeValue(ctx, SP![-1]);
                                    SP_GO!(-1);
                                    skip = true;
                                }
                            }

                            if !skip {
                                match opcode {
                                    OpCode::WITH_GET_VAR => {
                                        let val = JS_GetProperty(ctx, obj, atom);
                                        if val.is_exception() {
                                            goto = Some(GotoLabel::Exception);
                                        } else {
                                            set_value(ctx, sp.offset(-1), val);
                                        }
                                    }
                                    OpCode::WITH_PUT_VAR => {
                                        // XXX: check if strict mode
                                        let ret = JS_SetPropertyInternal(
                                            ctx,
                                            obj,
                                            atom,
                                            SP![-2],
                                            obj,
                                            JS_PROP_THROW_STRICT,
                                        );
                                        JS_FreeValue(ctx, SP![-1]);
                                        SP_GO!(-2);
                                        if ret < 0 {
                                            goto = Some(GotoLabel::Exception);
                                        }
                                    }
                                    OpCode::WITH_DELETE_VAR => {
                                        if let Ok(ret) = JS_DeleteProperty(ctx, obj, atom, 0) {
                                            JS_FreeValue(ctx, SP![-1]);
                                            SP![-1] = JS_NewBool(ctx, ret);
                                        } else {
                                            goto = Some(GotoLabel::Exception);
                                        }
                                    }
                                    OpCode::WITH_MAKE_REF => {
                                        // produce a pair object/propname on the stack
                                        SP![0] = atom.to_value(ctx, false);
                                        SP_GO!(1);
                                    }
                                    OpCode::WITH_GET_REF => {
                                        // produce a pair object/method on the stack
                                        let val = JS_GetProperty(ctx, obj, atom);
                                        if val.is_exception() {
                                            goto = Some(GotoLabel::Exception);
                                        } else {
                                            SP![0] = val;
                                            SP_GO!(1);
                                        }
                                    }
                                    OpCode::WITH_GET_REF_UNDEF => {
                                        /* produce a pair undefined/function on the stack */
                                        let val = JS_GetProperty(ctx, obj, atom);
                                        if val.is_exception() {
                                            goto = Some(GotoLabel::Exception);
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
                            }
                        } else {
                            // no with
                            /* if not jumping, drop the object argument */
                            JS_FreeValue(ctx, SP![-1]);
                            SP_GO!(-1);
                        }
                    } else {
                        goto = Some(GotoLabel::Exception);
                    }
                }

                OpCode::AWAIT => {
                    let ret_val = JSValue::from_i32(FUNC_RET_AWAIT);
                    goto = Some(GotoLabel::Done(ret_val, true));
                }
                OpCode::YIELD => {
                    let ret_val = JSValue::from_i32(FUNC_RET_YIELD);
                    goto = Some(GotoLabel::Done(ret_val, true));
                }
                OpCode::YIELD_STAR | OpCode::ASYNC_YIELD_STAR => {
                    let ret_val = JSValue::from_i32(FUNC_RET_YIELD_STAR);
                    goto = Some(GotoLabel::Done(ret_val, true));
                }
                OpCode::RETURN_ASYNC => {
                    goto = Some(GotoLabel::Done(JSValue::UNDEFINED, true));
                }
                OpCode::INITIAL_YIELD => {
                    goto = Some(GotoLabel::Done(
                        JSValue::from_i32(FUNC_RET_INITIAL_YIELD),
                        true,
                    ));
                }

                OpCode::NOP => {}

                OpCode::IS_UNDEFINED_OR_NULL => {
                    if matches!(JS_VALUE_GET_TAG(SP![-1]), JS_TAG_UNDEFINED | JS_TAG_NULL) {
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
                    goto = Some(GotoLabel::Exception);
                }
            } // match opcode

            if goto.is_some() {
                // exit bytecode loop
                // will run label dispatch block above this loop
                break;
            }
        } // bytecode loop
    } // label dispatch loop
}
