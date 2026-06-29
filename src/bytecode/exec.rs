// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cmp;

use std::ptr::NonNull;

use fallible_collections::TryCollect;

use crate::{
    Array, JS_CLASS_OBJECT,
    array::alloc_fast_array_slots,
    atom::{Atom, AtomKind},
    builtins::{
        generator::{FUNC_RET_AWAIT, FUNC_RET_INITIAL_YIELD, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR},
        iterator::ObjectIter,
    },
    bytecode::{
        OpCode, SpecialObject,
        arithmatic::{UnaryOp, op_binary_logic},
        operators::{
            js_append_enumerate, js_for_in_next, js_for_in_start, js_for_of_next,
            js_has_unscopable, js_iterator_get_value_done, js_method_set_properties,
            js_op_define_class, js_operator_delete, js_operator_in, js_operator_instanceof,
        },
    },
    class::{
        OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD,
        OP_DEFINE_METHOD_SETTER,
    },
    context::{ContextPtr, ScopedContext},
    error::{
        Exception, JS_THROW_ERROR_DELETE_SUPER, JS_THROW_ERROR_ITERATOR_THROW, JS_THROW_VAR_REDECL,
        JS_THROW_VAR_RO, JS_THROW_VAR_UNINITIALIZED, JS_ThrowReferenceErrorNotDefined,
        JS_ThrowReferenceErrorUninitialized, JS_ThrowReferenceErrorUninitialized2,
        JS_ThrowSyntaxErrorVarRedeclaration,
    },
    eval::{EvalFlags, EvalType},
    function::{
        BCWrapper, BFuncPayload, Bytecode, CallContext, FunctionKind, JS_CALL_FLAG_CONSTRUCTOR,
        VarRef, build_arg_list, js_build_arguments, js_build_mapped_arguments, js_function_apply,
        js_function_set_properties,
    },
    module::{js_dynamic_import, js_import_meta},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
        JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD, JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_VARREF,
        JS_PROP_WRITABLE, JSProperty, LazyInit, Object, ObjectPayload,
    },
    stack_frame::{ExecMode, FrameIndex, StackFrame, StackFrameArena},
    utils::{read_from_ptr, unlikely},
    value::JSValue,
};

/// Action returned by the bytecode interpreter inner loop to the trampoline.
/// The inner loop processes opcodes normally; when it encounters a call,
/// return, tail call, or uncaught exception, it yields control to the
/// trampoline via this enum.
#[derive(Debug)]
enum ExecAction {
    /// Call a bytecode function: trampoline saves caller state and switches
    /// to the callee frame.
    CallBytecode {
        callee_bc: Bytecode,
        callee_frame_idx: FrameIndex,
        /// SP offset in caller frame where return value should be stored.
        return_sp: usize,
    },
    /// Cross-realm call: trampoline switches to callee's context scope.
    CallBytecodeCrossRealm {
        callee_bc: Bytecode,
        callee_frame_idx: FrameIndex,
        callee_ctx_ptr: ContextPtr,
        return_sp: usize,
    },
    /// Tail-call optimization: recycle current frame, switch to callee
    /// without pushing a VirtualFrame.
    TailCall {
        callee_bc: Bytecode,
        callee_func_obj: Object,
        callee_buf_cap: usize,
    },
    /// Frame returned normally or yielded (generator/async suspension).
    /// bool = true means the frame suspended (yield/await), not completed.
    Return(JSValue, bool),
    /// Uncaught exception in current frame; trampoline must propagate
    /// upward through the virtual call stack.
    Unwind,
}

