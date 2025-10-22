use std::{
    cell::{Cell, UnsafeCell},
    ops::Deref,
};

use crate::{
    JS_CLASS_GENERATOR, JS_ITERATOR_NEXT_DEF, JS_PROP_STRING_DEF,
    atom::consts::JS_ATOM_prototype,
    context::JSContext,
    error::JSResult,
    eval::JS_EVAL_TYPE_INDIRECT,
    function::{
        AsyncFuncState, CallContext, FieldDef, IteratorState, JS_GetFunctionRealm,
        JSFunctionKindEnum,
    },
    iterator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    runtime::JSRuntime,
    stack_frame::StackFrame,
    string::{StringBuf, StringData},
    value::{JSValue, JSValueConst},
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
    state: Cell<GenState>,
    func_state: UnsafeCell<AsyncFuncState>,
}

impl GeneratorPayload {
    #[inline(always)]
    const fn state(&self) -> GenState {
        self.state.get()
    }

    #[inline(always)]
    fn set_state(&self, state: GenState) {
        self.state.set(state);
    }

    fn set_throw_flag(&self, b: bool) {
        unsafe {
            (&*self.func_state.get()).set_throw_flag(b);
        }
    }

    #[inline(always)]
    pub fn is_completed(&self) -> bool {
        unsafe { (&*self.func_state.get()).is_completed() }
    }

    /// run with stack frame
    #[inline(always)]
    fn with_stack_frame_mut<R>(&self, f: impl FnOnce(&mut StackFrame) -> R) -> R {
        unsafe {
            let mut t = (&*self.func_state.get()).0.borrow_mut();
            f(&mut t.as_mut().get_mut().frame)
        }
    }

    /// execute the function up to 'OP_initial_yield' opcode
    #[inline(always)]
    pub fn resume(&self, ctx: &JSContext) -> JSResult {
        unsafe { (&*self.func_state.get()).resume(ctx) }
    }

    /// mark generator is completed
    pub fn complete(&self) {
        if self.state() != GenState::JS_GENERATOR_STATE_COMPLETED {
            self.set_state(GenState::JS_GENERATOR_STATE_COMPLETED);
            //  self.func_state.take();
        }
    }
}

// XXX: add a specific eval mode so that Function("}), ({") is rejected
#[deprecated(note = "should use BytecodeFunction::class_constructor ???")]
pub fn js_function_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let func_kind =
        unsafe { std::mem::transmute::<u8, crate::function::JSFunctionKindEnum>(cc.magic() as u8) };

    // build function source
    let mut source = StringBuf::with_capacity(64, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    source.push_char('(').unwrap();
    if matches!(
        func_kind,
        JSFunctionKindEnum::JS_FUNC_ASYNC | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
    ) {
        source.push_str("async").unwrap();
    }
    source.push_str("function").unwrap();

    if matches!(
        func_kind,
        JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
    ) {
        source.push_char('*').unwrap();
    }
    source.push_str(" anonymous(").unwrap();

    if let Some((body, args)) = cc.arguments().split_last() {
        // args list
        for (i, a) in args.iter().enumerate() {
            if i != 0 {
                source.push_char(',').map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            let s = a.to_string(ctx)?;
            source.push_content(s.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
        source.push_str(") {\n").map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        // body
        let s = body.to_string(ctx)?;
        source.push_content(s.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        source.push_str("\n})").map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    } else {
        source.push_str(") {})").map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }

    // eval to make function
    let func_obj = ctx.eval_internal(
        &JSValue::Object(ctx.global_obj()),
        &StringData::from(source).encode_utf8_bytes(),
        "<input>",
        JS_EVAL_TYPE_INDIRECT,
        -1,
    )?;

    // set prototype, if provided by `this`
    let new_target = cc.this();
    if !new_target.is_undefined() {
        let mut proto = new_target.get_property(ctx, JS_ATOM_prototype.deref())?;
        if !proto.is_object() {
            let realm = JS_GetFunctionRealm(ctx, new_target)?;
            proto = realm.0.borrow().class_proto[func_kind.class_id() as usize].clone();
        }

        func_obj.set_prototype_internal(ctx, proto, true)?;
    }

    Ok(func_obj)
}

pub fn js_generator_function_call(
    ctx: &JSContext,
    func_obj: &JSValueConst,
    this_obj: &JSValueConst,
    args: &[JSValue],
    _flags: i32,
) -> JSResult {
    let func_state = AsyncFuncState::new(
        ctx,
        func_obj.as_object_BCFunction().unwrap().clone(),
        this_obj.clone(),
        args,
    )?;
    let g = Box::new(GeneratorPayload {
        state: Cell::new(GenState::JS_GENERATOR_STATE_SUSPENDED_START),
        func_state: UnsafeCell::new(func_state),
    });

    // execute the function up to 'OP_initial_yield'
    g.resume(ctx)?;

    // create js object of generator class
    ctx.create_object_from_ctor(func_obj, ObjectPayload::GENERATOR(g))
        .map(JSValue::Object)
        .map_err(|_| -1)
}

pub const fn js_generator_finalizer(_rt: &mut JSRuntime, _isa: &ObjectPayload, _id: usize) {
    // if let ObjectPayload::GENERATOR(_) = isa {
    //     //s.free_generator_stack_rt(rt);
    // }
}

pub fn js_generator_mark(_: JSValue) {
    unimplemented!();
}

pub fn js_generator_next(
    ctx: &JSContext,
    cc: CallContext,
    iter_state: &mut IteratorState,
) -> JSResult {
    let magic = cc.magic();
    *iter_state = IteratorState::Done;

    if let ObjectPayload::GENERATOR(g) =
        cc.this().retrive_object_payload(ctx, JS_CLASS_GENERATOR)?
    {
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
                let val = cc.arg(0).clone();

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

                debug_assert!(res.as_ref().unwrap().is_int());

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
                    GEN_MAGIC_RETURN => Ok(cc.arg(0).clone()),
                    GEN_MAGIC_THROW => {
                        ctx.throw(cc.arg(0).clone());
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
    JS_ITERATOR_NEXT_DEF!("next", 1, js_generator_next, GEN_MAGIC_NEXT as _),
    JS_ITERATOR_NEXT_DEF!("return", 1, js_generator_next, GEN_MAGIC_RETURN as _),
    JS_ITERATOR_NEXT_DEF!("throw", 1, js_generator_next, GEN_MAGIC_THROW as _),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "Generator", JS_PROP_CONFIGURABLE),
];

pub static js_generator_function_proto_funcs: [FieldDef; 1] = [JS_PROP_STRING_DEF!(
    "[Symbol.toStringTag]",
    "GeneratorFunction",
    JS_PROP_CONFIGURABLE
)];
