use gc_lite::Collect;

use crate::{
    JS_CLASS_GENERATOR,
    builtins::iterator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW},
    context::{ContextImpl, ContextPtr},
    error::JSResult,
    function::{AsyncFuncState, CallContext, FieldDef, IteratorState},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    stack_frame::StackFrame,
    value::JSValue,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
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
    func_state: AsyncFuncState,
}

unsafe impl Collect for GeneratorPayload {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.func_state.trace(tracer);
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
        self.func_state.set_throw_flag(b);
    }

    #[inline(always)]
    pub fn is_completed(&self) -> bool {
        self.func_state.is_completed
    }

    /// run with stack frame
    #[inline(always)]
    fn with_stack_frame_mut<R>(&mut self, f: impl FnOnce(&mut StackFrame) -> R) -> R {
        f(unsafe { self.func_state.frame.as_mut().get_unchecked_mut() })
    }

    /// execute the function up to 'OP_initial_yield' opcode
    #[inline(always)]
    pub fn resume(&mut self, ctx: &ContextImpl) -> JSResult {
        self.func_state.resume(ctx)
    }

    /// mark generator is completed
    pub fn complete(&mut self) {
        if self.state != GenState::JS_GENERATOR_STATE_COMPLETED {
            self.set_state(GenState::JS_GENERATOR_STATE_COMPLETED);
        }
    }
}

pub fn js_generator_function_call(
    ctx: ContextPtr,
    func_obj: JSValue,
    this_obj: JSValue,
    args: &[JSValue],
    _flags: i32,
) -> JSResult {
    let func_state = AsyncFuncState::new(
        &ctx,
        *func_obj.as_object_BCFunction().unwrap(),
        this_obj,
        args,
    )?;
    let mut g = Box::new(GeneratorPayload {
        state: GenState::JS_GENERATOR_STATE_SUSPENDED_START,
        func_state,
    });

    // execute bytecodes up to 'OP_initial_yield'
    g.resume(&ctx)?;

    // create js object of generator class
    ctx.create_object_from_ctor(func_obj, ObjectPayload::GENERATOR(g))
        .map(JSValue::Object)
        .map_err(|_| -1)
}

pub fn js_generator_next(cc: CallContext, iter_state: &mut IteratorState) -> JSResult {
    let magic = cc.magic();
    let ctx = cc.ctx();
    *iter_state = IteratorState::Done;

    let mut this = *cc.this();
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
                let val = *cc.arg(0);

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
                    GEN_MAGIC_RETURN => Ok(*cc.arg(0)),
                    GEN_MAGIC_THROW => {
                        ctx.throw(*cc.arg(0));
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

pub static js_generator_proto_funcs: [FieldDef; 4] = [
    FieldDef::ITERATOR_NEXT("next", 1, js_generator_next, GEN_MAGIC_NEXT as _),
    FieldDef::ITERATOR_NEXT("return", 1, js_generator_next, GEN_MAGIC_RETURN as _),
    FieldDef::ITERATOR_NEXT("throw", 1, js_generator_next, GEN_MAGIC_THROW as _),
    FieldDef::STR("[Symbol.toStringTag]", "Generator", JS_PROP_CONFIGURABLE),
];

pub static js_generator_function_proto_funcs: [FieldDef; 1] = [FieldDef::STR(
    "[Symbol.toStringTag]",
    "GeneratorFunction",
    JS_PROP_CONFIGURABLE,
)];
