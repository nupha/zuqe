use std::{borrow::Cow, io::Write};

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CLASS_ERROR, JS_PROP_STRING_DEF,
    atom::{JS_ATOM_NULL, JSAtom, consts::*},
    class::JSClassID,
    context::{JSContext, is_strict_mode},
    function::{Bytecode, CallContext, NativeField, get_func_name},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_WRITABLE, ObjectDetail,
    },
    runtime::JSRuntime,
    stack_frame::StackFrame,
    string::{JS_ConcatString, JS_ConcatString3, JS_IsEmptyString},
    value::{JSValue, JSValueConst},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum JSErrorEnum {
    Eval,
    Range,
    Reference,
    Syntax,
    Type,
    Uri,
    Internal,
    Aggregate,
    NativeErrorCount,
}

pub const JS_NATIVE_ERROR_COUNT: usize = JSErrorEnum::NativeErrorCount as usize;

pub type JSResult = Result<JSValue, i32>;

pub const NATIVE_ERROR_NAMES: [&str; JS_NATIVE_ERROR_COUNT] = [
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "InternalError",
    "AggregateError",
];

pub const JS_THROW_VAR_RO: u8 = 0;
pub const JS_THROW_VAR_REDECL: u8 = 1;
pub const JS_THROW_VAR_UNINITIALIZED: u8 = 2;
pub const JS_THROW_ERROR_DELETE_SUPER: u8 = 3;
pub const JS_THROW_ERROR_ITERATOR_THROW: u8 = 4;

impl JSContext {
    /// Get current exception value
    #[inline(always)]
    pub fn exception(&self) -> JSValue {
        self.rt().cur_exception.clone()
    }

    /// get current exception value, and reset current_exception to null.
    #[inline(always)]
    pub fn take_exception(&self) -> JSValue {
        self.set_exception(JSValue::NULL)
    }

    /// set new obj to current_exception, returns the old exception value.
    /// Note: returnd value should be freed by caller
    #[inline]
    fn set_exception(&self, obj: JSValue) -> JSValue {
        std::mem::replace(&mut self.rt_mut().cur_exception, obj)
    }

    /// put `obj` to current_exception, free old exception.
    /// Note: `obj` is consumed.
    #[inline(always)]
    pub fn throw(&self, obj: JSValue) {
        // strict debug guard.
        #[cfg(debug_assertions)]
        if obj.is_undefined_or_null() {
            panic!("throw a null or undefined value is not recommanded");
        }

        self.set_exception(obj);
    }

    /// make error object and throw it as current exception.
    /// old exception will be freed.
    pub fn throw_error_ex(&self, error_num: JSErrorEnum, message: &str, add_backtrace: bool) {
        let obj = if let Ok(o) = self.create_instance_from_proto(
            &self.0.borrow().native_error_proto[error_num as usize],
            ObjectDetail::ERROR,
        ) {
            o
        } else {
            // out of memory: throw NULL to avoid recursing
            JSValue::NULL
        };

        let s = self.create_string(message);
        let _ = obj.define_property_value(
            self,
            JS_ATOM_message,
            s,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );

        if add_backtrace {
            self.build_backtrace_to(&obj, None, 0);
        }

        self.throw(obj);
    }

    /// make error object and throw it as current exception.
    /// old exception will be freed.
    pub fn JS_ThrowError(&self, error_num: JSErrorEnum, message: &str) {
        let rt = self.rt();

        // the backtrace is added later if called from a bytecode function
        let sf = rt.cur_stack_frame.cast::<StackFrame>();
        let add_backtrace = !rt.in_out_of_memory
            && (sf.is_null()
                || unsafe {
                    (*sf)
                        .cur_running
                        .object()
                        .and_then(|o| o.0.detail.borrow().func().and_then(|f| f.bytecode.clone()))
                        .is_none()
                });

        self.throw_error_ex(error_num, message, add_backtrace);
    }

    // pub fn JS_ThrowError(&self, error_num: JSErrorEnum, message: &str) {}

    #[inline]
    pub fn JS_ThrowInternalError(&self, message: &str) {
        self.JS_ThrowError(JSErrorEnum::Internal, message);
    }

    #[inline]
    pub fn JS_ThrowTypeError(&self, message: &str) {
        self.JS_ThrowError(JSErrorEnum::Type, message);
    }

    #[inline]
    pub fn JS_ThrowTypeErrorNotAnObject(&self) {
        self.JS_ThrowTypeError("not an object");
    }

    #[inline]
    pub fn JS_ThrowTypeErrorNotASymbol(&self) {
        self.JS_ThrowTypeError("not a symbol");
    }

    pub fn JS_ThrowTypeErrorInvalidClass(&self, class_id: JSClassID) {
        let name = self.rt().class_defs[class_id as usize].class_name;
        self.JS_ThrowTypeError(&format!("{} object expected", name.to_str(self.rt())))
    }

