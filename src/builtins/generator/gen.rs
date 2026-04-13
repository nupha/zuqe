// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    Atom, Callback, ClassID, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION,
    context::ScopedContext,
    error::JSResult,
    function::{
        AsyncState, BytecodeFunction, CallContext, FieldDef, FunctionKind, IteratorState,
        NativeFunction,
    },
    object::{JS_PROP_CONFIGURABLE, Object, ObjectImpl, ObjectPayload},
    object_cast,
    stack_frame::StackFrame,
    value::JSValue,
};

pub(crate) const GEN_MAGIC_NEXT: i32 = 0;
pub(crate) const GEN_MAGIC_RETURN: i32 = 1;
pub(crate) const GEN_MAGIC_THROW: i32 = 2;

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum GenState {
    JS_GENERATOR_STATE_SUSPENDED_START,
    JS_GENERATOR_STATE_SUSPENDED_YIELD,
    JS_GENERATOR_STATE_SUSPENDED_YIELD_STAR,
    JS_GENERATOR_STATE_EXECUTING,
    JS_GENERATOR_STATE_COMPLETED,
}

pub const FUNC_RET_AWAIT: i32 = 0;
pub const FUNC_RET_YIELD: i32 = 1;
pub const FUNC_RET_YIELD_STAR: i32 = 2;
pub const FUNC_RET_INITIAL_YIELD: i32 = 3;

#[derive(Debug)]
pub struct GeneratorPayload {
    state: GenState,
    async_state: AsyncState,
}

impl GcTrace for GeneratorPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.async_state.trace(tr);
    }
}

impl GeneratorPayload {
    #[inline(always)]
    const fn state(&self) -> GenState {
        self.state
    }

    #[inline(always)]
    const fn set_state(&mut self, state: GenState) {
        self.state = state;
    }

    fn set_throw_flag(&mut self, b: bool) {
        self.async_state.set_throw_flag(b);
    }

    #[inline(always)]
    pub fn is_completed(&self) -> bool {
        self.async_state.is_completed
    }

    /// run with stack frame
    #[inline(always)]
    fn with_stack_frame_mut<R>(&mut self, f: impl FnOnce(&mut StackFrame) -> R) -> R {
        f(unsafe { self.async_state.stack_frame.as_mut().get_unchecked_mut() })
    }

    /// execute the function up to 'OP_initial_yield' opcode
    #[inline(always)]
    pub fn resume(&mut self, ctx: &mut ScopedContext) -> JSResult {
        self.async_state.resume(ctx)
    }

