// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Atom, Callback, ClassID, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION,
    context::ScopedContext,
    error::JSResult,
    function::{
        AsyncState, BytecodeFunction, CallContext, FieldDef, FunctionKind, IteratorState,
        NativeFunction,
    },
    object::{GeneratorFunction, JS_PROP_CONFIGURABLE, ObjectPayload},
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

    #[inline(always)]
    pub fn is_completed(&self) -> bool {
        self.async_state.is_completed
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
                ctx.set_class_proto(class_id, gen_func_proto);

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
        func_obj.as_object_BCWrapper().unwrap(),
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
    ctx.ordinary_create_from_constructor(func_obj, ObjectPayload::GENERATOR(p))
        .map(JSValue::from)
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

    let g: &mut GeneratorPayload = this.class_payload_mut(ctx, JS_CLASS_GENERATOR)?;

    enum WhatToDo {
        ExecNoArg,
        Done,
    }

    let action: WhatToDo = match g.state() {
        GenState::JS_GENERATOR_STATE_SUSPENDED_START => {
            if magic == GEN_MAGIC_NEXT {
                g.async_state.set_throw_flag(false);
                WhatToDo::ExecNoArg
            } else {
                WhatToDo::Done
            }
        }
        GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD_STAR
        | GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD => {
            // SP[-1] was set to UNDEFINED in the previous call
            let val = cc.arg(0);

            if magic == GEN_MAGIC_THROW && g.state() == GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD
            {
                let _ = ctx.throw_value(val);
                g.async_state.set_throw_flag(true);
            } else {
                // Read current SP offset to verify SP[-1] is UNDEFINED.
                {
                    let arena = ctx.rt_mut().stack_frame_arena_mut();
                    let sp_off = g.async_state.cur_sp_offset(arena);
                    debug_assert!(sp_off > 0);

                    let head = g.async_state.frame(arena);
                    let sp_base = head.stack_base_ptr();
                    debug_assert_eq!(
                        unsafe { *sp_base.add(sp_off as usize - 1) },
                        JSValue::UNDEFINED,
                        "SP[-1] was set to UNDEFINED in the previous call"
                    );
                }
                // Write val to SP[-1], magic to SP[0], then advance SP by 1.
                {
                    let arena = ctx.rt_mut().stack_frame_arena_mut();
                    g.async_state.stack_store_value(arena, -1, val);
                    g.async_state
                        .stack_store_value(arena, 0, JSValue::from(magic));
                    g.async_state.bump_sp(arena, 1);
                }

                g.async_state.set_throw_flag(false);
            }
            WhatToDo::ExecNoArg
        }
        GenState::JS_GENERATOR_STATE_COMPLETED => WhatToDo::Done,
        GenState::JS_GENERATOR_STATE_EXECUTING => {
            return Err(ctx.throw_type_error("cannot invoke a running generator"));
        }
    };

    match action {
        WhatToDo::ExecNoArg => {
            g.set_state(GenState::JS_GENERATOR_STATE_EXECUTING);
            let res = g.resume(ctx);
            g.set_state(GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD);

            if g.is_completed() {
                // finalize the execution in case of exception or normal return
                g.complete();
                return res;
            }

            debug_assert!(res.unwrap().is_int());

            // Get the returned yield value at the top of the stack.
            // Uses take_top which replaces SP[-1] with UNDEFINED and
            // protects the GC node in the current scope.
            let ret_val = {
                let arena = ctx.rt().stack_frame_arena();
                g.async_state.take_top_value(arena, ctx)
            };

            *iter_state = if let Ok(Some(FUNC_RET_YIELD_STAR)) = res.map(|v| v.as_int()) {
                g.set_state(GenState::JS_GENERATOR_STATE_SUSPENDED_YIELD_STAR);
                IteratorState::NotParsed // should return (value, done) object
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
                GEN_MAGIC_THROW => Err(ctx.throw_value(cc.arg(0))),
                _ => {
                    unreachable!()
                }
            }
        }
    }
}

const js_generator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 1, js_generator_next, GEN_MAGIC_NEXT as _),
    FieldDef::ITERATOR_NEXT("return", 1, js_generator_next, GEN_MAGIC_RETURN as _),
    FieldDef::ITERATOR_NEXT("throw", 1, js_generator_next, GEN_MAGIC_THROW as _),
    FieldDef::STR("[Symbol.toStringTag]", "Generator", JS_PROP_CONFIGURABLE),
];

const js_generator_function_proto_funcs: &[FieldDef] = &[FieldDef::STR(
    "[Symbol.toStringTag]",
    "GeneratorFunction",
    JS_PROP_CONFIGURABLE,
)];
