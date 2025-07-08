use std::ffi::c_void;

use smallvec::SmallVec;

use crate::{
    JS_CLASS_BYTECODE_FUNCTION,
    atom::consts::JS_ATOM_prototype,
    context::JSContext,
    error::JSResult,
    eval::JS_EVAL_TYPE_INDIRECT,
    function::{CallContext, JS_CALL_FLAG_COPY_ARGV, JS_GetFunctionRealm, func_kind_to_class_id},
    object::{JS_SetPrototypeInternal, JSObject},
    stack_frame::StackFrame,
    value::JSValue,
};

use super::JSFunctionKindEnum;

#[repr(transparent)]
#[derive(Clone)]
pub struct BytecodeFunction(JSObject);

impl TryFrom<JSObject> for BytecodeFunction {
    type Error = i32;

    fn try_from(obj: JSObject) -> Result<Self, Self::Error> {
        if obj.class_id() == JS_CLASS_BYTECODE_FUNCTION {
            Ok(Self(obj))
        } else {
            Err(-1)
        }
    }
}

impl TryFrom<JSValue> for BytecodeFunction {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        if let JSValue::Object(o) = value {
            o.try_into()
        } else {
            Err(-1)
        }
    }
}

impl From<BytecodeFunction> for JSObject {
    #[inline(always)]
    fn from(func: BytecodeFunction) -> Self {
        func.0
    }
}

impl From<BytecodeFunction> for JSValue {
    #[inline(always)]
    fn from(func: BytecodeFunction) -> Self {
        JSValue::Object(func.0)
    }
}

impl std::fmt::Debug for BytecodeFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<BytecodeFunction {:p}>", self.0.id() as *const c_void)
    }
}

impl BytecodeFunction {
    /// call bytecode function
    pub fn call(
        &self,
        caller_ctx: &JSContext,
        this_obj: &JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSValue {
        let (bc, home_obj) = self
            .0
            .0
            .isa
            .borrow()
            .func()
            .map(|f| (f.bytecode.clone().unwrap(), f.home_object.clone()))
            .unwrap();

        // how many args that should be copied to local buf?
        // `args.len()` is actual argument count
        // `b.arg_count` is the length when defining the function
        let arg_copy_num = if flags & JS_CALL_FLAG_COPY_ARGV != 0
            || args.len() < bc.0.borrow().arg_count as usize
        {
            std::cmp::max(args.len(), bc.0.borrow().arg_count as usize)
        } else {
            0
        };

        // how many bytes to allocate for local buf (args+vars+stack)?
        let (buf_values_count, js_mode) = {
            let b = bc.0.borrow();
            (
                arg_copy_num as usize + b.var_count as usize + b.stack_size as usize,
                b.js_mode as i32,
            )
        };

        let mut sframe = StackFrame {
            cur_running: JSValue::Object(self.0.clone()),
            js_mode,
            ..Default::default()
        };
        let sf = &mut sframe;

        // make local buffer and running bytecodes with it.
        // local_buf layout:
        //
        // +------------------+
        // | argument values  | <- arg_buf (optional, if need to copy arg)
        // | variable values  | <- var_buf
        // | stack space      | <- stack_buf
        // +------------------+
        //
        if sf.local_buf.try_reserve_exact(buf_values_count).is_err() {
            caller_ctx.JS_ThrowOutOfMemory();
            return JSValue::EXCEPTION;
        }

        // initialize args section
        if arg_copy_num != 0 {
            debug_assert!(arg_copy_num >= args.len());

            // copy actual arg values
            sf.local_buf.extend(args.iter().map(|x| x.clone()));
            // padding with undefined
            sf.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                arg_copy_num - args.len(),
            ));

            sf.arg_count = arg_copy_num as _;
            sf.arg_base_ptr = sf.local_buf.as_mut_ptr()
        } else {
            sf.arg_count = args.len() as _;
            sf.arg_base_ptr = args.as_ptr().cast::<JSValue>() as *mut _;
        }

        // initialize (vars + stack) section
        {
            let b = bc.0.borrow();

            sf.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                (b.var_count + b.stack_size) as usize,
            ));
            debug_assert_eq!(sf.local_buf.len(), buf_values_count);

            sf.var_base_ptr = unsafe { sf.local_buf.as_mut_ptr().add(arg_copy_num as usize) };
            sf.var_count = b.var_count;
        }

        sf.prev_frame = caller_ctx.rt().cur_stack_frame;
        caller_ctx.rt_mut().cur_stack_frame = (sf as *mut StackFrame).cast();

        let retval = bc.run(
            caller_ctx,
            home_obj.as_ref(),
            sf,
            this_obj,
            new_target,
            0,
            0,
            &self.0.0.isa.borrow().func().unwrap().var_refs,
            args,
            false,
        );

        debug_assert_eq!(
            caller_ctx.rt().cur_stack_frame,
            sf.prev_frame,
            "runtime current stack frame should be restored after running bytecodes"
        );

        // stack frame is about to drop
        sf.finalize();

        retval
    }

    /// handle `Function` constructor. For example:
    ///
    /// ```js
    /// const f = new Function("a", "b", "return a+b")
    /// ```
    pub fn hook_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
        debug_assert!((0..4).contains(&cc.magic()));

        // XXX: add a specific eval mode so that Function("}), ({") is rejected

        let new_target = cc.this();
        let func_kind = unsafe { std::mem::transmute::<u8, JSFunctionKindEnum>(cc.magic() as u8) };

        //
        // make function source script:
        // ([async] function[*] anonymous(a,b){<body>})
        //
        let mut buf = SmallVec::<[u8; 256]>::new();
        buf.push(b'(');

        if matches!(
            func_kind,
            JSFunctionKindEnum::JS_FUNC_ASYNC | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            buf.extend_from_slice(b"async ");
        }

        buf.extend_from_slice(b"function");
        if matches!(
            func_kind,
            JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            buf.push(b'*');
        }

        buf.extend_from_slice(b" anonymous(");

        // Note: the last argument is function body.
        if let Some((body, arglist)) = cc.arguments().split_last() {
            // args list
            for (i, a) in arglist.iter().enumerate() {
                if i != 0 {
                    buf.push(b',');
                }
                buf.extend_from_slice(a.coerce_to_utf8(ctx)?.as_bytes());
            }
            buf.extend_from_slice(b"){");

            // function body
            buf.extend_from_slice(body.coerce_to_utf8(ctx)?.as_bytes());
        }

        buf.extend_from_slice(b"})");

        // create script string
        let src = ctx
            .create_string_from_utf8(buf.into_vec())
            .check_exception()?;

        // eval string `src`
        let func_val = {
            // Note: to avoid ctx.0 borrow conflict, fetch global object in advance
            let global = JSValue::Object(ctx.global_obj());
            src.eval_object(ctx, global, JS_EVAL_TYPE_INDIRECT, -1)
                .check_exception()?
        };

        if !new_target.is_undefined() {
            // set the prototype
            let mut proto = new_target
                .get_property(ctx, JS_ATOM_prototype)
                .check_exception()?;

            if !proto.is_object() {
                let realm = JS_GetFunctionRealm(ctx, &new_target)?;
                proto = realm.0.borrow().class_proto
                    [func_kind_to_class_id[func_kind as usize] as usize]
                    .clone();
            }

            JS_SetPrototypeInternal(ctx, &func_val, proto, true)?;
        }

        Ok(func_val)
    }
}