impl Bytecode {
    /// Execute bytecodes. Return value is protected as local gc node.
    ///
    /// Switch of runtime's current stack frame:
    /// - during execution, current stack frame will be set to given `stack_frame`,
    /// - On exit, current stack frame will be restored back to the original one.
    /// - When exception raised (returns Err), `stack_frame` will be kept as current stack frame.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute(
        &self,
        caller_scope: &mut ScopedContext, // caller's scoped context
        func_home_obj: Option<Object>,
        stack_frame: &mut StackFrame, // The stack frame for execution
        this_obj: JSValue,            // The 'this' binding for the function execution
        new_target: JSValue,          // The new.target value for constructor calls
        pc_offset: usize,             // Current instruction pointer (program counter) offset
        sp_offset: usize,             // Current stack pointer offset
        captured_vars: &mut [VarRef], // Closure var refs
        args: &[JSValue], // actual calling arguments pass in by caller, not padded length
        throwed: bool,    // Indicates if an exception has been thrown
    ) -> JSValue {
        let mut realm_ctx = self.realm();

        // SAFETY: The return value (JSValue) is used immediately by the caller.
        // It is stored on the VM stack which is traced by GC marker.
        realm_ctx.with_new_scope(|cx| {
            // caller-scope must be at lower index than realm-scope in gc scope stack
            #[cfg(debug_assertions)]
            if cx.scope_stack_id() == caller_scope.scope_stack_id() {
                debug_assert!(
                    cx.depth() > caller_scope.depth(),
                    "stack_id={:?}, {}, {}",
                    cx.scope_stack_id(),
                    cx.depth(),
                    caller_scope.depth()
                );
            }

            self.execute_with_realm_scope(
                caller_scope,
                cx,
                func_home_obj,
                stack_frame,
                this_obj,
                new_target,
                pc_offset,
                sp_offset,
                captured_vars,
                args,
                throwed,
            )
        })
    }

    /// # Args:
    /// - caller_ctx: caller's scope must be kept untouched in this function. Consider using &ScopedContext instead?
    /// - realm_scope: a dedicated sub-scope under the bytecode's realm-context for this execution
    #[allow(clippy::too_many_arguments)]
    fn execute_with_realm_scope(
        &self,
        caller_ctx: &mut ScopedContext,
        realm_scope: &mut ScopedContext,
        func_home_obj: Option<Object>,
        stack_frame: &mut StackFrame,
        this_obj: JSValue,
        new_target: JSValue,
        pc_offset: usize,
        sp_offset: usize,
        captured_vars: &mut [VarRef],
        args: &[JSValue],
        throwed: bool,
    ) -> JSValue {
        let mut rt = realm_scope.rt_ptr();

        let bytecodes = self.opcodes();
        debug_assert!(
            pc_offset < bytecodes.len(),
            "pc_offset={}, bytecodes.len()={}",
            pc_offset,
            bytecodes.len()
        );

        let mode = stack_frame.mode;
        let cpools = self.consts();

        let sp_base = stack_frame.stack_base_ptr().as_ptr();
        let pc_base = bytecodes.as_ptr();
        let mut pc = unsafe { pc_base.add(pc_offset) };
        let mut sp = unsafe { sp_base.add(sp_offset) };

        stack_frame.cur_sp_offset = sp_offset as _;

        rt.push_stack_frame(NonNull::from_ref(stack_frame));

        if throwed {
            let cur_pc = unsafe { pc.offset_from(pc_base) as usize };

            if let Some((new_pc, new_sp)) = stack_frame.handle_exception(
                realm_scope,
                cur_pc,
                stack_frame.cur_sp_offset as usize,
            ) {
                stack_frame.cur_sp_offset = new_sp as i32;
                unsafe {
                    pc = pc_base.add(new_pc);
                    sp = sp_base.add(new_sp);
                }
            } else {
                stack_frame.handle_exit(
                    realm_scope,
                    cur_pc,
                    0,
                    self.func_kind() != FunctionKind::Normal,
                );
                return JSValue::EXCEPTION;
            }
        }

        macro_rules! SP_VAL {
            (0) => {
                unsafe { *sp }
            };
            ($idx:expr) => {
                unsafe { *sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_REF {
            (0) => {
                unsafe { sp.as_ref().unwrap() }
            };
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_ref().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_REF {
            (0) => {
                unsafe { &*sp }
            };
            ($idx:expr) => {
                unsafe { &*sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_MUT {
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_MUT {
            ($idx:expr) => {
                unsafe { &mut *sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_PUT {
            ($idx:expr, $val:expr) => {
                unsafe {
                    *sp.offset(($idx) as isize).as_mut().unwrap() = $val;
                }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_PUT {
            ($idx:expr, $val:expr) => {
                unsafe {
                    *sp.offset(($idx) as isize) = $val;
                }
            };
        }

        macro_rules! SP_GO {
            ($step:expr) => {
                sp = unsafe {
                    stack_frame.cur_sp_offset += ($step) as i32;
                    sp.offset(($step) as isize)
                }
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
            cpools.as_ref()[($idx) as usize]
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! CPOOL {
        [$idx:expr] => {
            unsafe {
                *cpools.as_ref().as_ptr().add(($idx) as usize)
            }
        };
    }

        enum State {
            Done(JSValue, bool),
            Exception,
        }

        #[cfg(all(debug_assertions, feature = "dump_bytecode"))]
        let mut debug_setting: Option<crate::function::BytecodeDebugSetting> = None;

        'restart: loop {
            let state: State = loop {
                #[cfg(all(debug_assertions, feature = "dump_bytecode"))]
                {
                    if unlikely(debug_setting.is_none()) {
                        let mut b = stack_frame
                            .func_obj
                            .payload()
                            .bc_function()
                            .and_then(|f| f.bytecode().debug.as_ref().map(|x| x.debug_setting))
                            .unwrap_or_default();

                        b.dump_run = 2;
                        debug_setting.replace(b);
                    }

                    let dr = debug_setting.map_or(0, |s| s.dump_run);
                    if unlikely(dr != 0) {
                        super::dump::dump_run_instruction(
                            realm_scope,
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
                        SP_PUT!(0, CPOOL![id]);
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
                            Atom::from_id(a)
                                .to_value(realm_scope)
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
                        let val: JSValue = if !mode.contains(ExecMode::STRICT) {
                            match this_obj {
                                JSValue::Object(_) => this_obj,
                                JSValue::NULL | JSValue::UNDEFINED => {
                                    JSValue::Object(realm_scope.global_obj())
                                }
                                _ => {
                                    if let Ok(v) = this_obj.objectify(realm_scope) {
                                        v.into()
                                    } else {
                                        break State::Exception;
                                    }
                                }
                            }
                        } else {
                            this_obj
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
                        let obj = realm_scope
                            .create_base_object()
                            .map_or(JSValue::EXCEPTION, JSValue::Object);
                        SP_PUT!(0, obj);
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
                                SP_PUT!(0, js_build_arguments(realm_scope, args));
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::MappedArguments => {
                                let ma = js_build_mapped_arguments(
                                    realm_scope,
                                    args,
                                    stack_frame,
                                    std::cmp::min(args.len() as i32, self.arg_count as i32),
                                )
                                .map_or(JSValue::EXCEPTION, JSValue::Object);
                                SP_PUT!(0, ma);
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::ThisFunc => {
                                SP_PUT!(0, JSValue::from(stack_frame.func_obj));
                                SP_GO!(1);
                            }
                            SpecialObject::NewTarget => {
                                SP_PUT!(0, new_target);
                                SP_GO!(1);
                            }
                            SpecialObject::HomeObject => {
                                SP_PUT!(
                                    0,
                                    if let Some(ho) = func_home_obj {
                                        JSValue::Object(ho)
                                    } else {
                                        JSValue::UNDEFINED
                                    }
                                );
                                SP_GO!(1);
                            }
                            SpecialObject::VarObject => {
                                SP_PUT!(
                                    0,
                                    realm_scope
                                        .create_base_object_with_proto(None)
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
                                    js_import_meta(realm_scope)
                                        .map_or(JSValue::EXCEPTION, JSValue::Object)
                                );
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                        }
                    }
                    OpCode::REST => {
                        let start = PC_FETCH!(u16);

                        if let Ok(arr) = args
                            .iter()
                            .skip(start as usize)
                            .copied()
                            .try_collect::<Vec<_>>()
                            .map_err(|_| {
                                realm_scope.throw_out_of_memory();
                                -1
                            })
                            .and_then(|vv| Array::from_values(realm_scope, vv))
                        {
                            SP_PUT!(0, JSValue::from(arr));
                            SP_GO!(1);
                        } else {
                            SP_PUT!(0, JSValue::EXCEPTION);
                            SP_GO!(1);
                            break State::Exception;
                        }
                    }
                    OpCode::DROP => {
                        SP_GO!(-1);
                    }
                    OpCode::NIP => {
                        // Stack operation: [a, b, ^] -> [b, ^] where ^ is stack top
                        // Removes the second value from top and moves top value down
                        SP_PUT!(-2, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::NIP1 => {
                        // Stack operation: [a, b, c, ^] -> [b, c, ^] where ^ is stack top
                        // Removes the third value from top and shifts remaining values down
                        SP_PUT!(-3, SP_VAL!(-2));
                        SP_PUT!(-2, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::DUP => {
                        // Stack operation: [a, ^] -> [a, a, ^] where ^ is stack top
                        // Duplicates the top value on the stack
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_GO!(1);
                    }
                    OpCode::DUP1 => {
                        // Stack operation: [a, b, ^] -> [a, a, b, ^]
                        // Duplicates the second value from top and places it below top
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_PUT!(-1, SP_VAL!(-2));
                        SP_GO!(1);
                    }
                    OpCode::DUP2 => {
                        // Stack operation: [a, b, ^] -> [a, b, a, b, ^] where ^ is stack top
                        // Duplicates the top two values on the stack
                        SP_PUT!(0, SP_VAL!(-2));
                        SP_PUT!(1, SP_VAL!(-1));
                        SP_GO!(2);
                    }
                    OpCode::DUP3 => {
                        // Stack operation: [a, b, c, ^] -> [a, b, c, a, b, c, ^] where ^ is stack top
                        // Duplicates the top three values on the stack
                        SP_PUT!(0, SP_VAL!(-3));
                        SP_PUT!(1, SP_VAL!(-2));
                        SP_PUT!(2, SP_VAL!(-1));
                        SP_GO!(3);
                    }
                    OpCode::INSERT2 => {
                        // Stack operation: [obj, a, ^] -> [a, obj, a, ^]
                        // Duplicates top value and inserts it under second position
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_PUT!(-1, SP_VAL!(-2));
                        SP_PUT!(-2, SP_VAL!(0));
                        SP_GO!(1);
                    }
                    OpCode::INSERT3 => {
                        // [obj, prop, a, ^] -> [a, obj, prop, a, ^]
                        // Duplicates top value and inserts it under third position
                        // Uses array rotation for efficient stack manipulation
                        unsafe {
                            std::slice::from_raw_parts_mut(sp.offset(-3), 4).rotate_right(1);
                        };

                        // After rotation, set SP![-3] to value in SP![0] duplicated
                        SP_PUT!(-3, SP_VAL!(0));
                        SP_GO!(1);
                    }
                    OpCode::INSERT4 => {
                        // [this, obj, prop, a, ^] -> [a, this, obj, prop, a, ^] where ^ is stack top
                        // Duplicates top value and inserts it under fourth position
                        // Uses array rotation for efficient stack manipulation
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 5) };
                        lst.rotate_right(1);

                        // After rotation, SP![0] contains the value to duplicate and insert
                        SP_PUT!(-4, SP_VAL!(0));

                        SP_GO!(1);
                    }
                    OpCode::ROT3L => {
                        // [x, a, b, ^] -> [a, b, x, ^] where ^ is stack top
                        // Performs a left rotation on top three values
                        // The leftmost value moves to the right, others shift left
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 3) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT4L => {
                        // [x, a, b, c, ^] -> [a, b, c, x, ^] where ^ is stack top
                        // Performs a left rotation on top four values
                        // The leftmost value moves to the right, others shift left
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 4) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT5L => {
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-5), 5) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT3R => {
                        // [a, b, x, ^] -> [x, a, b, ^] where ^ is stack top
                        // Performs a right rotation on top three values
                        // The rightmost value moves to the left, others shift right
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 3) };
                        lst.rotate_right(1);
                    }
                    OpCode::PERM3 => {
                        // [obj, a, fixed, ^] -> [a, obj, fixed, ^] where ^ is stack top
                        // Swaps the second and third values from top, keeping the fixed value in place
                        unsafe {
                            std::ptr::swap(sp.offset(-2), sp.offset(-3));
                        }
                    }
                    OpCode::PERM4 => {
                        // [obj, prop, a, fixed, ^] -> [a, obj, prop, fixed, ^]
                        // Performs a right rotation on three values before the fixed value
                        // The rightmost value moves left, others shift right, fixed value stays in place
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 3) };
                        lst.rotate_right(1);
                    }
                    OpCode::PERM5 => {
                        // [this, obj, prop, a, fixed, ^] -> [a, this, obj, prop, fixed, ^] where ^ is stack top
                        // Performs a right rotation on four values before the fixed value
                        // The rightmost value moves left, others shift right, fixed value stays in place
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-5), 4) };
                        lst.rotate_right(1);
                    }
                    OpCode::SWAP => {
                        // [a, b, ^] -> [b, a, ^] where ^ is stack top
                        // Swaps the top two values on the stack
                        unsafe {
                            std::ptr::swap(sp.offset(-1), sp.offset(-2));
                        }
                    }
                    OpCode::SWAP2 => {
                        // [a, b, c, d, ^] -> [c, d, a, b, ^] where ^ is stack top
                        // Swaps pairs of values: first with third, second with fourth
                        // Effectively exchanges the top two pairs of values
                        unsafe {
                            std::ptr::swap(sp.offset(-1), sp.offset(-3));
                            std::ptr::swap(sp.offset(-2), sp.offset(-4));
                        }
                    }
                    OpCode::FCLOSURE => {
                        // [^] -> [func_obj, ^]
                        let id = PC_FETCH!(u32);

                        if let JSValue::Bytecode(bc) = CPOOL![id] {
                            let r = bc
                                .make_closure(realm_scope, None, captured_vars, Some(stack_frame))
                                .map_or(JSValue::EXCEPTION, |f| f.into());
                            SP_PUT!(0, r);
                            SP_GO!(1);
                            if r.is_exception() {
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
                        let func = unsafe { *call_argv.offset(-1) };

                        let ret_val = func.call_internal(
                            realm_scope,
                            JSValue::UNDEFINED,
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if let Ok(r) = ret_val {
                            if unlikely(opcode == OpCode::TAIL_CALL) {
                                break State::Done(r, false);
                            } else {
                                // clear stack values: no need so
                                // for i in -1..call_argc as isize {
                                //     unsafe {
                                //         *call_argv.offset(i) = JSValue::UNDEFINED;
                                //     }
                                // }
                                SP_GO!(-(call_argc as i32 + 1));
                                SP_PUT!(0, r);
                                SP_GO!(1);
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::CALL_CONSTRUCTOR => {
                        // [func, new_target, arg1 .., argN, ^] -> [instance, ^]
                        // [                  |<-- argc -->|  ]
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        stack_frame.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        let func = unsafe { *call_argv.offset(-2) };
                        let ret_val = func.call_internal(
                            realm_scope,
                            JSValue::UNDEFINED,
                            unsafe { *call_argv.offset(-1) },
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            JS_CALL_FLAG_CONSTRUCTOR,
                        );

                        if let Ok(val) = ret_val {
                            // clear stack values: no need so
                            // for i in -2..call_argc {
                            //     unsafe {
                            //         *call_argv.offset(i as isize) = JSValue::UNDEFINED;
                            //     }
                            // }
                            SP_GO!(-(call_argc + 2));
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::CALL_METHOD | OpCode::TAIL_CALL_METHOD => {
                        // [this_obj, func, arg1, ..., argN, ^] -> [ret_val, ^]
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };

                        // save current pc
                        stack_frame.cur_pc_offset = unsafe { pc.offset_from(pc_base) as _ };

                        // could have recursive call.
                        let func = unsafe { *call_argv.offset(-1) };
                        let ret_val = func.call_internal(
                            realm_scope,
                            unsafe { *call_argv.offset(-2) },
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if let Ok(r) = ret_val {
                            if unlikely(opcode == OpCode::TAIL_CALL_METHOD) {
                                break State::Done(r, false);
                            } else {
                                // clear stack values: no need so
                                // for i in -2..call_argc as isize {
                                //     unsafe { *call_argv.offset(i) = JSValue::UNDEFINED };
                                // }

                                SP_GO!(-(call_argc + 2));
                                SP_PUT!(0, r);
                                SP_GO!(1);
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::ARRAY_FROM => {
                        // [el1, el2 ... elN, ^] -> [array, ^]
                        let call_argc = PC_FETCH!(u16) as i32;

                        if let Ok(mut list) = alloc_fast_array_slots(realm_scope, call_argc as _) {
                            // add elements
                            let call_argv = unsafe { sp.offset(-call_argc as isize) };
                            for i in 0..call_argc as usize {
                                let val = unsafe { *call_argv.add(i) };
                                list.push(val);
                            }

                            // create array with elements
                            if let Ok(arr) = crate::array::Array::from_values(realm_scope, list) {
                                SP_GO!(-call_argc);
                                SP_PUT!(0, JSValue::from(arr));
                                SP_GO!(1);
                            } else {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::APPLY => {
                        // Function.apply(arg1, arg2)
                        // [this_val, arg1, arg2, ^] -> [ret_val, ^]
                        let magic = PC_FETCH!(u16) as i32;

                        let op_3 = SP_VAL!(-3);
                        let op_2 = SP_VAL!(-2);
                        let op_1 = SP_VAL!(-1);
                        SP_GO!(-3);

                        if let Ok(res) = js_function_apply(
                            realm_scope,
                            CallContext::new(op_3, &[op_2, op_1], magic, None),
                        ) {
                            SP_PUT!(0, res);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::RETURN => {
                        SP_GO!(-1);
                        let ret = SP_VAL!(0);
                        break State::Done(ret, false);
                    }
                    OpCode::RETURN_UNDEF => {
                        break State::Done(JSValue::UNDEFINED, false);
                    }
                    OpCode::CHECK_CTOR_RETURN => {
                        // [this, ^] -> [this, bool, ^]
                        // put TRUE if 'this' should be returned
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
                        if new_target.is_undefined() {
                            realm_scope
                                .throw_type_error("class constructors must be invoked with 'new'");
                            break State::Exception;
                        }
                    }
                    OpCode::CHECK_BRAND => {
                        #[cfg(feature = "builtin_symbol")]
                        if let Ok(b) = SP_REF!(-2).check_brand(realm_scope, SP_VAL!(-1)) {
                            if unlikely(!b) {
                                realm_scope.throw_type_error("invalid brand on object");
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }

                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope.throw_internal_error("Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::ADD_BRAND => {
                        // [func_obj, home_obj, ^] -> [^]
                        #[cfg(feature = "builtin_symbol")]
                        {
                            let mut func_obj = SP_VAL!(-2);
                            let home_object = SP_VAL!(-1);
                            SP_GO!(-2);
                            if unlikely(func_obj.add_brand(realm_scope, home_object).is_err()) {
                                break State::Exception;
                            }
                        }

                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope
                                .throw_internal_error("can't add brand: Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::THROW => {
                        SP_GO!(-1);
                        realm_scope.throw_value(SP_VAL!(0));
                        break State::Exception;
                    }
                    OpCode::THROW_ERROR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let type_ = PC_FETCH!(u8);

                        match type_ {
                            JS_THROW_VAR_RO => {
                                realm_scope.throw_type_error_read_only(atom);
                            }
                            JS_THROW_VAR_REDECL => {
                                JS_ThrowSyntaxErrorVarRedeclaration(realm_scope, atom);
                            }
                            JS_THROW_VAR_UNINITIALIZED => {
                                JS_ThrowReferenceErrorUninitialized(realm_scope, atom);
                            }
                            JS_THROW_ERROR_DELETE_SUPER => {
                                realm_scope
                                    .throw_reference_error("unsupported reference to 'super'");
                            }
                            JS_THROW_ERROR_ITERATOR_THROW => {
                                realm_scope
                                    .throw_type_error("iterator does not have a throw method");
                            }
                            _ => {
                                realm_scope.throw_internal_error(&format!(
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
                        let func_val = unsafe { *call_argv.offset(-1) };

                        // eval: call function
                        let ret_val = if func_val.is_same(realm_scope.eval_func) {
                            if call_argc >= 1 {
                                // in case `eval(obj, ...)`
                                // where call_argv[0] is the `obj` arg, call_argv[-1] is `eval` func
                                let obj = unsafe { &*call_argv };
                                obj.eval_object(
                                    realm_scope,
                                    JSValue::UNDEFINED,
                                    EvalType::Direct,
                                    EvalFlags::empty(),
                                    scope_idx as _,
                                )
                            } else {
                                // eval() returns undefined
                                Ok(JSValue::UNDEFINED)
                            }
                        } else {
                            func_val.call_internal(
                                realm_scope,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                },
                                0,
                            )
                        };

                        // pop and drop values on stack since `func_val` position
                        // for i in -1..call_argc as isize {
                        //     unsafe {
                        //         *call_argv.offset(i) = JSValue::UNDEFINED;
                        //     }
                        // }
                        SP_GO!(-(call_argc + 1));

                        if let Ok(v) = ret_val {
                            // push result value on stack
                            SP_PUT!(0, v);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::APPLY_EVAL => {
                        let var_scope_idx = PC_FETCH!(u16) as i32 - 1; // NOTE: could be minus
                        let func_val = SP_REF!(-2);

                        if let Ok(arg_list) = build_arg_list(realm_scope, SP_VAL!(-1)) {
                            let ret_val = {
                                if func_val.is_same(realm_scope.eval_func) {
                                    if !arg_list.is_empty() {
                                        // eval(obj, ...)
                                        arg_list[0].eval_object(
                                            realm_scope,
                                            JSValue::UNDEFINED,
                                            EvalType::Direct,
                                            EvalFlags::empty(),
                                            var_scope_idx,
                                        )
                                        // return value will be placed on stack, and is traced by gc marker.
                                        // it's no need to promote return value
                                    } else {
                                        // eval() returns undefined
                                        Ok(JSValue::UNDEFINED)
                                    }
                                } else {
                                    func_val.call(realm_scope, JSValue::UNDEFINED, &arg_list)
                                    // return value will be placed on stack, and is traced by gc marker.
                                    // it's no need to promote return value
                                }
                            };

                            if let Ok(v) = ret_val {
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
                        #[cfg(feature = "builtin_regexp")]
                        if let JSValue::Int(re_id) = SP_VAL!(-1) {
                            let re = Box::new(
                                realm_scope.rt().regexp_precompiled[re_id as usize].clone(),
                            );
                            let regexp = crate::builtins::regexp::RegExp::from(
                                realm_scope,
                                JSValue::UNDEFINED,
                                re,
                            )
                            .map_or(JSValue::EXCEPTION, |o| o.into());
                            SP_PUT!(-2, regexp);
                            SP_GO!(-1);
                        }
                        #[cfg(not(feature = "builtin_regexp"))]
                        {
                            realm_scope.throw_internal_error("regex not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::GET_SUPER => {
                        if let Ok(proto) = SP_REF!(-1).prototype(realm_scope) {
                            SP_PUT!(-1, proto);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::IMPORT => {
                        if let Ok(res) = js_dynamic_import(realm_scope, SP_VAL!(-1)) {
                            SP_PUT!(-1, res);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::CHECK_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        if let Ok(b) = realm_scope.check_global_var(atom) {
                            SP_PUT!(0, JSValue::Bool(b));
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_VAR_UNDEF | OpCode::GET_VAR => {
                        // [^] -> [value, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        if let Ok(val) = realm_scope.get_global_var(atom, opcode == OpCode::GET_VAR)
                        {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_VAR | OpCode::PUT_VAR_INIT => {
                        // [val, ^] -> [^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let res = realm_scope.set_global_var(
                            atom,
                            SP_VAL!(-1),
                            (opcode as u8 - OpCode::PUT_VAR as u8) as _,
                        );

                        SP_GO!(-1);
                        if res.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_VAR_STRICT => {
                        // [bool, val, ^] -> [^]
                        let atom = Atom::from_id(PC_FETCH!(u32));

                        if let JSValue::Bool(false) = SP_VAL!(-2) {
                            JS_ThrowReferenceErrorNotDefined(realm_scope, atom);
                            break State::Exception;
                        } else {
                            let ret = realm_scope.set_global_var(atom, SP_VAL!(-1), 2);
                            SP_GO!(-2);
                            if unlikely(ret.is_err()) {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::CHECK_DEFINE_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        let r = realm_scope.define_global_var_checked(atom, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        let r = realm_scope.define_global_var(atom, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_FUNC => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);

                        let func = SP_VAL!(-1);
                        SP_GO!(-1);

                        let r = realm_scope.define_global_function(atom, func, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_LOC => {
                        // [^] -> [value, ^]
                        let idx = PC_FETCH!(u16);

                        #[cfg(debug_assertions)]
                        if let Some(n) = VAR_REF!(idx).gc_node() {
                            n.debug_assert_node_valid(rt.gc_heap_mut());
                        }

                        SP_PUT!(0, *VAR_REF!(idx));
                        SP_GO!(1);
                    }
                    OpCode::PUT_LOC => {
                        // [value, ^] -> [^]
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = SP_VAL!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::SET_LOC => {
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = SP_VAL!(-1);
                    }
                    OpCode::SET_LOC_UNINITIALIZED => {
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = JSValue::UNINITIALIZED;
                    }
                    OpCode::GET_ARG => {
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, *ARG_REF!(idx));
                        SP_GO!(1);
                    }
                    OpCode::PUT_ARG => {
                        let idx = PC_FETCH!(u16);
                        *ARG_REF!(idx) = SP_VAL!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::SET_ARG => {
                        let idx = PC_FETCH!(u16);
                        *ARG_REF![idx] = SP_VAL!(-1);
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::GET_LOC8..=OpCode::SET_VAR_REF3 => {
                        todo!()
                    }

                    OpCode::GET_VAR_REF => {
                        // retrive value of var_ref, push it on stack
                        // [^] -> [value, ^]
                        let idx = PC_FETCH!(u16);
                        let val = captured_vars[idx as usize].value();
                        SP_PUT!(0, val);
                        SP_GO!(1);
                    }
                    OpCode::PUT_VAR_REF => {
                        // [value, ^] -> [^]
                        // consume and store stack top value to var_ref
                        let idx = PC_FETCH!(u16);
                        captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::SET_VAR_REF => {
                        // [value, ^] -> [value, ^]
                        // store stack top value to var_ref
                        let idx = PC_FETCH!(u16);
                        captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                    }

                    OpCode::GET_VAR_REF_CHECK | OpCode::PUT_VAR_REF_CHECK => {
                        let idx = PC_FETCH!(u16);

                        let val = captured_vars[idx as usize].value();
                        if unlikely(val.is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(realm_scope, self, idx, true);
                            break State::Exception;
                        } else if opcode == OpCode::GET_VAR_REF_CHECK {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_VAR_REF_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        if unlikely(!captured_vars[idx as usize].value().is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(realm_scope, self, idx, true);
                            break State::Exception;
                        } else {
                            captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                            SP_GO!(-1);
                        }
                    }

                    OpCode::GET_LOC_CHECK | OpCode::GET_LOC_CHECKTHIS => {
                        // [^] -> [value, ^]
                        let idx = PC_FETCH!(u16);

                        let value = VAR_REF![idx];
                        if unlikely(value.is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(caller_ctx, self, idx, false);
                            break State::Exception;
                        } else {
                            SP_PUT!(0, *value);
                            SP_GO!(1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(pval.is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(realm_scope, self, idx, false);
                            break State::Exception;
                        } else {
                            *pval = SP_VAL!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(!pval.is_uninitialized()) {
                            realm_scope
                                .throw_reference_error("'this' can be initialized only once");
                            break State::Exception;
                        } else {
                            *pval = SP_VAL!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::CLOSE_LOC => {
                        let idx = PC_FETCH!(u16);
                        stack_frame.close_var_ref(idx, false);
                    }
                    OpCode::MAKE_LOC_REF | OpCode::MAKE_ARG_REF | OpCode::MAKE_VAR_REF_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(
                            0,
                            realm_scope
                                .create_base_object_with_proto(None)
                                .map_or(JSValue::EXCEPTION, JSValue::Object)
                        );
                        SP_GO!(1);
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break State::Exception;
                        }

                        let var_ref = if opcode == OpCode::MAKE_VAR_REF_REF {
                            Ok(captured_vars[idx as usize])
                        } else {
                            stack_frame.get_var_ref(
                                realm_scope,
                                idx,
                                opcode == OpCode::MAKE_ARG_REF,
                            )
                        };
                        match var_ref {
                            Ok(vr) => {
                                if unsafe { SP_MUT!(-1).as_object_mut_unchecked() }
                                    .add_own_property(
                                        realm_scope,
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
                                        atom.to_value(realm_scope).unwrap_or(JSValue::EXCEPTION)
                                    );
                                    SP_GO!(1);
                                }
                            }
                            Err(_) => {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::MAKE_VAR_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        if let Ok(vs) = realm_scope.build_global_varref(atom) {
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

                        if unlikely(realm_scope.poll_interrupts()) {
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

                        if js_poll_interrupts(realm_scope) != 0 {
                            state = State::Exception;
                        }
                    }

                    OpCode::IF_TRUE | OpCode::IF_FALSE => {
                        let b = opcode == OpCode::IF_TRUE;
                        let op_1 = SP_VAL!(-1);
                        SP_GO!(-1);

                        // skip jump offset
                        PC_GO!(4);

                        let test = match op_1 {
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

                        if b == test {
                            // jump pc
                            pc = unsafe {
                                pc.offset((read_from_ptr::<i32>(pc.offset(-4)) - 4) as isize)
                            };
                        }

                        if unlikely(realm_scope.poll_interrupts()) {
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
                                realm_scope.throw_internal_error("invalid ret value");
                                break State::Exception;
                            } else {
                                SP_GO!(-1);
                                // jump pc
                                pc = unsafe { pc_base.offset(pos as isize) };
                            }
                        } else {
                            realm_scope.throw_internal_error("invalid ret value");
                            break State::Exception;
                        }
                    }
                    OpCode::FOR_IN_START => {
                        if js_for_in_start(realm_scope, sp).is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::FOR_IN_NEXT => {
                        if js_for_in_next(realm_scope, sp).is_err() {
                            break State::Exception;
                        }
                        SP_GO!(2);
                    }
                    OpCode::FOR_OF_START | OpCode::FOR_AWAIT_OF_START => {
                        // [obj, ^] -> [iter_obj, next_method, catchoffset(0), ^]
                        // [obj, ^] -> [iter_obj, next_method, catchoffset(0), ^]
                        let is_async = opcode == OpCode::FOR_AWAIT_OF_START;

                        if let Ok((iter_obj, next_method)) = SP_REF!(-1)
                            .get_iterator_must(realm_scope, is_async)
                            .and_then(|o| {
                                o.get_property(realm_scope, Atom::CONST_next)
                                    .map(|m| (o, m))
                            })
                        {
                            SP_PUT!(-1, JSValue::from(iter_obj)); // result of obj[Symbol.iterator]() or obj[Symbol.asyncIterator]()
                            SP_PUT!(0, next_method); // iter_obj.next
                            SP_GO!(1);
                            SP_PUT!(0, JSValue::CatchOffset(0)); // mark for_of start anchor
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::FOR_OF_NEXT => {
                        // [iter_obj, next, ^] -> [value, done, ^]
                        let offset = -3 - PC_FETCH!(u8) as i32;
                        let r = unsafe { js_for_of_next(realm_scope, sp, offset) };
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                        SP_GO!(2);
                    }
                    OpCode::ITERATOR_GET_VALUE_DONE => {
                        if unlikely(unsafe { js_iterator_get_value_done(realm_scope, sp) } != 0) {
                            break State::Exception;
                        } else {
                            SP_GO!(1);
                        }
                    }
                    OpCode::ITERATOR_CHECK_OBJECT => {
                        if unlikely(!SP_REF!(-1).is_object()) {
                            realm_scope.throw_type_error("iterator must return an object");
                            break State::Exception;
                        }
                    }
                    OpCode::ITERATOR_CLOSE => {
                        // [iter_obj, next, catch_offset, ^] -> [^]
                        SP_GO!(-2);

                        // SP[-1] is iter_obj
                        if let JSValue::Object(o) = SP_VAL!(-1)
                            && unlikely(ObjectIter::close_iter_internal(realm_scope, o).is_err())
                        {
                            break State::Exception; // leave iter_obj in stack
                        }
                        SP_GO!(-1); // pop iter_obj
                    }
                    OpCode::NIP_CATCH => {
                        // [catch_offset, ..., ret_val, ^] -> [ret_eval, ^]
                        SP_GO!(-1);
                        // keep ret_val
                        let ret_val = SP_VAL!(0);
                        // drop ... values
                        while sp > sp_base && !matches!(SP_REF!(-1), JSValue::CatchOffset(_)) {
                            SP_GO!(-1);
                            // let _ = SP_VAL!(0);
                        }
                        if unlikely(sp == sp_base) {
                            realm_scope.throw_internal_error("nip_catch");
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
                        if let Ok(ret) = SP_REF!(-3).call(
                            realm_scope,
                            SP_VAL!(-4),
                            std::slice::from_ref(SP_REF!(-1)),
                        ) {
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
                            realm_scope,
                            if flags & 1 != 0 {
                                Atom::CONST_throw
                            } else {
                                Atom::CONST_return
                            },
                        ) {
                            let ret_flag = if unlikely(method.is_undefined_or_null()) {
                                true
                            } else {
                                if let Ok(ret) = method.call(
                                    realm_scope,
                                    SP_VAL!(-4),
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
                        let op_1: JSValue = SP_VAL!(-1);
                        let b = match op_1 {
                            JSValue::Int(i) => i != 0,
                            JSValue::Bool(b) => b,
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
                        let atom = Atom::from_id(PC_FETCH!(u32));

                        if let Ok(val) = SP_REF!(-1).get_property(realm_scope, atom) {
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
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let obj = SP_VAL!(-2);
                        let value = SP_VAL!(-1);
                        SP_GO!(-2);

                        let r = obj.set_property_internal(
                            realm_scope,
                            atom,
                            value,
                            None,
                            JS_PROP_THROW_STRICT,
                        );
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::PRIVATE_SYMBOL => {
                        #[cfg(feature = "builtin_symbol")]
                        {
                            let atom = Atom::from_id(PC_FETCH!(u32));
                            if let Ok(val) =
                                realm_scope.create_symbol_from_atom(atom, AtomKind::Private)
                            {
                                SP_PUT!(0, val);
                                SP_GO!(1);
                            } else {
                                break State::Exception;
                            }
                        }
                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope.throw_internal_error("Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::GET_PRIVATE_FIELD => {
                        #[cfg(feature = "builtin_symbol")]
                        {
                            let op_2 = SP_VAL!(-2);
                            let op_1 = SP_VAL!(-1);
                            let val = op_2
                                .get_private_field(realm_scope, op_1)
                                .unwrap_or(JSValue::EXCEPTION);
                            SP_PUT!(-2, val);
                            SP_GO!(-1);

                            // because has done go(-1), so SP[-1] is orginal SP[-2]
                            if unlikely(SP_REF!(-1).is_exception()) {
                                break State::Exception;
                            }
                        }

                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope.throw_internal_error("Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [^]
                        #[cfg(feature = "builtin_symbol")]
                        {
                            let ret = SP_MUT!(-3).set_private_field(
                                realm_scope,
                                SP_VAL!(-1),
                                SP_VAL!(-2),
                            );
                            SP_GO!(-3);
                            if unlikely(ret.is_err()) {
                                break State::Exception;
                            }
                        }

                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope.throw_internal_error("Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [this_obj, ^]
                        #[cfg(feature = "builtin_symbol")]
                        {
                            let ret = SP_MUT!(-3).define_private_field(
                                realm_scope,
                                SP_VAL!(-2),
                                SP_VAL!(-1),
                            );
                            SP_GO!(-2);
                            if unlikely(ret.is_err()) {
                                break State::Exception;
                            }
                        }

                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope.throw_internal_error("Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_FIELD => {
                        // [this_obj, value, ^] -> [this_obj, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let ret = SP_MUT!(-2).define_property_value(
                            realm_scope,
                            atom,
                            SP_VAL!(-1),
                            JS_PROP_C_W_E | JS_PROP_THROW,
                        );
                        SP_GO!(-1);
                        if unlikely(ret.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_NAME => {
                        // [obj, ^] -> [obj, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let r = SP_MUT!(-1)
                            .as_object_mut()
                            .ok_or(-1)
                            .and_then(|o| o.define_name(realm_scope, atom, JS_PROP_CONFIGURABLE));
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_NAME_COMPUTED => {
                        // [name, value, ^] -> [name, value, ^]
                        let r = SP_MUT!(-1).as_object_mut().unwrap().define_name_computed(
                            realm_scope,
                            SP_VAL!(-2),
                            JS_PROP_CONFIGURABLE,
                        );
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::SET_PROTO => {
                        // [obj, proto, ^] -> [obj, ^]
                        let proto = SP_VAL!(-1);
                        if proto.is_object() || proto.is_null() {
                            let r = SP_MUT!(-2).set_prototype_internal(realm_scope, proto, true);
                            if unlikely(r.is_err()) {
                                break State::Exception;
                            }
                        }
                        // SP_TAKE!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::SET_HOME_OBJECT => {
                        // [home_obj, func, ^] -> [home_obj, func, ^]
                        if let Some(fo) = SP_MUT!(-1).as_object_BCWrapper_mut() {
                            fo.method_set_home_object(SP_VAL!(-2).to_object());
                        } else {
                            #[cfg(debug_assertions)]
                            unreachable!("SET_HOME_OBJECT expect BytecodeFunction object");
                        }
                    }
                    OpCode::DEFINE_METHOD | OpCode::DEFINE_METHOD_COMPUTED => {
                        // DEFINE_METHOD: [this_obj, value, ^] -> [this_obj, ^]
                        // DEFINE_METHOD_COMPUTED: [this_obj, name, value, ^] -> [this_obj, ^]
                        let (is_computed, atom, op_flags) =
                            if opcode == OpCode::DEFINE_METHOD_COMPUTED {
                                // u8
                                (true, Atom::NULL, PC_FETCH!(u8))
                            } else {
                                // atom_u8
                                (false, Atom::from_id(PC_FETCH!(u32)), PC_FETCH!(u8))
                            };

                        let name = if atom.is_null() {
                            // compute name from value SP[-2]
                            if let Ok(atom) = SP_REF!(-2).to_atom(realm_scope) {
                                atom
                            } else {
                                break State::Exception;
                            }
                        } else {
                            atom
                        };

                        let mut flags = JS_PROP_HAS_CONFIGURABLE
                            | JS_PROP_CONFIGURABLE
                            | JS_PROP_HAS_ENUMERABLE
                            | JS_PROP_THROW;

                        if op_flags & OP_DEFINE_METHOD_ENUMERABLE != 0 {
                            flags |= JS_PROP_ENUMERABLE
                        }

                        let func_obj: &mut JSValue;
                        let mut value = JSValue::UNDEFINED;
                        let mut getter = JSValue::UNDEFINED;
                        let mut setter = JSValue::UNDEFINED;

                        match op_flags & (OP_DEFINE_METHOD_GETTER | OP_DEFINE_METHOD_SETTER) {
                            OP_DEFINE_METHOD_METHOD => {
                                value = SP_VAL!(-1);
                                func_obj = &mut value;
                                flags |=
                                    JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE;
                            }
                            OP_DEFINE_METHOD_GETTER => {
                                getter = SP_VAL!(-1);
                                func_obj = &mut getter;
                                flags |= JS_PROP_HAS_GET;
                            }
                            OP_DEFINE_METHOD_SETTER => {
                                setter = SP_VAL!(-1);
                                func_obj = &mut setter;
                                flags |= JS_PROP_HAS_SET;
                            }
                            _ => unreachable!(),
                        }

                        let this_obj = SP_MUT!(if is_computed { -3 } else { -2 });
                        let mut ret =
                            js_method_set_properties(realm_scope, func_obj, name, flags, this_obj);

                        if ret.is_ok() {
                            ret = this_obj
                                .define_property(realm_scope, name, value, getter, setter, flags)
                                .map(|_| ());
                        }

                        if is_computed {
                            // SP_TAKE!(-2);
                            SP_GO!(-2);
                        } else {
                            SP_GO!(-1);
                        }

                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_CLASS | OpCode::DEFINE_CLASS_COMPUTED => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let class_flags = PC_FETCH!(u8) as u32;
                        let r = js_op_define_class(
                            realm_scope,
                            sp,
                            atom,
                            class_flags as _,
                            captured_vars,
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
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(-2, val);
                        SP_GO!(-1);

                        if unlikely(is_err) {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL2 => {
                        // [array, prop, ^] -> [array, value, ^]
                        let val = SP_REF!(-2)
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(-1, val);

                        if unlikely(is_err) {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL3 => {
                        // [array, prop, ^] -> [array, prop1, value, ^]
                        match SP_REF!(-2) {
                            JSValue::Int(_) | JSValue::String(_) | JSValue::Symbol(_) => {}
                            a => {
                                // must be tested before ToPropertyKey
                                if a.is_undefined_or_null() {
                                    realm_scope.throw_type_error("value has no property ");
                                    break State::Exception;
                                }
                                let Ok(prop1) = SP_REF!(-1).to_propkey(realm_scope) else {
                                    break State::Exception;
                                };
                                SP_PUT!(-1, prop1);
                            }
                        }

                        let val = SP_REF!(-2)
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(0, val);
                        SP_GO!(1);

                        if unlikely(is_err) {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_REF_VALUE => {
                        // [obj, prop, ^] -> [obj, value, ^]
                        if unlikely(SP_REF!(-2).is_undefined()) {
                            if let Ok(atom) = SP_REF!(-1).to_atom(realm_scope) {
                                JS_ThrowReferenceErrorNotDefined(realm_scope, atom);
                            }
                            break State::Exception;
                        } else if let Ok(val) =
                            SP_REF!(-2).get_property_value(realm_scope, SP_VAL!(-1))
                        {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_SUPER_VALUE => {
                        if let Ok(atom) = SP_REF!(-1).to_atom(realm_scope) {
                            let val = SP_REF!(-2).get_property_internal(
                                realm_scope,
                                atom,
                                SP_VAL!(-3),
                                false,
                            );

                            if let Ok(val) = val {
                                // SP_TAKE!(-1);
                                // SP_TAKE!(-2);
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
                        // [arr, prop, value, ^] -> [^]
                        let mut arr = SP_VAL!(-3);
                        let ret = arr.set_property_value(
                            realm_scope,
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                            JS_PROP_THROW_STRICT,
                        );
                        SP_GO!(-3);
                        if unlikely(ret.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_REF_VALUE => {
                        // [obj, prop, val, ^] -> [^]
                        let mut obj = SP_VAL!(-3);
                        let prop = SP_VAL!(-2);
                        let val = SP_VAL!(-1);
                        SP_GO!(-3);

                        let mut flags = JS_PROP_THROW_STRICT;

                        if unlikely(obj.is_undefined()) {
                            if rt.is_strict_mode() {
                                if let Ok(atom) = prop.to_atom(realm_scope) {
                                    JS_ThrowReferenceErrorNotDefined(realm_scope, atom);
                                }
                                break State::Exception;
                            }
                            obj = JSValue::Object(realm_scope.global_obj());
                        } else if rt.is_strict_mode() {
                            flags |= JS_PROP_NO_ADD;
                        }

                        let ret = obj.set_property_value(realm_scope, prop, val, flags);
                        if ret.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_SUPER_VALUE => {
                        // [this_obj, obj, prop, val, ^] -> [^]
                        let this_obj = SP_VAL!(-4);
                        let obj = SP_VAL!(-3);
                        let prop = SP_VAL!(-2);
                        let val = SP_VAL!(-1);
                        SP_GO!(-4);

                        if unlikely(!obj.is_object()) {
                            realm_scope.throw_type_error_not_an_object();
                            break State::Exception;
                        }

                        if let Ok(atom) = prop.to_atom(realm_scope) {
                            debug_assert!(this_obj.is_object());

                            let ret = obj.set_property_internal(
                                realm_scope,
                                atom,
                                val,
                                this_obj.as_object().copied(),
                                JS_PROP_THROW_STRICT,
                            );

                            if unlikely(ret.is_err()) {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_ARRAY_EL => {
                        // [obj, prop, value, ^] -> [obj, prop, ^]
                        let obj = SP_MUT!(-3);
                        let prop = SP_REF!(-2);
                        let value = SP_VAL!(-1);

                        let ret = prop.to_atom(realm_scope).and_then(|a| {
                            obj.define_property_value(
                                realm_scope,
                                a,
                                value,
                                JS_PROP_C_W_E | JS_PROP_THROW,
                            )
                        });

                        SP_GO!(-1);
                        if unlikely(ret.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::APPEND => {
                        // [dest, start_pos, src, ^] -> [dest, end_pos, ^]
                        // all elements from `src` are appended to `dest` array, staring from `start_pos` index
                        // after operation, end_pos of dest array if put to stack top
                        if unsafe { js_append_enumerate(realm_scope, sp).is_ok() } {
                            // pop `src`
                            // SP_TAKE!(-1);
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::COPY_DATA_PROPERTIES => {
                        // target source excludeList
                        // stack offsets (-1 based):
                        // 2 bits for target: SP[-1 - (mask & 3)],
                        // 3 bits for source: SP[SP_REF!(-1 - ((mask >> 2) & 7)],
                        // 2 bits for exclusionList: SP[-1 - ((mask >> 5) & 7)]
                        let mask = PC_FETCH!(u8) as i32;
                        let r = SP_MUT!(-1 - (mask & 3)).copy_data_properties_from(
                            realm_scope,
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
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        let res = match opcode {
                            OpCode::ADD => JSValue::arith_add(realm_scope, op1, op2),
                            OpCode::SUB => JSValue::arith_sub(realm_scope, op1, op2),
                            OpCode::MUL => JSValue::arith_mul(realm_scope, op1, op2),
                            OpCode::DIV => JSValue::arith_div(realm_scope, op1, op2),
                            OpCode::MOD => JSValue::arith_mod(realm_scope, op1, op2),
                            OpCode::POW => JSValue::arith_pow(realm_scope, op1, op2),
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
                        let op1 = SP_VAL!(-1);
                        SP_GO!(-1);

                        let pv = VAR_REF![idx];
                        if let Ok(val) = JSValue::arith_add(realm_scope, *pv, op1) {
                            *pv = val;
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PLUS | OpCode::NEG | OpCode::INC | OpCode::DEC => {
                        let op1 = SP_VAL!(-1);
                        if opcode == OpCode::PLUS
                            && matches!(op1, JSValue::Int(_) | JSValue::Float64(_))
                        {
                            // + before an int or float: do nothing
                            SP_PUT!(-1, op1);
                        } else if let Ok(res) =
                            UnaryOp::new(realm_scope, op1, true).map(|unary| match opcode {
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
                    OpCode::POST_INC | OpCode::POST_DEC => {
                        let op1 = SP_VAL!(-1);
                        if let Ok(n1) = op1.to_number(realm_scope) {
                            SP_PUT!(-1, JSValue::from(n1));
                            if let Ok(res) =
                                UnaryOp::new(realm_scope, JSValue::from(n1), true).map(|unary| {
                                    match opcode {
                                        OpCode::POST_INC => unary.inc(),
                                        OpCode::POST_DEC => unary.dec(),
                                        _ => unreachable!(),
                                    }
                                })
                            {
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

                        if let Ok(res) =
                            UnaryOp::new(realm_scope, *op1, true).map(|unary| match opcode {
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
                            UnaryOp::new_int(realm_scope, SP_VAL!(-1)).map(|unary| unary.not())
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
                        if let Ok(new_sp) = op_binary_logic(realm_scope, opcode, sp) {
                            stack_frame.cur_sp_offset =
                                unsafe { new_sp.offset_from(sp_base) as i32 };
                            sp = new_sp;
                        } else {
                            break State::Exception;
                        }
                    }

                    OpCode::LT | OpCode::LTE | OpCode::GT | OpCode::GTE => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);

                        if let Ok(rel) = op1.arith_cmp(realm_scope, op2) {
                            let b = matches!(
                                (rel, opcode),
                                (Some(std::cmp::Ordering::Less), OpCode::LT | OpCode::LTE)
                                    | (Some(std::cmp::Ordering::Greater), OpCode::GT | OpCode::GTE)
                                    | (Some(std::cmp::Ordering::Equal), OpCode::LTE | OpCode::GTE)
                            );
                            SP_PUT!(-2, JSValue::Bool(b));
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::EQ | OpCode::NEQ => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        if let Ok(b) = op1.arith_eq(realm_scope, op2) {
                            SP_PUT!(-2, JSValue::Bool(b ^ (opcode == OpCode::NEQ)));
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::STRICT_EQ | OpCode::STRICT_NEQ => {
                        let mut b = SP_VAL!(-2).strict_eq(SP_VAL!(-1));
                        if opcode == OpCode::STRICT_NEQ {
                            b = !b;
                        }
                        SP_PUT!(-2, JSValue::Bool(b));
                        SP_GO!(-1);
                    }

                    OpCode::IN => {
                        // [prop, obj, ^] -> [bool, ^]
                        if let Ok(b) = js_operator_in(realm_scope, SP_REF!(-1), SP_REF!(-2)) {
                            SP_PUT!(-2, JSValue::Bool(b));
                            //  SP_TAKE!(-1);
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PRIVATE_IN => {
                        #[cfg(feature = "builtin_symbol")]
                        if unlikely(
                            crate::bytecode::operators::js_operator_private_in(realm_scope, sp)
                                .is_err(),
                        ) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }

                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope.throw_internal_error("Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::INSTANCEOF => {
                        if unlikely(js_operator_instanceof(realm_scope, sp).is_err()) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::TYPEOF => {
                        let op1 = SP_VAL!(-1);
                        let atom = op1.type_of();
                        SP_PUT!(
                            -1,
                            atom.force_string(realm_scope)
                                .map_or(JSValue::EXCEPTION, JSValue::from)
                        );
                    }
                    OpCode::DELETE => {
                        if unlikely(js_operator_delete(realm_scope, sp) != 0) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::DELETE_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        match realm_scope
                            .global_obj()
                            .delete_property(realm_scope, atom, 0)
                        {
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
                            if let Ok(o) = SP_VAL!(-1).objectify(realm_scope) {
                                SP_PUT!(-1, JSValue::from(o));
                            } else {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::TO_PROPKEY => {
                        // [value, ^] -> [string_or_symbol, ^]
                        match SP_REF!(-1) {
                            JSValue::Int(_) | JSValue::String(_) => {}
                            #[cfg(feature = "builtin_symbol")]
                            JSValue::Symbol(_) => {}
                            _ => {
                                if let Ok(pk) = SP_REF!(-1).to_propkey(realm_scope) {
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
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let diff = PC_FETCH!(i32);
                        let is_with = PC_FETCH!(u8) != 0;

                        let mut env_obj = SP_VAL!(-1);

                        if let Ok(ret) = env_obj.has_property(realm_scope, atom) {
                            let in_env_obj = if ret {
                                if is_with {
                                    if let Ok(b) = js_has_unscopable(realm_scope, env_obj, atom) {
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
                                        if let Ok(val) = env_obj.get_property(realm_scope, atom) {
                                            SP_PUT!(-1, val);
                                        } else {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_PUT_VAR => {
                                        // XXX: check if strict mode
                                        let ret = env_obj.set_property_internal(
                                            realm_scope,
                                            atom,
                                            SP_VAL!(-2),
                                            None,
                                            JS_PROP_THROW_STRICT,
                                        );
                                        // SP_TAKE!(-1);
                                        SP_GO!(-2);
                                        if unlikely(ret.is_err()) {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_DELETE_VAR => {
                                        if let Ok(b) = env_obj.delete_property(realm_scope, atom, 0)
                                        {
                                            SP_PUT!(-1, JSValue::Bool(b));
                                        } else {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_MAKE_REF => {
                                        // produce a pair object/propname on the stack
                                        SP_PUT!(
                                            0,
                                            atom.to_value(realm_scope)
                                                .unwrap_or(JSValue::EXCEPTION)
                                        );
                                        SP_GO!(1);
                                    }
                                    OpCode::WITH_GET_REF => {
                                        // produce a pair object/method on the stack
                                        if let Ok(val) = env_obj.get_property(realm_scope, atom) {
                                            SP_PUT!(0, val);
                                            SP_GO!(1);
                                        } else {
                                            break State::Exception;
                                        }
                                    }
                                    OpCode::WITH_GET_REF_UNDEF => {
                                        // produce a pair undefined/function on the stack
                                        if let Ok(val) = env_obj.get_property(realm_scope, atom) {
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
                                //  SP_TAKE!(-1);
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
                    OpCode::INITIAL_YIELD => {
                        break State::Done(JSValue::Int(FUNC_RET_INITIAL_YIELD), true);
                    }
                    OpCode::RETURN_ASYNC => {
                        break State::Done(JSValue::UNDEFINED, true);
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

                    _ => {
                        realm_scope.throw_internal_error(&format!(
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
                            realm_scope,
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

                #[cfg(all(debug_assertions, feature = "debug_gc"))]
                {
                    // flush nodes in exec scope after each instruction
                    realm_scope.clear_locals();
                    // perform garbage collect after each opcode execution to strictly ensure gc is correct.
                    rt.gc_collect();
                }
                #[cfg(not(all(debug_assertions, feature = "debug_gc")))]
                if rt.gc_req {
                    // Live values have been put on the stack frame and will be marked by GC.
                    // Remaining locals in realm_scope are transient and safe to release.
                    // This avoids realm_scope accumulate too many unnecessary locals.
                    realm_scope.clear_locals();
                    // perform garbage collect after each opcode execution to strictly ensure gc is correct.
                    rt.gc_collect();
                }
            }; // opcodes loop

            match state {
                State::Done(ret_val, is_generator) => {
                    stack_frame.handle_exit(
                        realm_scope,
                        unsafe { pc.offset_from(pc_base) as usize },
                        unsafe { sp.offset_from(sp_base) as usize },
                        is_generator,
                    );

                    return ret_val;
                }

                State::Exception => {
                    // Interrupt bypasses catch handling — it is not a JS exception.
                    if realm_scope.rt().interrupt_code.is_some() {
                        let pc_offset = unsafe { pc.offset_from(pc_base) as usize };
                        stack_frame.handle_exit(
                            realm_scope,
                            pc_offset,
                            0,
                            self.func_kind() != FunctionKind::Normal,
                        );
                        return JSValue::EXCEPTION;
                    }

                    if let Some((pc2, sp2)) =
                        self.catch_exception(realm_scope, stack_frame, pc, pc_base, sp, sp_base)
                    {
                        debug_assert_eq!(stack_frame.cur_sp_offset, unsafe {
                            sp2.offset_from(sp_base) as i32
                        });
                        pc = pc2;
                        sp = sp2;
                        continue 'restart;
                    } else {
                        return JSValue::EXCEPTION;
                    }
                }
            }
        } // restart
    }

    /// try catch exception.
    ///
    /// # Return
    /// * new sp/pc to continue running
    /// * None to indicate exception is unchachable, should abort
    #[cold]
    #[inline(never)]
    fn catch_exception(
        &self,
        ctx: &mut ScopedContext,
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
            // exception was caught. continue running, set pc/sp to jump
            stack_frame.cur_sp_offset = new_sp as i32;
            unsafe { Some((pc_base.add(new_pc), sp_base.add(new_sp))) }
        } else {
            // abort with sp=0
            stack_frame.handle_exit(ctx, pc_offset, 0, self.func_kind() != FunctionKind::Normal);
            None
        }
    }
}

// ============================================================================
// Stackless Interpreter: Trampoline and Arena-aware Execution
// ============================================================================

/// Arena-based equivalent of StackFrame::get_var_ref.
fn arena_get_var_ref(
    ctx: &mut ScopedContext,
    var_idx: u16,
    is_arg: bool,
    arg_base_ptr: *mut JSValue,
    var_base_ptr: *mut JSValue,
    var_ref_list: &mut Vec<VarRef>,
    arg_count: u16,
    var_count: u16,
    mode: ExecMode,
) -> Result<VarRef, i32> {
    if let Some(vr) = var_ref_list
        .iter()
        .find(|x| x.is_match(var_idx, is_arg))
        .copied()
    {
        return Ok(vr);
    }
    let pvalue = unsafe {
        NonNull::new_unchecked(if is_arg {
            debug_assert!(var_idx < arg_count);
            unsafe { arg_base_ptr.offset(var_idx as isize) }
        } else {
            debug_assert!(var_idx < var_count);
            unsafe { var_base_ptr.offset(var_idx as isize) }
        })
    };
    let async_state = if mode.contains(ExecMode::ASYNC) {
        None
    } else {
        None
    };
    let vref = VarRef::create_bind(ctx, var_idx, is_arg, pvalue, async_state)?;
    var_ref_list.push(vref);
    Ok(vref)
}

impl Bytecode {
    /// Execute bytecodes using the arena-based stackless model.
    ///
    /// This variant works with FrameIndex + StackFrameArena instead of
    /// the legacy &mut StackFrame. It is called by execute_trampoline()
    /// and supports returning ExecAction for call/return/unwind control.
    ///
    /// Returns the ExecAction describing what the trampoline should do next.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn execute_with_arena(
        &self,
        realm_scope: &mut ScopedContext,
        arena: &mut StackFrameArena,
        cur_frame_idx: FrameIndex,
        this_obj: JSValue,
        new_target: JSValue,
        func_home_obj: Option<Object>,
        pc_offset: usize,
        sp_offset: usize,
        captured_vars: &mut [VarRef],
        args: &[JSValue],
        throwed: bool,
    ) -> ExecAction {
        let mut rt = realm_scope.rt_ptr();

        let bytecodes = self.opcodes();
        debug_assert!(
            pc_offset < bytecodes.len(),
            "pc_offset={}, bytecodes.len()={}",
            pc_offset,
            bytecodes.len()
        );

        // Extract frame data pointers from the arena.
        // All raw pointers derived here remain stable because:
        // - Arena data is a fixed-capacity Box<[JSValue]> (never resized).
        // - FrameHead is in a Vec that may reallocate, but raw pointers
        //   into arena.data (not FrameHead) are used for data access.
        //
        // Split into immutable then mutable borrows: data_ptr and
        // stack_base_ptr take &self on arena, while frame_mut takes &mut.
        let (arg_base_ptr, var_base_ptr, sp_base, mode, func_obj) = {
            let head = arena.frame(cur_frame_idx);
            let arg_base = arena.data_ptr(head);
            let var_base = unsafe { arg_base.add(head.var_base_offset as usize) };
            let sp = arena.stack_base_ptr(head);
            (arg_base, var_base, sp, head.mode, head.func_obj)
        };
        let head = arena.frame_mut(cur_frame_idx);
        // Store a raw pointer to cur_sp_offset for SP_GO! macro updates.
        // SAFETY: FrameHead is in Vec<FrameHead>, its address is stable
        // for the duration of this function (no arena resizing).
        let cur_sp_offset_ptr: *mut i32 = &mut head.cur_sp_offset;
        let cur_pc_offset_ptr: *mut usize = &mut head.cur_pc_offset;

        let cpools = self.consts();

        let pc_base = bytecodes.as_ptr();
        let mut pc = unsafe { pc_base.add(pc_offset) };
        let mut sp = unsafe { sp_base.add(sp_offset) };

        unsafe {
            *cur_sp_offset_ptr = sp_offset as i32;
        }

        // Resolve captured_vars from the current frame's function object.
        // Each function in the call chain has its own closure_var_refs stored
        // in its BFuncPayload. The trampoline always passes the initial
        // function's captured_vars, which is wrong after a call/frame switch.
        // We re-derive the correct slice from the current frame's func_obj.
        let captured_vars: &mut [VarRef] = {
            let func_obj = arena.frame(cur_frame_idx).func_obj;
            if let Ok(mut bfun) = BCWrapper::try_from(JSValue::from(func_obj)) {
                // SAFETY: bfun is a Copy (GcRef), so payload_mut borrows GC-managed
                // memory that outlives this function call. No GC runs during execution.
                let cv: *mut [VarRef] = bfun.payload_mut().closure_var_refs_mut();
                unsafe { &mut *cv }
            } else {
                // Non-bytecode functions have no closure_var_refs.
                // But execute_with_arena should never be called for them.
                &mut []
            }
        };

        if throwed {
            let cur_pc = unsafe { pc.offset_from(pc_base) as usize };

            if let Some((new_pc, new_sp)) = arena.find_catch_in_frame(cur_frame_idx, realm_scope) {
                unsafe {
                    *cur_sp_offset_ptr = new_sp as i32;
                    pc = pc_base.add(new_pc);
                    sp = sp_base.add(new_sp);
                }
            } else {
                // Exception not caught in this frame; propagate upward.
                return ExecAction::Unwind;
            }
        }

        // ── Stack access macros ──
        // These work with raw pointers and the cur_sp_offset pointer,
        // identical to the legacy versions but using *cur_sp_offset_ptr.

        macro_rules! SP_VAL {
            (0) => {
                unsafe { *sp }
            };
            ($idx:expr) => {
                unsafe { *sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_REF {
            (0) => {
                unsafe { sp.as_ref().unwrap() }
            };
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_ref().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_REF {
            (0) => {
                unsafe { &*sp }
            };
            ($idx:expr) => {
                unsafe { &*sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_MUT {
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_MUT {
            ($idx:expr) => {
                unsafe { &mut *sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_PUT {
            ($idx:expr, $val:expr) => {
                unsafe {
                    *sp.offset(($idx) as isize).as_mut().unwrap() = $val;
                }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_PUT {
            ($idx:expr, $val:expr) => {
                unsafe {
                    *sp.offset(($idx) as isize) = $val;
                }
            };
        }

        macro_rules! SP_GO {
            ($step:expr) => {
                sp = unsafe {
                    *cur_sp_offset_ptr += ($step) as i32;
                    sp.offset(($step) as isize)
                }
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
                unsafe { arg_base_ptr.offset(($idx) as _).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! ARG_REF {
            ($idx:expr) => {
                unsafe { &mut *arg_base_ptr.offset(($idx) as _) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (var_base_ptr.offset(($idx) as isize).as_mut().unwrap()) }
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (&mut *var_base_ptr.offset(($idx) as isize)) }
        };
    }

        #[cfg(debug_assertions)]
        macro_rules! CPOOL {
        [$idx:expr] => {
            cpools.as_ref()[($idx) as usize]
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! CPOOL {
        [$idx:expr] => {
            unsafe {
                *cpools.as_ref().as_ptr().add(($idx) as usize)
            }
        };
    }

        enum State {
            Done(JSValue, bool),
            Exception,
            CallBytecode {
                callee_bc: Bytecode,
                callee_frame_idx: FrameIndex,
                return_sp: usize,
            },
            TailCall {
                callee_bc: Bytecode,
                callee_func_obj: Object,
                callee_buf_cap: usize,
            },
        }

        // ── Opcode dispatch loop ──
        // The outer 'arena_loop handles exception catch re-entry.
        // CallBytecode / TailCall return ExecAction to the trampoline.
        // Done / Exception: Done returns ExecAction::Return, Exception
        // tries local catch and re-enters via continue 'arena_loop.
        'arena_loop: loop {
            let state: State = loop {
                let opcode = unsafe { std::mem::transmute::<u8, OpCode>(PC_FETCH!(u8)) };

                match opcode {
                    // ═══════════════════════════════════════════════════
                    // PUSH / CONST / VALUE ops
                    // ═══════════════════════════════════════════════════
                    OpCode::PUSH_I32 => {
                        // [^] -> [int_value, ^]
                        let a = PC_FETCH!(i32);
                        SP_PUT!(0, JSValue::Int(a));
                        SP_GO!(1);
                    }
                    OpCode::PUSH_CONST => {
                        // [^] -> [const_val, ^]
                        let id = PC_FETCH!(u32);
                        SP_PUT!(0, CPOOL![id]);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_ATOM_VALUE => {
                        let a = PC_FETCH!(u32);
                        SP_PUT!(
                            0,
                            Atom::from_id(a)
                                .to_value(realm_scope)
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
                        let val: JSValue = if !mode.contains(ExecMode::STRICT) {
                            match this_obj {
                                JSValue::Object(_) => this_obj,
                                JSValue::NULL | JSValue::UNDEFINED => {
                                    JSValue::Object(realm_scope.global_obj())
                                }
                                _ => {
                                    if let Ok(v) = this_obj.objectify(realm_scope) {
                                        v.into()
                                    } else {
                                        break State::Exception;
                                    }
                                }
                            }
                        } else {
                            this_obj
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

                    // ═══════════════════════════════════════════════════
                    // OBJECT / SPECIAL_OBJECT
                    // ═══════════════════════════════════════════════════
                    OpCode::OBJECT => {
                        // [^] -> [obj, ^]
                        let obj = realm_scope
                            .create_base_object()
                            .map_or(JSValue::EXCEPTION, JSValue::Object);
                        SP_PUT!(0, obj);
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
                                SP_PUT!(0, js_build_arguments(realm_scope, args));
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::MappedArguments => {
                                let object_proto = realm_scope.class_proto(JS_CLASS_OBJECT);
                                let mut obj = match realm_scope.create_instance_with_proto_payload(
                                    object_proto,
                                    ObjectPayload::MAPPED_ARGUMENTS,
                                ) {
                                    Ok(o) => o,
                                    Err(_) => {
                                        break State::Exception;
                                    }
                                };

                                if obj
                                    .add_own_property(
                                        realm_scope,
                                        Atom::CONST_length,
                                        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                                        JSProperty::Normal(JSValue::Int(args.len() as i32)),
                                    )
                                    .is_err()
                                {
                                    break State::Exception;
                                }

                                let mut abort = false;
                                for i in 0..cmp::min(args.len() as i32, self.arg_count as i32) {
                                    let content = match arena_get_var_ref(
                                        realm_scope,
                                        i as u16,
                                        true,
                                        arg_base_ptr,
                                        var_base_ptr,
                                        &mut arena.frame_mut(cur_frame_idx).var_ref_list,
                                        self.arg_count,
                                        self.var_count,
                                        mode,
                                    ) {
                                        Ok(vr) => JSProperty::VarRef(vr),
                                        Err(_) => {
                                            abort = true;
                                            break;
                                        }
                                    };
                                    if obj
                                        .add_own_property(
                                            realm_scope,
                                            Atom::new_small_int(i as u32),
                                            JS_PROP_C_W_E | JS_PROP_VARREF,
                                            content,
                                        )
                                        .is_err()
                                    {
                                        abort = true;
                                        break;
                                    }
                                }
                                if abort {
                                    break State::Exception;
                                }

                                for (i, a) in args
                                    .iter()
                                    .enumerate()
                                    .skip(cmp::min(args.len(), self.arg_count as usize))
                                {
                                    if obj
                                        .define_property_value_u32(
                                            realm_scope,
                                            i as _,
                                            *a,
                                            JS_PROP_C_W_E,
                                        )
                                        .is_err()
                                    {
                                        abort = true;
                                        break;
                                    }
                                }
                                if abort {
                                    break State::Exception;
                                }

                                #[cfg(feature = "builtin_symbol")]
                                if obj
                                    .define_property_value(
                                        realm_scope,
                                        Atom::CONST_Symbol_iterator,
                                        realm_scope.array_proto_values,
                                        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
                                    )
                                    .is_err()
                                {
                                    break State::Exception;
                                }

                                if obj
                                    .define_property_value(
                                        realm_scope,
                                        Atom::CONST_callee,
                                        JSValue::from(func_obj),
                                        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
                                    )
                                    .is_err()
                                {
                                    break State::Exception;
                                }

                                let ma = JSValue::Object(obj);
                                SP_PUT!(0, ma);
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                            SpecialObject::ThisFunc => {
                                SP_PUT!(0, JSValue::from(func_obj));
                                SP_GO!(1);
                            }
                            SpecialObject::NewTarget => {
                                SP_PUT!(0, new_target);
                                SP_GO!(1);
                            }
                            SpecialObject::HomeObject => {
                                SP_PUT!(
                                    0,
                                    if let Some(ho) = func_home_obj {
                                        JSValue::Object(ho)
                                    } else {
                                        JSValue::UNDEFINED
                                    }
                                );
                                SP_GO!(1);
                            }
                            SpecialObject::VarObject => {
                                SP_PUT!(
                                    0,
                                    realm_scope
                                        .create_base_object_with_proto(None)
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
                                    js_import_meta(realm_scope)
                                        .map_or(JSValue::EXCEPTION, JSValue::Object)
                                );
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break State::Exception;
                                }
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // REST params
                    // ═══════════════════════════════════════════════════
                    OpCode::REST => {
                        let start = PC_FETCH!(u16);
                        if let Ok(arr) = args
                            .iter()
                            .skip(start as usize)
                            .copied()
                            .try_collect::<Vec<_>>()
                            .map_err(|_| {
                                realm_scope.throw_out_of_memory();
                                -1
                            })
                            .and_then(|vv| Array::from_values(realm_scope, vv))
                        {
                            SP_PUT!(0, JSValue::from(arr));
                            SP_GO!(1);
                        } else {
                            SP_PUT!(0, JSValue::EXCEPTION);
                            SP_GO!(1);
                            break State::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Stack manipulation ops
                    // ═══════════════════════════════════════════════════
                    OpCode::DROP => {
                        SP_GO!(-1);
                    }
                    OpCode::NIP => {
                        SP_PUT!(-2, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::NIP1 => {
                        SP_PUT!(-3, SP_VAL!(-2));
                        SP_PUT!(-2, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::DUP => {
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_GO!(1);
                    }
                    OpCode::DUP1 => {
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_PUT!(-1, SP_VAL!(-2));
                        SP_GO!(1);
                    }
                    OpCode::DUP2 => {
                        SP_PUT!(0, SP_VAL!(-2));
                        SP_PUT!(1, SP_VAL!(-1));
                        SP_GO!(2);
                    }
                    OpCode::SWAP => unsafe {
                        std::ptr::swap(sp.offset(-1), sp.offset(-2));
                    },
                    OpCode::SWAP2 => unsafe {
                        std::ptr::swap(sp.offset(-1), sp.offset(-3));
                        std::ptr::swap(sp.offset(-2), sp.offset(-4));
                    },

                    // ═══════════════════════════════════════════════════
                    // Local variable / argument access
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_LOC => {
                        // [^] -> [local_val, ^]
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, *VAR_REF![idx]);
                        SP_GO!(1);
                    }
                    OpCode::PUT_LOC => {
                        // [val, ^] -> [^]
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = SP_VAL!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::GET_ARG => {
                        // [^] -> [arg_val, ^]
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, *ARG_REF!(idx));
                        SP_GO!(1);
                    }
                    OpCode::PUT_ARG => {
                        let idx = PC_FETCH!(u16);
                        *ARG_REF!(idx) = SP_VAL!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::SET_ARG => {
                        let idx = PC_FETCH!(u16);
                        *ARG_REF![idx] = SP_VAL!(-1);
                    }
                    OpCode::SET_LOC => {
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = SP_VAL!(-1);
                    }
                    OpCode::CLOSE_LOC => {
                        let idx = PC_FETCH!(u16);
                        let head = arena.frame_mut(cur_frame_idx);
                        if let Some(vr) = head
                            .var_ref_list
                            .iter_mut()
                            .find(|v| v.is_match(idx, false))
                        {
                            vr.detach();
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Closure variable refs
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_VAR_REF => {
                        let idx = PC_FETCH!(u16);
                        let val = captured_vars[idx as usize].value();
                        SP_PUT!(0, val);
                        SP_GO!(1);
                    }
                    OpCode::PUT_VAR_REF => {
                        let idx = PC_FETCH!(u16);
                        captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::SET_VAR_REF => {
                        let idx = PC_FETCH!(u16);
                        let val_ref = &mut captured_vars[idx as usize];
                        val_ref.set_value(&mut rt, SP_VAL!(0));
                    }
                    OpCode::MAKE_LOC_REF | OpCode::MAKE_ARG_REF | OpCode::MAKE_VAR_REF_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(
                            0,
                            realm_scope
                                .create_base_object_with_proto(None)
                                .map_or(JSValue::EXCEPTION, JSValue::Object)
                        );
                        SP_GO!(1);
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break State::Exception;
                        }

                        let var_ref = if opcode == OpCode::MAKE_VAR_REF_REF {
                            Ok(captured_vars[idx as usize])
                        } else {
                            arena_get_var_ref(
                                realm_scope,
                                idx,
                                opcode == OpCode::MAKE_ARG_REF,
                                arg_base_ptr,
                                var_base_ptr,
                                &mut arena.frame_mut(cur_frame_idx).var_ref_list,
                                self.arg_count,
                                self.var_count,
                                mode,
                            )
                        };
                        match var_ref {
                            Ok(vr) => {
                                if unsafe { SP_MUT!(-1).as_object_mut_unchecked() }
                                    .add_own_property(
                                        realm_scope,
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
                                        atom.to_value(realm_scope).unwrap_or(JSValue::EXCEPTION)
                                    );
                                    SP_GO!(1);
                                }
                            }
                            Err(_) => {
                                break State::Exception;
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Type conversion / unary operations
                    // ═══════════════════════════════════════════════════
                    OpCode::PLUS | OpCode::NEG | OpCode::INC | OpCode::DEC => {
                        let op1 = SP_VAL!(-1);
                        if opcode == OpCode::PLUS
                            && matches!(op1, JSValue::Int(_) | JSValue::Float64(_))
                        {
                            // + before an int or float: do nothing
                            SP_PUT!(-1, op1);
                        } else if let Ok(res) =
                            UnaryOp::new(realm_scope, op1, true).map(|unary| match opcode {
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
                    OpCode::TO_PROPKEY => match SP_REF!(-1) {
                        JSValue::Int(_) | JSValue::String(_) => {}
                        #[cfg(feature = "builtin_symbol")]
                        JSValue::Symbol(_) => {}
                        _ => {
                            if let Ok(pk) = SP_REF!(-1).to_propkey(realm_scope) {
                                SP_PUT!(-1, pk);
                            } else {
                                break State::Exception;
                            }
                        }
                    },
                    OpCode::TO_OBJECT => {
                        if !SP_REF!(-1).is_object() {
                            if let Ok(o) = SP_VAL!(-1).objectify(realm_scope) {
                                SP_PUT!(-1, JSValue::from(o));
                            } else {
                                break State::Exception;
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // CALL / RETURN
                    // ═══════════════════════════════════════════════════
                    OpCode::CALL | OpCode::TAIL_CALL => {
                        // [func_obj, arg1, ..., argN, ^] -> [ret_val, ^]
                        let call_argc = PC_FETCH!(u16);
                        let call_argv = unsafe { sp.offset(-(call_argc as isize)) };

                        unsafe {
                            *cur_pc_offset_ptr = pc.offset_from(pc_base) as _;
                        }

                        let func = unsafe { *call_argv.offset(-1) };

                        // ── Inline bytecode dispatch ──
                        if let Ok(mut bfun) = BCWrapper::try_from(func) {
                            let bc = bfun.payload().bytecode();
                            let func_kind = bc.func_kind();

                            // Generator and async functions need their specific
                            // class_call path (creates AGenPayload/AsyncState and
                            // runs INITIAL_YIELD). Only plain (non-generator)
                            // bytecode functions can use the inline frame setup.
                            if func_kind == FunctionKind::Normal {
                                let callee_buf_cap =
                                    bc.arg_count as usize + bc.local_buf_size as usize;

                                let callee_frame_idx =
                                    arena.alloc_frame(bfun.into(), bc.mode(), callee_buf_cap);
                                arena.frame_mut(callee_frame_idx).prev = cur_frame_idx;

                                // Init callee frame data from stack args.
                                {
                                    let callee_data = {
                                        let h = arena.frame(callee_frame_idx);
                                        arena.data_ptr(h)
                                    };
                                    let callee_head = arena.frame_mut(callee_frame_idx);
                                    let args_slice = unsafe {
                                        std::slice::from_raw_parts(call_argv, call_argc as usize)
                                    };
                                    for (i, arg) in args_slice.iter().enumerate() {
                                        unsafe {
                                            *callee_data.add(i) = *arg;
                                        }
                                    }
                                    callee_head.arg_base_offset = 0;
                                    callee_head.arg_real_count = call_argc;
                                    callee_head.arg_count = std::cmp::max(bc.arg_count, call_argc);
                                    callee_head.var_base_offset = callee_head.arg_count as u32;
                                    callee_head.var_count = bc.var_count;
                                    callee_head.cur_sp_offset = 0;
                                }

                                let return_sp = unsafe {
                                    sp.offset(-(call_argc as isize + 1)).offset_from(sp_base)
                                } as usize;

                                if unlikely(opcode == OpCode::TAIL_CALL) {
                                    break State::TailCall {
                                        callee_bc: bc,
                                        callee_func_obj: bfun.into(),
                                        callee_buf_cap,
                                    };
                                }
                                break State::CallBytecode {
                                    callee_bc: bc,
                                    callee_frame_idx,
                                    return_sp,
                                };
                            }
                            // Generator/async function: fall through to
                            // call_internal for proper class_call handling.
                        }

                        // Native function: use legacy call_internal path.
                        let ret_val = func.call_internal(
                            realm_scope,
                            JSValue::UNDEFINED,
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if let Ok(r) = ret_val {
                            if unlikely(opcode == OpCode::TAIL_CALL) {
                                break State::Done(r, false);
                            } else {
                                SP_GO!(-(call_argc as i32 + 1));
                                SP_PUT!(0, r);
                                SP_GO!(1);
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::CALL_CONSTRUCTOR => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        unsafe {
                            *cur_pc_offset_ptr = pc.offset_from(pc_base) as _;
                        }

                        let func = unsafe { *call_argv.offset(-2) };
                        let ret_val = func.call_internal(
                            realm_scope,
                            JSValue::UNDEFINED,
                            unsafe { *call_argv.offset(-1) },
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            JS_CALL_FLAG_CONSTRUCTOR,
                        );
                        if let Ok(val) = ret_val {
                            SP_GO!(-(call_argc + 2));
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::CALL_METHOD | OpCode::TAIL_CALL_METHOD => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        unsafe {
                            *cur_pc_offset_ptr = pc.offset_from(pc_base) as _;
                        }

                        let func = unsafe { *call_argv.offset(-1) };
                        let ret_val = func.call_internal(
                            realm_scope,
                            unsafe { *call_argv.offset(-2) },
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if let Ok(r) = ret_val {
                            if unlikely(opcode == OpCode::TAIL_CALL_METHOD) {
                                break State::Done(r, false);
                            } else {
                                SP_GO!(-(call_argc + 2));
                                SP_PUT!(0, r);
                                SP_GO!(1);
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::RETURN => {
                        SP_GO!(-1);
                        let ret = SP_VAL!(0);
                        break State::Done(ret, false);
                    }
                    OpCode::RETURN_UNDEF => {
                        break State::Done(JSValue::UNDEFINED, false);
                    }

                    // ═══════════════════════════════════════════════════
                    // Unary / comparison / binary logic
                    // ═══════════════════════════════════════════════════
                    OpCode::POST_INC | OpCode::POST_DEC => {
                        let op1 = SP_VAL!(-1);
                        if let Ok(n1) = op1.to_number(realm_scope) {
                            SP_PUT!(-1, JSValue::from(n1));
                            if let Ok(res) =
                                UnaryOp::new(realm_scope, JSValue::from(n1), true).map(|unary| {
                                    match opcode {
                                        OpCode::POST_INC => unary.inc(),
                                        OpCode::POST_DEC => unary.dec(),
                                        _ => unreachable!(),
                                    }
                                })
                            {
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
                        if let Ok(res) =
                            UnaryOp::new(realm_scope, *op1, true).map(|unary| match opcode {
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
                    OpCode::ADD_LOC => {
                        let idx = PC_FETCH!(u8) as i32;
                        let op1 = SP_VAL!(-1);
                        SP_GO!(-1);
                        let pv = VAR_REF![idx];
                        if let Ok(val) = JSValue::arith_add(realm_scope, *pv, op1) {
                            *pv = val;
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::NOT => {
                        if let Ok(res) =
                            UnaryOp::new_int(realm_scope, SP_VAL!(-1)).map(|unary| unary.not())
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
                        if let Ok(new_sp) = op_binary_logic(realm_scope, opcode, sp) {
                            sp = new_sp;
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::LT | OpCode::LTE | OpCode::GT | OpCode::GTE => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        if let Ok(rel) = op1.arith_cmp(realm_scope, op2) {
                            let b = matches!(
                                (rel, opcode),
                                (Some(std::cmp::Ordering::Less), OpCode::LT | OpCode::LTE)
                                    | (Some(std::cmp::Ordering::Greater), OpCode::GT | OpCode::GTE)
                                    | (Some(std::cmp::Ordering::Equal), OpCode::LTE | OpCode::GTE)
                            );
                            SP_PUT!(-2, JSValue::Bool(b));
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::EQ | OpCode::NEQ => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        if let Ok(b) = op1.arith_eq(realm_scope, op2) {
                            SP_PUT!(-2, JSValue::Bool(b ^ (opcode == OpCode::NEQ)));
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::STRICT_EQ | OpCode::STRICT_NEQ => {
                        let mut b = SP_VAL!(-2).strict_eq(SP_VAL!(-1));
                        if opcode == OpCode::STRICT_NEQ {
                            b = !b;
                        }
                        SP_PUT!(-2, JSValue::Bool(b));
                        SP_GO!(-1);
                    }
                    OpCode::IN => {
                        if let Ok(b) = js_operator_in(realm_scope, SP_REF!(-1), SP_REF!(-2)) {
                            SP_PUT!(-2, JSValue::Bool(b));
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PRIVATE_IN => {
                        #[cfg(feature = "builtin_symbol")]
                        if unlikely(
                            crate::bytecode::operators::js_operator_private_in(realm_scope, sp)
                                .is_err(),
                        ) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope.throw_internal_error("Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::INSTANCEOF => {
                        if unlikely(js_operator_instanceof(realm_scope, sp).is_err()) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::TYPEOF => {
                        let op1 = SP_VAL!(-1);
                        let atom = op1.type_of();
                        SP_PUT!(
                            -1,
                            atom.force_string(realm_scope)
                                .map_or(JSValue::EXCEPTION, JSValue::from)
                        );
                    }
                    OpCode::DELETE => {
                        if unlikely(js_operator_delete(realm_scope, sp) != 0) {
                            break State::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Global variable access
                    // ═══════════════════════════════════════════════════
                    OpCode::CHECK_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        if let Ok(b) = realm_scope.check_global_var(atom) {
                            SP_PUT!(0, JSValue::Bool(b));
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_VAR_UNDEF | OpCode::GET_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        if let Ok(val) = realm_scope.get_global_var(atom, opcode == OpCode::GET_VAR)
                        {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_VAR | OpCode::PUT_VAR_INIT => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let res = realm_scope.set_global_var(
                            atom,
                            SP_VAL!(-1),
                            (opcode as u8 - OpCode::PUT_VAR as u8) as _,
                        );
                        SP_GO!(-1);
                        if res.is_err() {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_VAR_STRICT => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        if let JSValue::Bool(false) = SP_VAL!(-2) {
                            JS_ThrowReferenceErrorNotDefined(realm_scope, atom);
                            break State::Exception;
                        } else {
                            let ret = realm_scope.set_global_var(atom, SP_VAL!(-1), 2);
                            SP_GO!(-2);
                            if unlikely(ret.is_err()) {
                                break State::Exception;
                            }
                        }
                    }
                    OpCode::DEFINE_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        let r = realm_scope.define_global_var(atom, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::CHECK_DEFINE_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        let r = realm_scope.define_global_var_checked(atom, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::DEFINE_FUNC => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let flags = PC_FETCH!(u8);
                        let func = SP_VAL!(-1);
                        SP_GO!(-1);
                        let r = realm_scope.define_global_function(atom, func, flags as _);
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }
                    OpCode::DELETE_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        match realm_scope
                            .global_obj()
                            .delete_property(realm_scope, atom, 0)
                        {
                            Ok(ret) => {
                                SP_PUT!(0, JSValue::Bool(ret));
                                SP_GO!(1);
                            }
                            Err(_) => {
                                break State::Exception;
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Local variable check / init
                    // ═══════════════════════════════════════════════════
                    OpCode::SET_LOC_UNINITIALIZED => {
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = JSValue::UNINITIALIZED;
                    }
                    OpCode::GET_LOC_CHECK | OpCode::GET_LOC_CHECKTHIS => {
                        let idx = PC_FETCH!(u16);
                        let value = VAR_REF![idx];
                        if unlikely(value.is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(realm_scope, self, idx, false);
                            break State::Exception;
                        } else {
                            SP_PUT!(0, *value);
                            SP_GO!(1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(pval.is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(realm_scope, self, idx, false);
                            break State::Exception;
                        } else {
                            *pval = SP_VAL!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(!pval.is_uninitialized()) {
                            realm_scope
                                .throw_reference_error("'this' can be initialized only once");
                            break State::Exception;
                        } else {
                            *pval = SP_VAL!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::GET_VAR_REF_CHECK | OpCode::PUT_VAR_REF_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let val = captured_vars[idx as usize].value();
                        if unlikely(val.is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(realm_scope, self, idx, true);
                            break State::Exception;
                        } else if opcode == OpCode::GET_VAR_REF_CHECK {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_VAR_REF_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        if unlikely(!captured_vars[idx as usize].value().is_uninitialized()) {
                            JS_ThrowReferenceErrorUninitialized2(realm_scope, self, idx, true);
                            break State::Exception;
                        } else {
                            captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                            SP_GO!(-1);
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Constructor / brand / throw
                    // ═══════════════════════════════════════════════════
                    OpCode::CHECK_CTOR_RETURN => {
                        if !SP_REF!(-1).is_object() {
                            if unlikely(!SP_REF!(-1).is_undefined()) {
                                realm_scope.throw_type_error(
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
                            realm_scope
                                .throw_type_error("class constructors must be invoked with 'new'");
                            break State::Exception;
                        }
                    }
                    OpCode::CHECK_BRAND => {
                        #[cfg(feature = "builtin_symbol")]
                        if let Ok(b) = SP_REF!(-2).check_brand(realm_scope, SP_VAL!(-1)) {
                            if unlikely(!b) {
                                realm_scope.throw_type_error("invalid brand on object");
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope.throw_internal_error("Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::ADD_BRAND => {
                        #[cfg(feature = "builtin_symbol")]
                        {
                            let mut func_obj = SP_VAL!(-2);
                            let home_object = SP_VAL!(-1);
                            SP_GO!(-2);
                            if unlikely(func_obj.add_brand(realm_scope, home_object).is_err()) {
                                break State::Exception;
                            }
                        }
                        #[cfg(not(feature = "builtin_symbol"))]
                        {
                            realm_scope
                                .throw_internal_error("can't add brand: Symbol is not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::THROW => {
                        SP_GO!(-1);
                        realm_scope.throw_value(SP_VAL!(0));
                        break State::Exception;
                    }
                    OpCode::THROW_ERROR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let type_ = PC_FETCH!(u8);
                        match type_ {
                            JS_THROW_VAR_RO => {
                                realm_scope.throw_type_error_read_only(atom);
                            }
                            JS_THROW_VAR_REDECL => {
                                JS_ThrowSyntaxErrorVarRedeclaration(realm_scope, atom);
                            }
                            JS_THROW_VAR_UNINITIALIZED => {
                                JS_ThrowReferenceErrorUninitialized(realm_scope, atom);
                            }
                            JS_THROW_ERROR_DELETE_SUPER => {
                                realm_scope
                                    .throw_reference_error("unsupported reference to 'super'");
                            }
                            JS_THROW_ERROR_ITERATOR_THROW => {
                                realm_scope
                                    .throw_type_error("iterator does not have a throw method");
                            }
                            _ => {
                                realm_scope.throw_internal_error(&format!(
                                    "invalid throw var type {type_}"
                                ));
                            }
                        }
                        break State::Exception;
                    }

                    // ═══════════════════════════════════════════════════
                    // Array / Apply
                    // ═══════════════════════════════════════════════════
                    OpCode::ARRAY_FROM => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        if let Ok(mut list) = alloc_fast_array_slots(realm_scope, call_argc as _) {
                            let call_argv = unsafe { sp.offset(-call_argc as isize) };
                            for i in 0..call_argc as usize {
                                let val = unsafe { *call_argv.add(i) };
                                list.push(val);
                            }
                            if let Ok(arr) = crate::array::Array::from_values(realm_scope, list) {
                                SP_GO!(-call_argc);
                                SP_PUT!(0, JSValue::from(arr));
                                SP_GO!(1);
                            } else {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::APPLY => {
                        let magic = PC_FETCH!(u16) as i32;
                        let op_3 = SP_VAL!(-3);
                        let op_2 = SP_VAL!(-2);
                        let op_1 = SP_VAL!(-1);
                        SP_GO!(-3);
                        if let Ok(res) = js_function_apply(
                            realm_scope,
                            CallContext::new(op_3, &[op_2, op_1], magic, None),
                        ) {
                            SP_PUT!(0, res);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Eval
                    // ═══════════════════════════════════════════════════
                    OpCode::EVAL => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let scope_idx = PC_FETCH!(u16) as i32 - 1;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        unsafe {
                            *cur_pc_offset_ptr = pc.offset_from(pc_base) as _;
                        }
                        let func_val = unsafe { *call_argv.offset(-1) };
                        let ret_val = if func_val.is_same(realm_scope.eval_func) {
                            if call_argc >= 1 {
                                let obj = unsafe { &*call_argv };
                                obj.eval_object(
                                    realm_scope,
                                    JSValue::UNDEFINED,
                                    EvalType::Direct,
                                    EvalFlags::empty(),
                                    scope_idx as _,
                                )
                            } else {
                                Ok(JSValue::UNDEFINED)
                            }
                        } else {
                            func_val.call_internal(
                                realm_scope,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                },
                                0,
                            )
                        };
                        SP_GO!(-(call_argc + 1));
                        if let Ok(v) = ret_val {
                            SP_PUT!(0, v);
                            SP_GO!(1);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::APPLY_EVAL => {
                        let var_scope_idx = PC_FETCH!(u16) as i32 - 1;
                        let func_val = SP_REF!(-2);
                        if let Ok(arg_list) = build_arg_list(realm_scope, SP_VAL!(-1)) {
                            let ret_val = {
                                if func_val.is_same(realm_scope.eval_func) {
                                    if !arg_list.is_empty() {
                                        arg_list[0].eval_object(
                                            realm_scope,
                                            JSValue::UNDEFINED,
                                            EvalType::Direct,
                                            EvalFlags::empty(),
                                            var_scope_idx,
                                        )
                                    } else {
                                        Ok(JSValue::UNDEFINED)
                                    }
                                } else {
                                    func_val.call(realm_scope, JSValue::UNDEFINED, &arg_list)
                                }
                            };
                            if let Ok(v) = ret_val {
                                SP_GO!(-2);
                                SP_PUT!(0, v);
                                SP_GO!(1);
                            } else {
                                break State::Exception;
                            }
                        } else {
                            break State::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Property access
                    // ═══════════════════════════════════════════════════
                    OpCode::LNOT => {
                        let op_1: JSValue = SP_VAL!(-1);
                        let b = match op_1 {
                            JSValue::Int(i) => i != 0,
                            JSValue::Bool(b) => b,
                            JSValue::NULL | JSValue::UNDEFINED => false,
                            _ => {
                                if let Ok(b) = op_1.to_bool() {
                                    b
                                } else {
                                    break State::Exception;
                                }
                            }
                        };
                        SP_PUT!(-1, JSValue::Bool(!b));
                    }
                    OpCode::GET_FIELD | OpCode::GET_FIELD2 => {
                        // get_field: [obj, ^] -> [value, ^]
                        // get_field2: [obj, ^] -> [obj, value, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));

                        if let Ok(val) = SP_REF!(-1).get_property(realm_scope, atom) {
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
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let obj = SP_VAL!(-2);
                        let value = SP_VAL!(-1);
                        SP_GO!(-2);

                        let r = obj.set_property_internal(
                            realm_scope,
                            atom,
                            value,
                            None,
                            JS_PROP_THROW_STRICT,
                        );
                        if unlikely(r.is_err()) {
                            break State::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Binary arithmetic
                    // ═══════════════════════════════════════════════════
                    OpCode::ADD
                    | OpCode::SUB
                    | OpCode::MUL
                    | OpCode::DIV
                    | OpCode::MOD
                    | OpCode::POW => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        let res = match opcode {
                            OpCode::ADD => JSValue::arith_add(realm_scope, op1, op2),
                            OpCode::SUB => JSValue::arith_sub(realm_scope, op1, op2),
                            OpCode::MUL => JSValue::arith_mul(realm_scope, op1, op2),
                            OpCode::DIV => JSValue::arith_div(realm_scope, op1, op2),
                            OpCode::MOD => JSValue::arith_mod(realm_scope, op1, op2),
                            OpCode::POW => JSValue::arith_pow(realm_scope, op1, op2),
                            _ => unreachable!(),
                        };

                        if let Ok(val) = res {
                            SP_PUT!(-2, val);
                            SP_GO!(-1);
                        } else {
                            break State::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Control flow
                    // ═══════════════════════════════════════════════════
                    OpCode::GOTO => {
                        let n = unsafe { read_from_ptr::<i32>(pc) }; // could be minus
                        PC_GO!(n);

                        if unlikely(realm_scope.poll_interrupts()) {
                            break State::Exception;
                        }
                    }
                    OpCode::IF_TRUE | OpCode::IF_FALSE => {
                        let b = opcode == OpCode::IF_TRUE;
                        let op_1 = SP_VAL!(-1);
                        SP_GO!(-1);

                        // skip jump offset
                        PC_GO!(4);

                        let test = match op_1 {
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

                        if b == test {
                            // jump pc
                            pc = unsafe {
                                pc.offset((read_from_ptr::<i32>(pc.offset(-4)) - 4) as isize)
                            };
                        }

                        if unlikely(realm_scope.poll_interrupts()) {
                            break State::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Array element access
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_ARRAY_EL => {
                        // [array, prop, ^] -> [value, ^]
                        let val = SP_REF!(-2)
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(-2, val);
                        SP_GO!(-1);

                        if unlikely(is_err) {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL2 => {
                        // [array, prop, ^] -> [array, value, ^]
                        let val = SP_REF!(-2)
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(-1, val);

                        if unlikely(is_err) {
                            break State::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL3 => {
                        // [array, prop, ^] -> [array, prop1, value, ^]
                        match SP_REF!(-2) {
                            JSValue::Int(_) | JSValue::String(_) | JSValue::Symbol(_) => {}
                            a => {
                                // must be tested before ToPropertyKey
                                if a.is_undefined_or_null() {
                                    realm_scope.throw_type_error("value has no property ");
                                    break State::Exception;
                                }
                                let Ok(prop1) = SP_REF!(-1).to_propkey(realm_scope) else {
                                    break State::Exception;
                                };
                                SP_PUT!(-1, prop1);
                            }
                        }

                        let val = SP_REF!(-2)
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(0, val);
                        SP_GO!(1);

                        if unlikely(is_err) {
                            break State::Exception;
                        }
                    }
                    OpCode::PUT_ARRAY_EL => {
                        // [arr, prop, value, ^] -> [^]
                        let mut arr = SP_VAL!(-3);
                        let ret = arr.set_property_value(
                            realm_scope,
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                            JS_PROP_THROW_STRICT,
                        );
                        SP_GO!(-3);
                        if unlikely(ret.is_err()) {
                            break State::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Misc
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_SUPER => {
                        if let Ok(proto) = SP_REF!(-1).prototype(realm_scope) {
                            SP_PUT!(-1, proto);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::IMPORT => {
                        if let Ok(res) = js_dynamic_import(realm_scope, SP_VAL!(-1)) {
                            SP_PUT!(-1, res);
                        } else {
                            break State::Exception;
                        }
                    }
                    OpCode::REGEXP => {
                        #[cfg(feature = "builtin_regexp")]
                        if let JSValue::Int(re_id) = SP_VAL!(-1) {
                            let re = Box::new(
                                realm_scope.rt().regexp_precompiled[re_id as usize].clone(),
                            );
                            let regexp = crate::builtins::regexp::RegExp::from(
                                realm_scope,
                                JSValue::UNDEFINED,
                                re,
                            )
                            .map_or(JSValue::EXCEPTION, |o| o.into());
                            SP_PUT!(-2, regexp);
                            SP_GO!(-1);
                        }
                        #[cfg(not(feature = "builtin_regexp"))]
                        {
                            realm_scope.throw_internal_error("regex not supported");
                            break State::Exception;
                        }
                    }
                    OpCode::NOP => {}

                    // ═══════════════════════════════════════════════════
                    // Try-catch
                    // ═══════════════════════════════════════════════════
                    OpCode::CATCH => {
                        let diff = unsafe { read_from_ptr::<i32>(pc) };
                        SP_PUT!(
                            0,
                            JSValue::CatchOffset(pc.offset_from(pc_base) as i32 + diff)
                        );
                        SP_GO!(1);
                        PC_GO!(4);
                    }
                    OpCode::NIP_CATCH => {
                        // [catch_offset, ..., ret_val, ^] -> [ret_eval, ^]
                        SP_GO!(-1);
                        // keep ret_val
                        let ret_val = SP_VAL!(0);
                        // drop ... values
                        while sp > sp_base && !matches!(SP_REF!(-1), JSValue::CatchOffset(_)) {
                            SP_GO!(-1);
                        }
                        if unlikely(sp == sp_base) {
                            realm_scope.throw_internal_error("nip_catch");
                            break State::Exception;
                        } else {
                            SP_PUT!(-1, ret_val);
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // FCLOSURE
                    // ═══════════════════════════════════════════════════
                    OpCode::FCLOSURE => {
                        // [^] -> [func_obj, ^]
                        let id = PC_FETCH!(u32);

                        if let JSValue::Bytecode(bc) = CPOOL![id] {
                            let mut abort = false;
                            let captures: Vec<VarRef> = if bc.closure_var_count != 0 {
                                let mut lst = Vec::new();
                                if lst
                                    .try_reserve_exact(bc.closure_var_count as usize)
                                    .is_err()
                                {
                                    realm_scope.throw_out_of_memory();
                                    break State::Exception;
                                }
                                for cv in bc.closure_vars() {
                                    lst.push(if cv.is_local() {
                                        match arena_get_var_ref(
                                            realm_scope,
                                            cv.var_idx,
                                            cv.is_arg(),
                                            arg_base_ptr,
                                            var_base_ptr,
                                            &mut arena.frame_mut(cur_frame_idx).var_ref_list,
                                            self.arg_count,
                                            self.var_count,
                                            mode,
                                        ) {
                                            Ok(vr) => vr,
                                            Err(_) => {
                                                abort = true;
                                                break;
                                            }
                                        }
                                    } else {
                                        captured_vars[cv.var_idx as usize]
                                    });
                                }
                                lst
                            } else {
                                Vec::new()
                            };
                            if abort {
                                break State::Exception;
                            }

                            let p = BFuncPayload::new(bc, captures);
                            let isa = match bc.func_kind() {
                                FunctionKind::Normal => ObjectPayload::BYTECODE_FUNCTION(p),
                                FunctionKind::Generator => ObjectPayload::GENERATOR_FUNCTION(p),
                                FunctionKind::Async => ObjectPayload::ASYNC_FUNCTION(p),
                                FunctionKind::AsyncGenerator => {
                                    ObjectPayload::ASYNC_GENERATOR_FUNCTION(p)
                                }
                            };
                            let mut obj = match realm_scope.create_instance_with_proto_payload(
                                realm_scope.class_proto(isa.class_id()),
                                isa,
                            ) {
                                Ok(o) => o,
                                Err(_) => {
                                    break State::Exception;
                                }
                            };

                            js_function_set_properties(
                                realm_scope,
                                obj,
                                if bc.func_name.is_null() {
                                    Atom::CONST_empty_string
                                } else {
                                    bc.func_name
                                },
                                bc.attrs.defined_arg_count as _,
                            );

                            if let Some(gen_class_id) = bc.func_kind().generator_class_id() {
                                let proto = match realm_scope.create_base_object_with_proto(
                                    realm_scope.class_proto(gen_class_id),
                                ) {
                                    Ok(p) => p,
                                    Err(_) => {
                                        break State::Exception;
                                    }
                                };
                                if obj
                                    .define_property_value(
                                        realm_scope,
                                        Atom::CONST_prototype,
                                        proto.into(),
                                        JS_PROP_WRITABLE,
                                    )
                                    .is_err()
                                {
                                    break State::Exception;
                                }
                            } else if bc.has_prototype() {
                                obj.set_flag_constructor(true);
                                if obj
                                    .define_lazy_property(
                                        realm_scope,
                                        Atom::CONST_prototype,
                                        LazyInit::Prototype,
                                        JS_PROP_WRITABLE,
                                    )
                                    .is_err()
                                {
                                    break State::Exception;
                                }
                            }

                            let r = JSValue::from(obj);
                            SP_PUT!(0, r);
                            SP_GO!(1);
                            if r.is_exception() {
                                break State::Exception;
                            }
                        } else {
                            unreachable!()
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Generator/Async suspension and return
                    // ═══════════════════════════════════════════════════
                    OpCode::AWAIT => {
                        break State::Done(JSValue::Int(FUNC_RET_AWAIT), true);
                    }
                    OpCode::YIELD => {
                        break State::Done(JSValue::Int(FUNC_RET_YIELD), true);
                    }
                    OpCode::YIELD_STAR | OpCode::ASYNC_YIELD_STAR => {
                        break State::Done(JSValue::Int(FUNC_RET_YIELD_STAR), true);
                    }
                    OpCode::INITIAL_YIELD => {
                        break State::Done(JSValue::Int(FUNC_RET_INITIAL_YIELD), true);
                    }
                    OpCode::RETURN_ASYNC => {
                        break State::Done(JSValue::UNDEFINED, true);
                    }

                    // ═══════════════════════════════════════════════════
                    // Subroutine call (try-finally)
                    // ═══════════════════════════════════════════════════
                    OpCode::GOSUB => {
                        let diff = unsafe { read_from_ptr::<i32>(pc) };
                        // Push return address as Int (offset to instruction after GOSUB)
                        SP_PUT!(0, JSValue::Int(pc.offset_from(pc_base) as i32 + 4));
                        SP_GO!(1);
                        PC_GO!(diff);
                    }
                    OpCode::RET => {
                        if let JSValue::Int(i) = SP_REF!(-1) {
                            let pos = *i;
                            if unlikely(pos >= bytecodes.len() as i32) {
                                realm_scope.throw_internal_error("invalid ret value");
                                break State::Exception;
                            } else {
                                SP_GO!(-1);
                                pc = unsafe { pc_base.offset(pos as isize) };
                            }
                        } else {
                            realm_scope.throw_internal_error("invalid ret value");
                            break State::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Unimplemented: with, short_opcode, etc.
                    // ═══════════════════════════════════════════════════
                    _ => {
                        // TODO: Port remaining opcodes from execute_with_realm_scope.
                        realm_scope
                            .throw_internal_error("unimplemented opcode in execute_with_arena");
                        break State::Exception;
                    }
                } // match opcode

                #[cfg(all(debug_assertions, feature = "debug_gc"))]
                {
                    realm_scope.clear_locals();
                    rt.gc_collect();
                }
                #[cfg(not(all(debug_assertions, feature = "debug_gc")))]
                if rt.gc_req {
                    realm_scope.clear_locals();
                    rt.gc_collect();
                }
            }; // opcodes inner loop

            // ── State dispatch ──
            match state {
                State::Done(ret_val, is_generator) => {
                    unsafe {
                        *cur_pc_offset_ptr = pc.offset_from(pc_base) as usize;
                        *cur_sp_offset_ptr = sp.offset_from(sp_base) as i32;
                    }
                    return ExecAction::Return(ret_val, is_generator);
                }

                State::Exception => {
                    // Interrupt bypasses catch handling.
                    if realm_scope.rt().interrupt_code.is_some() {
                        return ExecAction::Unwind;
                    }
                    if let Some((new_pc, new_sp)) =
                        arena.find_catch_in_frame(cur_frame_idx, realm_scope)
                    {
                        unsafe {
                            *cur_sp_offset_ptr = new_sp as i32;
                            pc = pc_base.add(new_pc);
                            sp = sp_base.add(new_sp);
                        }
                        continue 'arena_loop;
                    }
                    return ExecAction::Unwind;
                }

                State::CallBytecode {
                    callee_bc,
                    callee_frame_idx,
                    return_sp,
                } => {
                    unsafe {
                        *cur_pc_offset_ptr = pc.offset_from(pc_base) as usize;
                        *cur_sp_offset_ptr = sp.offset_from(sp_base) as i32;
                    }
                    return ExecAction::CallBytecode {
                        callee_bc,
                        callee_frame_idx,
                        return_sp,
                    };
                }

                State::TailCall {
                    callee_bc,
                    callee_func_obj,
                    callee_buf_cap,
                } => {
                    return ExecAction::TailCall {
                        callee_bc,
                        callee_func_obj,
                        callee_buf_cap,
                    };
                }
            }
        } // 'arena_loop
    }
}

// ============================================================================
// Top-Level Trampoline
// ============================================================================

/// Execute bytecode in a single trampoline loop, completely avoiding
/// C-stack recursion for JS-to-JS calls.
///
/// The trampoline maintains:
/// - `cur_bc`: currently executing bytecode
/// - `cur_frame_idx`: currently executing frame in the arena
/// - `cur_scope`: GC scope for the current frame
/// - `virtual_frames`: caller chain (saved on call, restored on return)
///
/// Returns (result, is_completed) tuple:
/// - result: the return value (or EXCEPTION on uncaught error)
/// - is_completed: true if the function completed, false if suspended (yield/await)
pub(crate) fn execute_trampoline(
    start_bc: Bytecode,
    start_frame_idx: FrameIndex,
    start_scope: &mut ScopedContext,
    this_obj: JSValue,
    new_target: JSValue,
    args: &[JSValue],
    captured_vars: &mut [VarRef],
) -> (JSValue, bool) {
    use crate::stack_frame::VirtualFrame;
    use std::mem::ManuallyDrop;

    // Get a stable raw pointer to the arena. The arena lives in RuntimeImpl
    // which has the same lifetime as the runtime. Using a raw pointer avoids
    // borrow conflicts between cur_scope (which borrows rt) and arena access.
    let arena_ptr: *mut StackFrameArena = {
        let rt = start_scope.rt_mut();
        &raw mut rt.frame_arena
    };
    let mut rt_ptr = start_scope.rt_ptr();

    let mut cur_bc = start_bc;
    let mut cur_frame_idx = start_frame_idx;
    let mut virtual_frames: Vec<VirtualFrame> = Vec::new();

    // realm_ctx provides the factory for new GC scopes.
    // We transmute scopes to 'static because VirtualFrame stores them
    // with ManuallyDrop — the actual lifetime is managed by the trampoline
    // loop which restores and drops each scope before the realm expires.
    let mut realm_ctx = start_bc.realm();
    let mut cur_scope: ScopedContext<'static> =
        unsafe { std::mem::transmute(realm_ctx.new_scope()) };

    // Initialize PC/SP offsets from the frame head.
    // On first execution, both are 0 (fresh frame).
    // On resume after yield/await, they hold the saved position
    // from execute_with_arena's State::Done handler.
    let (mut pc_offset, mut sp_offset) = {
        let arena = unsafe { &*arena_ptr };
        let head = arena.frame(start_frame_idx);
        (head.cur_pc_offset, head.cur_sp_offset as usize)
    };
    // Check for a pending runtime exception (e.g., .throw() on a suspended
    // async generator or async function). If present, the first
    // execute_with_arena call must handle it immediately.
    let mut throwed = cur_scope.rt().exception() != Exception::No;

    'trampoline: loop {
        let arena = unsafe { &mut *arena_ptr };

        rt_ptr.active_trampoline_frame = cur_frame_idx;

        let action = cur_bc.execute_with_arena(
            &mut cur_scope,
            arena,
            cur_frame_idx,
            this_obj,
            new_target,
            None, // func_home_obj
            pc_offset,
            sp_offset,
            captured_vars,
            args,
            throwed,
        );

        match action {
            // ── Call sub-frame ──
            ExecAction::CallBytecode {
                callee_bc,
                callee_frame_idx: callee_idx,
                return_sp,
            } => {
                // Create callee's GC scope first, then swap to avoid
                // zero-initializing ScopedContext (which contains NonNull).
                // SAFETY: lifetime erased to 'static; trampoline restores
                // and drops all scopes before realm_ctx expires.
                let callee_scope: ScopedContext<'static> =
                    unsafe { std::mem::transmute(realm_ctx.new_scope()) };

                let caller_scope =
                    ManuallyDrop::new(std::mem::replace(&mut cur_scope, callee_scope));

                virtual_frames.push(VirtualFrame {
                    frame_idx: cur_frame_idx,
                    caller_bc: cur_bc,
                    return_sp,
                    caller_scope,
                    caller_ctx_ptr: None,
                });

                cur_bc = callee_bc;
                cur_frame_idx = callee_idx;
                pc_offset = 0;
                sp_offset = 0;
                throwed = false;
            }

            // ── Tail call ──
            ExecAction::TailCall {
                callee_bc,
                callee_func_obj,
                callee_buf_cap,
            } => {
                let arena = unsafe { &mut *arena_ptr };
                arena.free_frame(cur_frame_idx);
                let new_idx = arena.alloc_frame(callee_func_obj, callee_bc.mode(), callee_buf_cap);
                {
                    let head = arena.frame_mut(new_idx);
                    head.arg_base_offset = 0;
                    head.var_base_offset = head.arg_count as u32;
                    head.var_count = callee_bc.var_count;
                    head.cur_sp_offset = 0;
                }

                cur_bc = callee_bc;
                cur_frame_idx = new_idx;
                pc_offset = 0;
                sp_offset = 0;
                throwed = false;
            }

            // ── Frame return ──
            ExecAction::Return(ret_val, is_generator) => {
                if is_generator {
                    if ret_val.is_undefined() {
                        // RETURN_ASYNC: async function completed normally.
                        // The actual return value is at SP[-1] in the frame.
                        let arena = unsafe { &mut *arena_ptr };
                        // Extract stack pointer and read return value.
                        let sp_offset = {
                            let head = arena.frame(cur_frame_idx);
                            head.cur_sp_offset
                        };
                        let sp_base = {
                            let head = arena.frame(cur_frame_idx);
                            arena.stack_base_ptr(head)
                        };
                        let actual_ret =
                            unsafe { std::ptr::read(sp_base.offset(sp_offset as isize - 1)) };
                        // Detach all VarRefs before freeing the frame. The
                        // frame may hold closure variables captured by inner
                        // arrows (e.g., `new.target` in `async () => new.target`).
                        let var_refs = &mut arena.frame_mut(cur_frame_idx).var_ref_list;
                        for vr in var_refs {
                            vr.detach();
                        }
                        arena.free_frame(cur_frame_idx);
                        drop(cur_scope);
                        // Drain virtual_frames to drop saved caller scopes.
                        while let Some(vf) = virtual_frames.pop() {
                            drop(ManuallyDrop::into_inner(vf.caller_scope));
                        }
                        rt_ptr.active_trampoline_frame = FrameIndex::NULL;

                        return (actual_ret, true);
                    }

                    // YIELD/AWAIT/YIELD_STAR: function suspended.
                    // Frame stays Active in arena; caller must take_frame.
                    drop(cur_scope);

                    // Drain virtual_frames to drop saved caller scopes.
                    // These were placed in ManuallyDrop during CallBytecode.
                    // On early return (yield/await), we must explicitly drain
                    // them to prevent GC scope stack corruption.
                    while let Some(vf) = virtual_frames.pop() {
                        drop(ManuallyDrop::into_inner(vf.caller_scope));
                    }
                    rt_ptr.active_trampoline_frame = FrameIndex::NULL;
                    return (ret_val, false);
                }

                let arena = unsafe { &mut *arena_ptr };
                arena.free_frame(cur_frame_idx);
                drop(cur_scope);

                if let Some(caller_vf) = virtual_frames.pop() {
                    cur_bc = caller_vf.caller_bc;
                    cur_frame_idx = caller_vf.frame_idx;
                    cur_scope = ManuallyDrop::into_inner(caller_vf.caller_scope);

                    let caller_head = arena.frame(caller_vf.frame_idx);
                    let sp_ptr = arena.stack_base_ptr(caller_head);
                    unsafe {
                        *sp_ptr.add(caller_vf.return_sp) = ret_val;
                    }

                    // Restore PC and adjust SP to match legacy CALL handler:
                    //   SP_GO!(-(call_argc + 1)) + SP_GO!(1)
                    // After call returns: sp = old_sp - call_argc
                    // return_sp = old_sp - call_argc - 1
                    // So: new_sp = return_sp + 1
                    pc_offset = caller_head.cur_pc_offset;
                    sp_offset = caller_vf.return_sp + 1;
                    throwed = false;
                } else {
                    rt_ptr.active_trampoline_frame = FrameIndex::NULL;
                    return (ret_val, true);
                }
            }

            // ── Exception propagation ──
            ExecAction::Unwind => {
                let arena = unsafe { &mut *arena_ptr };
                arena.free_frame(cur_frame_idx);
                drop(cur_scope);

                if let Some(caller_vf) = virtual_frames.pop() {
                    cur_scope = ManuallyDrop::into_inner(caller_vf.caller_scope);
                    cur_bc = caller_vf.caller_bc;
                    cur_frame_idx = caller_vf.frame_idx;

                    let caller_head = arena.frame(caller_vf.frame_idx);
                    pc_offset = caller_head.cur_pc_offset;
                    sp_offset = caller_head.cur_sp_offset as usize;
                    throwed = true;

                    continue 'trampoline;
                } else {
                    rt_ptr.active_trampoline_frame = FrameIndex::NULL;
                    return (JSValue::EXCEPTION, true);
                }
            }

            // ── Cross-realm call (deferred; falls through to error for now) ──
            ExecAction::CallBytecodeCrossRealm { .. } => {
                // Cross-realm call handling is deferred. For initial
                // implementation, bytecode cross-realm detection in
                // execute_with_arena produces CallBytecode instead.
                unreachable!("CallBytecodeCrossRealm not yet implemented");
            }
        }
    }
}
