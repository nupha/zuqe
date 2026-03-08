// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_GENERATOR, ScopedContext,
    context::{Context, ContextImpl},
    error::JSResult,
    function::{AsyncState, CallContext, FieldDef, IteratorState},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
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

pub(crate) fn js_generator_function_call(
    ctx: &mut ScopedContext,
    func_obj: JSValue,
    this_obj: JSValue,
    args: &[JSValue],
    _flags: i32,
) -> JSResult {
    let async_state = AsyncState::create(
        ctx.into(),
        [const { JSValue::UNDEFINED }; 2],
        *func_obj.as_object_BCWrapper().unwrap(),
        this_obj,
        args,
    )?;
    let mut p = Box::new(GeneratorPayload {
        state: GenState::JS_GENERATOR_STATE_SUSPENDED_START,
        async_state,
    });

    // protect async_state before .resume() executes bytecode.
    // ctx.gc_heap_mut().protect_node_v2(async_state.gc_node_ptr());

    // initial execute bytecodes till OpCode::INITIAL_YIELD instruction
    p.resume(ctx)?;

    // create Generator instance
    ctx.create_object_from_ctor(func_obj, ObjectPayload::GENERATOR(p))
        .map(JSValue::Object)
        .map_err(|_| -1)
}

pub fn js_generator_next(
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
                // execution is finished
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

pub const js_generator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 1, js_generator_next, GEN_MAGIC_NEXT as _),
    FieldDef::ITERATOR_NEXT("return", 1, js_generator_next, GEN_MAGIC_RETURN as _),
    FieldDef::ITERATOR_NEXT("throw", 1, js_generator_next, GEN_MAGIC_THROW as _),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "Generator", JS_PROP_CONFIGURABLE),
];

pub const js_generator_function_proto_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "GeneratorFunction",
        JS_PROP_CONFIGURABLE,
    ),
];