    /// mark generator is completed
    pub fn complete(&mut self) {
        if self.state != GenState::JS_GENERATOR_STATE_COMPLETED {
            self.set_state(GenState::JS_GENERATOR_STATE_COMPLETED);
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct GeneratorFunction(GcRef<ObjectImpl>);

object_cast!(GeneratorFunction, JS_CLASS_GENERATOR_FUNCTION);

impl GeneratorFunction {
    pub fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        debug_assert!(ctx.class_slots[class_id as usize].proto.is_none());

        match class_id {
            JS_CLASS_GENERATOR => {
                let iter_proto = ctx.iterator_proto()?;
                let proto = ctx.create_base_object_with_proto(Some(iter_proto))?;
                ctx.init_class_proto_internal(class_id, proto, js_generator_proto_funcs)
            }
            JS_CLASS_GENERATOR_FUNCTION => {
                let mut gen_func_proto =
                    ctx.create_base_object_with_proto(Some(ctx.function_proto()))?;
                gen_func_proto.install_fields(ctx, js_generator_function_proto_funcs)?;
                ctx.class_slots[class_id as usize].proto = Some(gen_func_proto);

                let gen_proto = if let Some(o) = ctx.class_proto(JS_CLASS_GENERATOR) {
                    o
                } else {
                    // init AsyncGenerator first
                    const {
                        debug_assert!(JS_CLASS_GENERATOR_FUNCTION > JS_CLASS_GENERATOR);
                    }
                    Self::class_init(ctx, JS_CLASS_GENERATOR)?;
                    ctx.class_proto(JS_CLASS_GENERATOR).unwrap()
                };
                ctx.bind_ctor_proto_ex(
                    gen_func_proto,
                    gen_proto,
                    JS_PROP_CONFIGURABLE,
                    JS_PROP_CONFIGURABLE,
                )?;

                let gen_func_ctor = NativeFunction::create(
                    ctx,
                    Callback::ConstructorOrFunc(BytecodeFunction::class_constructor),
                    Atom::CONST_GeneratorFunction,
                    1,
                    FunctionKind::Generator as i16,
                    ctx.function_ctor(),
                    None,
                )?;
                ctx.bind_ctor_proto_ex(
                    gen_func_ctor.into(),
                    gen_func_proto,
                    0,
                    JS_PROP_CONFIGURABLE,
                )?;
                ctx.class_slots[JS_CLASS_GENERATOR_FUNCTION as usize].ctor =
                    Some(gen_func_ctor.into());

                Ok(())
            }
            _ => {
                unreachable!();
            }
        }
    }
}

pub(super) fn js_generator_function_call(
    ctx: &mut ScopedContext,
    func_obj: JSValue,
    this_obj: JSValue,
    args: &[JSValue],
    _flags: i32,
) -> JSResult {
    let async_state = AsyncState::new(
        ctx,
        [const { JSValue::UNDEFINED }; 2],
        *func_obj.as_object_BCWrapper().unwrap(),
        this_obj,
        args,
    )?;
    let mut p = Box::new(GeneratorPayload {
        state: GenState::JS_GENERATOR_STATE_SUSPENDED_START,
        async_state,
    });

    // initial execute bytecodes till OpCode::INITIAL_YIELD instruction
    p.resume(ctx)?;

    // create Generator instance
    ctx.create_object_from_ctor(func_obj, ObjectPayload::GENERATOR(p))
        .map(JSValue::Object)
        .map_err(|_| -1)
}

fn js_generator_next(
    ctx: &mut ScopedContext,
    cc: CallContext,
    iter_state: &mut IteratorState,
) -> JSResult {
    let magic = cc.magic();

    *iter_state = IteratorState::Done;
    let mut this = cc.this();

    if let ObjectPayload::GENERATOR(g) = this.retrive_object_payload_mut(ctx, JS_CLASS_GENERATOR)? {
        enum WhatToDo {
            ExecNoArg,
            Done,
        }

        let action: WhatToDo = match g.state() {
            GenState::JS_GENERATOR_STATE_SUSPENDED_START => {
                if magic == GEN_MAGIC_NEXT {
                    g.set_throw_flag(false);
                    WhatToDo::ExecNoArg
                } else {
                    WhatToDo::Done
                }
            }
            GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD_STAR
            | GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD => {
                // SP[-1] was set to JS_UNDEFINED in the previous call
                let val = cc.arg(0);

                if magic == GEN_MAGIC_THROW
                    && g.state() == GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD
                {
                    ctx.throw(val);
                    g.set_throw_flag(true);
                } else {
                    g.with_stack_frame_mut(|sf| unsafe {
                        let sp_ptr = sf.stack_cur_ptr();
                        debug_assert_eq!(
                            *sp_ptr.offset(-1).as_ptr(),
                            JSValue::UNDEFINED,
                            "SP[-1] was set to JS_UNDEFINED in the previous call"
                        );

                        std::ptr::replace(sp_ptr.offset(-1).as_ptr(), val);
                        std::ptr::replace(sp_ptr.as_ptr(), JSValue::Int(magic));
                        sf.cur_sp_offset += 1; // move sp pointer
                    });

                    g.set_throw_flag(false);
                }
                WhatToDo::ExecNoArg
            }
            GenState::JS_GENERATOR_STATE_COMPLETED => WhatToDo::Done,
            GenState::JS_GENERATOR_STATE_EXECUTING => {
                ctx.throw_type_error("cannot invoke a running generator");
                return Err(-1);
            }
        };

        match action {
            WhatToDo::ExecNoArg => {
                g.set_state(GenState::JS_GENERATOR_STATE_EXECUTING);
                let res = g.resume(ctx);
                g.set_state(GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD);

                if g.is_completed() {
                    /* finalize the execution in case of exception or normal return */
                    g.complete();
                    return res;
                }

                debug_assert!(res.unwrap().is_int());

                // get the returned yield value at the top of the stack
                let ret_val = g.with_stack_frame_mut(|sf| unsafe {
                    std::ptr::replace(sf.stack_cur_ptr().offset(-1).as_ptr(), JSValue::UNDEFINED)
                });

                *iter_state = if let Ok(JSValue::Int(FUNC_RET_YIELD_STAR)) = res {
                    g.set_state(GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD_STAR);
                    /* return (value, done) object */
                    IteratorState::NotParsed
                } else {
                    IteratorState::NotDone
                };

                ret_val.err()
            }
            WhatToDo::Done => {
                // execution is finished, mark generator as completed
                g.complete();

                match magic {
                    GEN_MAGIC_NEXT => Ok(JSValue::UNDEFINED),
                    GEN_MAGIC_RETURN => Ok(cc.arg(0)),
                    GEN_MAGIC_THROW => {
                        ctx.throw(cc.arg(0));
                        Err(-1)
                    }
                    _ => {
                        unreachable!()
                    }
                }
            }
        }
    } else {
        unreachable!()
    }
}

const js_generator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 1, js_generator_next, GEN_MAGIC_NEXT as _),
    FieldDef::ITERATOR_NEXT("return", 1, js_generator_next, GEN_MAGIC_RETURN as _),
    FieldDef::ITERATOR_NEXT("throw", 1, js_generator_next, GEN_MAGIC_THROW as _),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "Generator", JS_PROP_CONFIGURABLE),
];

const js_generator_function_proto_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "GeneratorFunction",
        JS_PROP_CONFIGURABLE,
    ),
];
