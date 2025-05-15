use std::{borrow::Cow, ffi::CStr, io::Write};

use crate::{
    atom::{consts::*, JSAtom, ATOM_GET_STR_BUF_SIZE, JS_ATOM_NULL},
    class::JSClassID,
    context::{is_strict_mode, JSContext},
    function::{
        get_func_name, js_class_has_bytecode, JSCFunctionListEntry, JSFunctionBytecode,
        JS_GetFunctionBytecode,
    },
    value::{
        object::{JS_PROP_CONFIGURABLE, JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_WRITABLE},
        JSTag, JSValue, JSValueConst, JS_ConcatString, JS_ConcatString3, JS_FreeValue,
        JS_IsEmptyString, JS_ToStringFree, JS_TAG_OBJECT, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CFUNC_DEF, JS_CLASS_ERROR, JS_PROP_STRING_DEF,
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

pub const NATIVE_ERROR_NAMES: [&CStr; JS_NATIVE_ERROR_COUNT] = [
    c"EvalError",
    c"RangeError",
    c"ReferenceError",
    c"SyntaxError",
    c"TypeError",
    c"URIError",
    c"InternalError",
    c"AggregateError",
];

pub const JS_THROW_VAR_RO: u8 = 0;
pub const JS_THROW_VAR_REDECL: u8 = 1;
pub const JS_THROW_VAR_UNINITIALIZED: u8 = 2;
pub const JS_THROW_ERROR_DELETE_SUPER: u8 = 3;
pub const JS_THROW_ERROR_ITERATOR_THROW: u8 = 4;

impl JSContext {
    /// put `obj` to current_exception, free old exception value. returns Exception value.
    /// Note: `obj` is consumed.
    pub fn throw(&mut self, obj: JSValue) -> JSValue {
        let old = self.rt_mut().current_exception.replace(obj);
        unsafe {
            JS_FreeValue(self, old); // free old exception value
        }
        JSValue::EXCEPTION
    }

    /// make error object and throw it as current exception.
    /// old exception will be freed.
    pub fn throw_error_ex(
        &mut self,
        error_num: JSErrorEnum,
        message: &CStr,
        add_backtrace: bool,
    ) -> JSValue {
        let obj = if let Ok(o) = self.create_object_with_proto_and_classid(
            self.native_error_proto[error_num as usize],
            JS_CLASS_ERROR,
        ) {
            o
        } else {
            // out of memory: throw NULL to avoid recursing
            JSValue::NULL
        };

        let s = self.create_string(message.to_string_lossy().as_ref());
        let _ = obj.define_property_value(
            self,
            JS_ATOM_message,
            s,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );

        if add_backtrace {
            self.build_backtrace_to(obj, None, 0);
        }

        self.throw(obj)
    }

    /// make error object and throw it as current exception.
    /// old exception will be freed.
    pub fn JS_ThrowError(&mut self, error_num: JSErrorEnum, message: &CStr) -> JSValue {
        let rt = self.rt();

        // the backtrace is added later if called from a bytecode function
        let sf = rt.current_stack_frame;
        let add_backtrace = !rt.in_out_of_memory
            && (sf.is_null() || unsafe { JS_GetFunctionBytecode((*sf).cur_func).is_null() });

        self.throw_error_ex(error_num, message, add_backtrace)
    }

    #[inline]
    pub fn JS_ThrowInternalError(&mut self, message: &CStr) -> JSValue {
        self.JS_ThrowError(JSErrorEnum::Internal, message)
    }

    #[inline]
    pub fn JS_ThrowTypeError(&mut self, message: &CStr) -> JSValue {
        self.JS_ThrowError(JSErrorEnum::Type, message)
    }

    #[inline]
    pub fn JS_ThrowTypeErrorNotAnObject(&mut self) -> JSValue {
        self.JS_ThrowTypeError(c"not an object")
    }

    #[inline]
    pub fn JS_ThrowTypeErrorNotASymbol(&mut self) -> JSValue {
        self.JS_ThrowTypeError(c"not a symbol")
    }

    pub fn JS_ThrowTypeErrorInvalidClass(&mut self, class_id: JSClassID) -> JSValue {
        let name = self.rt().class_array[class_id as usize].class_name;
        self.JS_ThrowTypeError(unsafe {
            CStr::from_ptr(
                format!("{} object expected\0", name.to_str(self.rt()))
                    .as_ptr()
                    .cast(),
            )
        })
    }

    /// returns: true=exception raised; false=no exception raised
    pub fn JS_ThrowTypeErrorOrFalse(&mut self, flags: u32, message: &CStr) -> bool {
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
    pub fn JS_ThrowTypeErrorReadOnly(&mut self, flags: u32, atom: JSAtom) -> bool {
        self.JS_ThrowTypeErrorOrFalse(flags, unsafe {
            CStr::from_ptr(
                format!("'{}' is read-only\0", atom.to_str(self.rt()))
                    .as_ptr()
                    .cast(),
            )
        })
    }

    /// raise out of memory exception
    pub fn JS_ThrowOutOfMemory(&mut self) -> JSValue {
        if !self.rt().in_out_of_memory {
            self.rt_mut().in_out_of_memory = true;
            self.JS_ThrowInternalError(&CStr::from_bytes_with_nul(b"out of memory\0").unwrap());
            self.rt_mut().in_out_of_memory = false;
        }
        JSValue::EXCEPTION
    }

    /// get and return current pending exception value. replace current_exception with null.
    /// Note: returnd value should be freed by caller
    pub fn take_exception(&mut self) -> JSValue {
        std::mem::replace(&mut self.rt_mut().current_exception, JSValue::NULL)
    }
}

pub const JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL: i32 = 1 << 0;
/* only taken into account if filename is provided */
pub const JS_BACKTRACE_FLAG_SINGLE_LEVEL: i32 = 1 << 1;

#[deprecated(note = "use JSContext::take_exception() instead")]
pub unsafe fn JS_GetException(ctx: *mut JSContext) -> JSValue {
    (*ctx).take_exception()
}

/* Note: it is important that no exception is returned by this function */
pub unsafe fn is_backtrace_needed(obj: JSValueConst) -> bool {
    if JS_VALUE_GET_TAG(obj) != JSTag::Object as i32 {
        false
    } else {
        let p = obj.object_unchecked();
        if p.class_id() != JS_CLASS_ERROR {
            false
        } else {
            p.find_own_property1(JS_ATOM_stack).is_none()
        }
    }
}

impl JSContext {
    fn inner_build_backtrace(
        &mut self,
        error_obj: JSValueConst,
        file_line: Option<(&str, u32)>, // filename and line number
        mut backtrace_flags: i32,
    ) -> Result<String, i32> {
        let mut buf = Vec::<u8>::with_capacity(128);
        let mut dbuf = std::io::Cursor::new(&mut buf);

        if let Some((filename, line_num)) = file_line {
            // filename and line no
            dbuf.write_fmt(format_args!("    at {filename}:{line_num}\n"))
                .map_err(|e| -1)?;

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

        let mut sf = self.rt().current_stack_frame;
        while !sf.is_null() {
            if backtrace_flags & JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL != 0 {
                backtrace_flags &= !JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL;
                sf = unsafe { (*sf).prev_frame };
                continue;
            }

            let str1 = unsafe { get_func_name(self, (*sf).cur_func) }.map_or(
                Cow::Borrowed("<anonymous>"),
                |x| {
                    if x.len() == 0 {
                        Cow::Borrowed("<anonymous>")
                    } else {
                        x
                    }
                },
            );
            dbuf.write_fmt(format_args!("    at {}", str1))
                .map_err(|e| -1)?;

            let p = unsafe { (*sf).cur_func.object_unchecked() };
            let mut backtrace_barrier = false;

            if js_class_has_bytecode(p.class_id()) {
                unsafe {
                    let b = p.u.func.function_bytecode;
                    backtrace_barrier = (*b).backtrace_barrier;

                    if (*b).has_debug {
                        let line_num1 = (*b).find_line_num(
                            ((*sf).cur_pc_offset as u32).wrapping_sub(1), // be u32::MAX when (*sf).cur_pc_offset==0
                        );

                        // let atom_str = JS_AtomToCString(ctx, (*b).debug.filename);
                        let atom_str = (*b)
                            .debug
                            .filename
                            .to_utf8_string(self.handle())
                            .unwrap_or(Cow::Borrowed("<null>"));

                        dbuf.write_fmt(format_args!(" ({}", atom_str.as_ref()))
                            .map_err(|e| -1)?;
                        if let Some(n) = line_num1 {
                            dbuf.write_fmt(format_args!(":{n}")).map_err(|e| -1)?;
                        }
                        dbuf.write_all(b")").map_err(|e| -1)?;
                    }
                }
            } else {
                dbuf.write_all(b" (native)").map_err(|e| -1)?;
            }
            dbuf.write_all(b"\n").map_err(|e| -1)?;

            /* stop backtrace if JS_EVAL_FLAG_BACKTRACE_BARRIER was used */
            if backtrace_barrier {
                break;
            }

            sf = unsafe { (*sf).prev_frame };
        }

        Ok(String::from_utf8_lossy(dbuf.into_inner()).to_string())
    }

    /// build traceback, set it as property of `error_obj`.
    /// if filename/line number is provided, they are defined as property of `error_obj` too.
    /// `error_obj` ref count is unchanged.
    pub fn build_backtrace_to(
        &mut self,
        error_obj: JSValueConst,
        file_line: Option<(&str, u32)>,
        backtrace_flags: i32,
    ) {
        let tb = self.inner_build_backtrace(error_obj, file_line, backtrace_flags);
        let tb_val = if let Ok(s) = tb {
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

// never use it directly
pub unsafe fn __JS_ThrowSyntaxErrorAtom(ctx: *mut JSContext, fmt: &CStr, atom: JSAtom) -> JSValue {
    let mut buf: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];

    libc::snprintf(
        buf.as_mut_ptr() as *mut libc::c_char,
        buf.len(),
        fmt.as_ptr() as *const libc::c_char,
        atom.to_str((*ctx).rt()),
    );
    JS_ThrowSyntaxError(ctx, CStr::from_bytes_with_nul(buf.as_slice()).unwrap())
}

pub fn JS_ThrowSyntaxErrorAtom(ctx: *mut JSContext, fmt: &CStr, atom: JSAtom) -> JSValue {
    unsafe { __JS_ThrowSyntaxErrorAtom(ctx, fmt, atom) }
}

// #[deprecated(note = "use JSContext::JS_ThrowTypeErr()")]
// pub unsafe fn JS_ThrowTypeErrorAtom(ctx: *mut JSContext, fmt: &CStr, atom: JSAtom) -> JSValue {
//     __JS_ThrowTypeErrorAtom(ctx, fmt, atom)
// }

pub unsafe fn JS_ThrowReferenceErrorUninitialized(ctx: *mut JSContext, name: JSAtom) -> JSValue {
    if name == JS_ATOM_NULL {
        JS_ThrowReferenceError(ctx, c"lexical variable is not initialized")
    } else {
        JS_ThrowReferenceError(
            ctx,
            CStr::from_bytes_with_nul(
                format!("{} is not initialized\0", name.to_str((*ctx).rt())).as_bytes(),
            )
            .unwrap(),
        )
    }
}

pub unsafe fn JS_ThrowReferenceError(ctx: *mut JSContext, message: &CStr) -> JSValue {
    ctx.as_mut()
        .unwrap()
        .JS_ThrowError(JSErrorEnum::Reference, message)
}

pub unsafe fn JS_ThrowReferenceErrorNotDefined(ctx: *mut JSContext, name: JSAtom) -> JSValue {
    JS_ThrowReferenceError(
        ctx,
        CStr::from_bytes_with_nul(
            format!("'{}' is not defined\0", name.to_str((*ctx).rt())).as_bytes(),
        )
        .unwrap(),
    )
}

pub unsafe fn JS_ThrowReferenceErrorUninitialized2(
    ctx: *mut JSContext,
    b: *const JSFunctionBytecode,
    idx: u16,
    is_ref: bool,
) -> JSValue {
    let atom = if is_ref {
        (*b).get_closure_var(idx).map(|c| c.var_name)
    } else {
        // not present if the function is stripped and contains no eval()
        (*b).get_vardef(idx, false).map(|vd| vd.var_name)
    }
    .unwrap_or(JS_ATOM_NULL);

    JS_ThrowReferenceErrorUninitialized(ctx, atom)
}

pub unsafe fn JS_ThrowTypeErrorRevokedProxy(ctx: *mut JSContext) -> JSValue {
    ctx.as_mut().unwrap().JS_ThrowTypeError(c"revoked proxy")
}

pub fn JS_ThrowTypeErrorDetachedArrayBuffer(ctx: *mut JSContext) -> JSValue {
    unsafe {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"ArrayBuffer is detached")
    }
}

pub fn JS_ThrowTypeErrorPrivateNotFound(ctx: *mut JSContext, atom: JSAtom) -> JSValue {
    unsafe {
        (*ctx).JS_ThrowTypeError(CStr::from_ptr(
            format!(
                "private class field '{}' does not exist\0",
                atom.to_str((*ctx).rt())
            )
            .as_ptr()
            .cast(),
        ))
    }
}

pub fn JS_ThrowRangeError(ctx: *mut JSContext, message: &CStr) -> JSValue {
    unsafe {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowError(JSErrorEnum::Range, message)
    }
}

pub fn JS_ThrowSyntaxError(ctx: *mut JSContext, message: &CStr) -> JSValue {
    unsafe {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowError(JSErrorEnum::Syntax, message)
    }
}

pub fn JS_ThrowSyntaxErrorVarRedeclaration(ctx: *mut JSContext, prop: JSAtom) -> JSValue {
    JS_ThrowSyntaxErrorAtom(ctx, c"redeclaration of '%s'", prop)
}

pub unsafe fn JS_ThrowStackOverflow(ctx: *mut JSContext) -> JSValue {
    ctx.as_mut()
        .unwrap()
        .JS_ThrowInternalError(c"stack overflow")
}

/// used to avoid catching interrupt exceptions
pub unsafe fn JS_IsUncatchableError(ctx: *const JSContext, val: JSValueConst) -> bool {
    if JS_VALUE_GET_TAG(val) != JSTag::Object as i32 {
        false
    } else {
        let p = JS_VALUE_GET_OBJ(val);
        (*p).class_id() == JS_CLASS_ERROR && (*p).detailed.is_uncatchable_error()
    }
}

pub unsafe fn JS_SetUncatchableError(ctx: *mut JSContext, val: JSValueConst, flag: bool) {
    if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
        return;
    }
    let p = JS_VALUE_GET_OBJ(val);
    if (*p).class_id() == JS_CLASS_ERROR {
        (*p).detailed.set_uncatchable_error(flag);
    }
}

pub unsafe fn js_error_toString(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    if !this_val.is_object() {
        return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
    }

    let mut name = this_val.get_property(ctx.as_mut().unwrap(), JS_ATOM_name);
    name = if name.is_undefined() {
        JS_ATOM_Error.to_value(ctx.as_mut().unwrap(), true)
    } else {
        JS_ToStringFree(ctx, name)
    };
    if name.is_exception() {
        return JSValue::EXCEPTION;
    }

    let mut msg = this_val.get_property(ctx.as_mut().unwrap(), JS_ATOM_message);
    if msg.is_undefined() {
        msg = JS_ATOM_empty_string.to_value(ctx.as_mut().unwrap(), true);
    } else {
        let s = msg
            .coerce_to_string_value(ctx.as_mut().unwrap())
            .unwrap_or(JSValue::EXCEPTION);
        JS_FreeValue(ctx, msg);
        msg = s;
    }
    if msg.is_exception() {
        JS_FreeValue(ctx, name);
        return JSValue::EXCEPTION;
    }

    if !JS_IsEmptyString(name) && !JS_IsEmptyString(msg) {
        name = JS_ConcatString3(ctx, c"", name, c": ");
    }

    return JS_ConcatString(ctx, name, msg);
}

pub const js_error_proto_funcs: [JSCFunctionListEntry; 3] = [
    JS_CFUNC_DEF!(c"toString", 0, js_error_toString),
    JS_PROP_STRING_DEF!(c"name", c"Error", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
    JS_PROP_STRING_DEF!(c"message", c"", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
];