    /// returns: true=exception raised; false=no exception raised
    pub fn JS_ThrowTypeErrorOrFalse(&self, flags: u32, message: &str) -> bool {
        if flags & JS_PROP_THROW != 0 || (flags & JS_PROP_THROW_STRICT != 0 && is_strict_mode(self))
        {
            self.JS_ThrowTypeError(message);
            true
        } else {
            false
        }
    }

    /// flags: 1=raise exception; 0=don't raise exception
    /// returns: true=exception raised; false=no exception raised
    pub fn JS_ThrowTypeErrorReadOnly(&self, flags: u32, atom: JSAtom) -> bool {
        self.JS_ThrowTypeErrorOrFalse(flags, &format!("'{}' is read-only", atom.to_str(self.rt())))
    }

    /// raise out of memory exception
    pub fn JS_ThrowOutOfMemory(&self) {
        if !self.rt().in_out_of_memory {
            self.rt_mut().in_out_of_memory = true;
            self.JS_ThrowInternalError("out of memory");
            self.rt_mut().in_out_of_memory = false;
        }
    }
}

pub const JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL: i32 = 1 << 0;
/* only taken into account if filename is provided */
pub const JS_BACKTRACE_FLAG_SINGLE_LEVEL: i32 = 1 << 1;

#[deprecated(note = "use JSContext::take_exception() instead")]
pub unsafe fn JS_GetException(ctx: &JSContext) -> JSValue {
    ctx.take_exception()
}

/* Note: it is important that no exception is returned by this function */
pub fn is_backtrace_needed(obj: &JSValueConst) -> bool {
    if let JSValue::Object(p) = obj {
        if p.class_id() != JS_CLASS_ERROR {
            false
        } else {
            !p.has_shape_property(JS_ATOM_stack)
        }
    } else {
        false
    }
}

impl JSContext {
    fn do_build_backtrace(
        &self,
        error_obj: &JSValue,
        file_line: Option<(&str, i32)>, // filename and line number
        mut backtrace_flags: i32,
    ) -> Result<String, i32> {
        let mut buf = Vec::<u8>::with_capacity(128);
        let mut dbuf = std::io::Cursor::new(&mut buf);

        if let Some((filename, line_num)) = file_line {
            // filename and line no
            dbuf.write_fmt(format_args!("    at {filename}:{line_num}\n"))
                .map_err(|_| -1)?;

            let str = (*self).create_string(filename);
            error_obj.define_property_value(
                self,
                JS_ATOM_fileName,
                str,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;
            error_obj.define_property_value(
                self,
                JS_ATOM_lineNumber,
                JSValue::from(line_num as i32),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;

            if backtrace_flags & JS_BACKTRACE_FLAG_SINGLE_LEVEL != 0 {
                return Ok(String::from_utf8_lossy(dbuf.into_inner()).to_string());
            }
        }

        let mut sf = self.rt().cur_stack_frame.cast::<StackFrame>();
        while !sf.is_null() {
            if backtrace_flags & JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL != 0 {
                backtrace_flags &= !JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL;
                sf = unsafe { (*sf).prev_frame.cast() };
                continue;
            }

            let str1 = get_func_name(unsafe { &(*sf).cur_running }).map_or(
                Cow::Borrowed("<anonymous>"),
                |x| {
                    if x.len() == 0 {
                        Cow::Borrowed("<anonymous>")
                    } else {
                        Cow::Owned(x)
                    }
                },
            );
            dbuf.write_fmt(format_args!("    at {}", str1))
                .map_err(|_| -1)?;

            let mut backtrace_barrier = false;

            if let Some(b) = unsafe {
                (*sf)
                    .cur_running
                    .object_unchecked()
                    .0
                    .detail
                    .borrow()
                    .func()
                    .and_then(|f| f.bytecode.clone())
            } {
                backtrace_barrier = b.0.borrow().backtrace_barrier;

                if b.0.borrow().has_debug {
                    let line_num1 = b.find_line_num(
                        unsafe { ((*sf).cur_pc_offset as u32).wrapping_sub(1) }, // be u32::MAX when (*sf).cur_pc_offset==0
                    );

                    let atom_str =
                        b.0.borrow()
                            .debug
                            .filename
                            .to_utf8_string(self)
                            .unwrap_or("<null>".to_string());

                    dbuf.write_fmt(format_args!(" ({}", atom_str))
                        .map_err(|_| -1)?;
                    if let Some(n) = line_num1 {
                        dbuf.write_fmt(format_args!(":{n}")).map_err(|_| -1)?;
                    }
                    dbuf.write_all(b")").map_err(|_| -1)?;
                }
            } else {
                dbuf.write_all(b" (native)").map_err(|_| -1)?;
            }
            dbuf.write_all(b"\n").map_err(|_| -1)?;

            /* stop backtrace if JS_EVAL_FLAG_BACKTRACE_BARRIER was used */
            if backtrace_barrier {
                break;
            }

            sf = unsafe { (*sf).prev_frame.cast() };
        }

        Ok(String::from_utf8_lossy(dbuf.into_inner()).to_string())
    }

    /// build traceback, set it as property of `error_obj`.
    /// if filename/line number is provided, they are defined as property of `error_obj` too.
    /// `error_obj` is not consumed.
    pub fn build_backtrace_to(
        &self,
        error_obj: &JSValue,
        file_line: Option<(&str, i32)>,
        backtrace_flags: i32,
    ) {
        let tb_val = if let Ok(s) = self.do_build_backtrace(error_obj, file_line, backtrace_flags) {
            self.create_string_from_utf8(s.into_bytes())
        } else {
            JSValue::NULL
        };

        let _ = error_obj.define_property_value(
            self,
            JS_ATOM_stack,
            tb_val,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );
    }
}

pub fn JS_ThrowReferenceErrorUninitialized(ctx: &JSContext, name: JSAtom) {
    if name == JS_ATOM_NULL {
        JS_ThrowReferenceError(ctx, "lexical variable is not initialized");
    } else {
        JS_ThrowReferenceError(
            ctx,
            &format!("{} is not initialized", name.to_str(&ctx.rt())),
        );
    }
}

pub fn JS_ThrowReferenceError(ctx: &JSContext, message: &str) {
    ctx.JS_ThrowError(JSErrorEnum::Reference, message)
}

pub fn JS_ThrowReferenceErrorNotDefined(ctx: &JSContext, name: JSAtom) {
    JS_ThrowReferenceError(ctx, &format!("'{}' is not defined", name.to_str(ctx.rt())))
}

pub fn JS_ThrowReferenceErrorUninitialized2(ctx: &JSContext, b: &Bytecode, idx: u16, is_ref: bool) {
    let atom = if is_ref {
        b.with_closure_var(idx, |c| c.var_name)
    } else {
        // not present if the function is stripped and contains no eval()
        b.with_vardef(idx, false, |vd| vd.var_name)
    }
    .unwrap_or(JS_ATOM_NULL);

    JS_ThrowReferenceErrorUninitialized(ctx, atom);
}

pub fn JS_ThrowTypeErrorRevokedProxy(ctx: &JSContext) {
    ctx.JS_ThrowTypeError("revoked proxy");
}

pub fn JS_ThrowTypeErrorDetachedArrayBuffer(ctx: &JSContext) {
    ctx.JS_ThrowTypeError("ArrayBuffer is detached");
}

pub fn JS_ThrowTypeErrorPrivateNotFound(ctx: &JSContext, atom: JSAtom) {
    ctx.JS_ThrowTypeError(&format!(
        "private class field '{}' does not exist",
        atom.to_str(ctx.rt())
    ));
}

pub fn JS_ThrowRangeError(ctx: &JSContext, message: &str) {
    ctx.JS_ThrowError(JSErrorEnum::Range, message);
}

pub fn JS_ThrowSyntaxError(ctx: &JSContext, message: &str) {
    ctx.JS_ThrowError(JSErrorEnum::Syntax, message);
}

pub fn JS_ThrowSyntaxErrorVarRedeclaration(ctx: &JSContext, prop: JSAtom) {
    JS_ThrowSyntaxError(
        ctx,
        &format!("redeclaration of '{}'", prop.to_str(ctx.rt())),
    );
}

pub fn JS_ThrowStackOverflow(ctx: &JSContext) {
    ctx.JS_ThrowInternalError("stack overflow");
}

/// used to avoid catching interrupt exceptions
pub unsafe fn JS_IsUncatchableError(val: &JSValue) -> bool {
    if let JSValue::Object(p) = val {
        p.class_id() == JS_CLASS_ERROR && p.flags().is_uncatchable_error()
    } else {
        false
    }
}

pub fn JS_SetUncatchableError(val: &JSValue, flag: bool) {
    if let JSValue::Object(p) = val {
        if p.class_id() == JS_CLASS_ERROR {
            unsafe {
                (*p.0.detailed.as_ptr()).set_uncatchable_error(flag);
            }
        }
    }
}

pub fn js_error_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let mut name = cc.this().get_property(ctx, JS_ATOM_name);
    name = if name.is_undefined() {
        JS_ATOM_Error.to_value(ctx, true)
    } else {
        name.coerce_to_string(ctx)?
    };

    let mut msg = cc.this().get_property(ctx, JS_ATOM_message);
    msg = if msg.is_undefined() {
        JS_ATOM_empty_string.to_value(ctx, true)
    } else {
        msg.coerce_to_string(ctx)?
    };

    if !JS_IsEmptyString(&name) && !JS_IsEmptyString(&msg) {
        name = unsafe { JS_ConcatString3(ctx, c"", name, c": ").check_exception()? };
    }

    unsafe { JS_ConcatString(ctx, name, msg).check_exception() }
}

pub const js_error_proto_funcs: [NativeField; 3] = [
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_error_toString, 0),
    JS_PROP_STRING_DEF!("name", "Error", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
    JS_PROP_STRING_DEF!("message", "", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
];
