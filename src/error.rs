use std::{
    borrow::Cow,
    ffi::{c_char, CStr},
    io::Write,
};

use crate::{
    atom::{consts::*, JSAtom, ATOM_GET_STR_BUF_SIZE, JS_ATOM_NULL},
    class::JSClassID,
    context::{is_strict_mode, JSContext},
    function::{
        get_func_name, js_class_has_bytecode, JSCFunctionListEntry, JSFunctionBytecode,
        JS_GetFunctionBytecode,
    },
    value::{
        object::{
            JS_NewObjectProtoClass, JS_PROP_CONFIGURABLE, JS_PROP_THROW, JS_PROP_THROW_STRICT,
            JS_PROP_WRITABLE,
        },
        JSTag, JSValue, JSValueConst, JS_ConcatString, JS_ConcatString3, JS_FreeValue,
        JS_IsEmptyString, JS_IsObject, JS_ToStringFree, JS_TAG_OBJECT, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_TAG,
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
    /// put `obj` to current_exception, returns Exception value
    /// Note: obj is consumed.
    pub fn JS_Throw(&mut self, obj: JSValue) -> JSValue {
        unsafe {
            let old = self.rt_mut().current_exception.replace(obj);
            JS_FreeValue(self, old); // free old exception value
        }
        JSValue::EXCEPTION
    }

    #[inline]
    pub fn JS_ThrowInternalError(&mut self, message: &CStr) -> JSValue {
        self.JS_ThrowError(JSErrorEnum::Internal, message)
    }

    pub fn JS_ThrowError2(
        &mut self,
        error_num: JSErrorEnum,
        message: &CStr,
        add_backtrace: bool,
    ) -> JSValue {
        let mut obj = unsafe {
            JS_NewObjectProtoClass(
                self,
                self.native_error_proto[error_num as usize],
                JS_CLASS_ERROR,
            )
        };

        if obj.is_exception() {
            // out of memory: throw JS_NULL to avoid recursing
            obj = JSValue::NULL;
        } else {
            let s = self.create_string(message.to_string_lossy().as_ref());
            let _ = obj.define_property_value(
                self,
                JS_ATOM_message,
                s,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            );
        }
        if add_backtrace {
            unsafe {
                build_backtrace(self, obj, std::ptr::null_mut(), Some(0), 0);
            }
        }

        self.JS_Throw(obj)
    }

    pub fn JS_ThrowError(&mut self, error_num: JSErrorEnum, message: &CStr) -> JSValue {
        let rt = self.rt();

        // the backtrace is added later if called from a bytecode function
        let sf = rt.current_stack_frame;
        let add_backtrace = !rt.in_out_of_memory
            && (sf.is_null() || unsafe { JS_GetFunctionBytecode((*sf).cur_func).is_null() });

        self.JS_ThrowError2(error_num, message, add_backtrace)
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
    pub fn get_exception(&mut self) -> JSValue {
        std::mem::replace(&mut self.rt_mut().current_exception, JSValue::NULL)
    }
}

pub const JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL: i32 = 1 << 0;
/* only taken into account if filename is provided */
pub const JS_BACKTRACE_FLAG_SINGLE_LEVEL: i32 = 1 << 1;

#[deprecated(note = "use JSContext::get_exception() instead")]
pub unsafe fn JS_GetException(ctx: *mut JSContext) -> JSValue {
    (*ctx).get_exception()
}

/* Note: it is important that no exception is returned by this function */
pub unsafe fn is_backtrace_needed(obj: JSValueConst) -> bool {
    if JS_VALUE_GET_TAG(obj) != JSTag::Object as i32 {
        return false;
    }

    let p = JS_VALUE_GET_OBJ(obj);
    if (*p).class_id() != JS_CLASS_ERROR {
        return false;
    }
    if (*p).find_own_property1(JS_ATOM_stack).is_some() {
        return false;
    }

    true
}

/* if filename != NULL, an additional level is added with the filename
and line number information (used for parse error). */
unsafe fn inner_build_backtrace(
    ctx: *mut JSContext,
    error_obj: JSValueConst,
    filename: *const c_char,
    line_num: Option<u32>,
    mut backtrace_flags: i32,
    dbuf: &mut std::io::Cursor<&mut Vec<u8>>,
) -> std::io::Result<()> {
    if !filename.is_null() {
        // filename and line no
        let c_filename = CStr::from_ptr(filename.cast());
        dbuf.write_fmt(format_args!("    at {c_filename:?}"))?;
        if let Some(ln) = line_num {
            dbuf.write_fmt(format_args!(":{ln}"))?;
        }
        dbuf.write_all(b"\n")?;

        let str = (*ctx).create_string(c_filename.to_string_lossy().as_ref());
        let _ = error_obj.define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_fileName,
            str,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );
        let _ = error_obj.define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_lineNumber,
            JSValue::from_i32(line_num.map_or(-1, |x| x as i32)),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );

        if backtrace_flags & JS_BACKTRACE_FLAG_SINGLE_LEVEL != 0 {
            return Ok(());
        }
    }

    let mut sf = (*ctx).rt().current_stack_frame;
    while !sf.is_null() {
        if backtrace_flags & JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL != 0 {
            backtrace_flags &= !JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL;
            sf = (*sf).prev_frame;
            continue;
        }

        let str1 = get_func_name(ctx, (*sf).cur_func).map_or(Cow::Borrowed("<anonymous>"), |x| {
            if x.len() == 0 {
                Cow::Borrowed("<anonymous>")
            } else {
                x
            }
        });
        dbuf.write_fmt(format_args!("    at {}", str1))?;

        let p = JS_VALUE_GET_OBJ((*sf).cur_func);
        let mut backtrace_barrier = false;

        if js_class_has_bytecode((*p).class_id()) {
            let b = (*p).u.func.function_bytecode;
            backtrace_barrier = (*b).backtrace_barrier;

            if (*b).has_debug {
                let line_num1 = (*b).find_line_num(
                    ((*sf).cur_pc_offset as u32).wrapping_sub(1), // be u32::MAX when (*sf).cur_pc_offset==0
                );

                // let atom_str = JS_AtomToCString(ctx, (*b).debug.filename);
                let atom_str = (*b)
                    .debug
                    .filename
                    .to_utf8_string(ctx.into())
                    .unwrap_or(Cow::Borrowed("<null>"));

                dbuf.write_fmt(format_args!(" ({}", atom_str.as_ref()))?;
                if let Some(n) = line_num1 {
                    dbuf.write_fmt(format_args!(":{n}"))?;
                }
                dbuf.write_all(b")")?;
            }
        } else {
            dbuf.write_all(b" (native)")?;
        }
        dbuf.write_all(b"\n")?;

        /* stop backtrace if JS_EVAL_FLAG_BACKTRACE_BARRIER was used */
        if backtrace_barrier {
            break;
        }

        sf = (*sf).prev_frame;
    }

    Ok(())
}

/* if filename != NULL, an additional level is added with the filename
and line number information (used for parse error). */
pub unsafe fn build_backtrace(
    ctx: *mut JSContext,
    error_obj: JSValueConst,
    filename: *const c_char,
    line_num: Option<u32>,
    backtrace_flags: i32,
) {
    // TODO: use SmallVec?
    let mut buf = Vec::<u8>::with_capacity(128);

    let ret = {
        let mut dbuf = std::io::Cursor::new(&mut buf);
        let r = inner_build_backtrace(
            ctx,
            error_obj,
            filename,
            line_num,
            backtrace_flags,
            &mut dbuf,
        );

        r
    };

    let _ = error_obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_stack,
        if ret.is_err() {
            JSValue::NULL
        } else {
            (*ctx).create_string(std::str::from_utf8_unchecked(buf.as_slice()))
        },
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    );
}

// /// TODO: remove using libc
// #[deprecated(note = "use JSContext::JS_ThrowTypeErr()")]
// pub unsafe fn __JS_ThrowTypeErrorAtom(ctx: *mut JSContext, fmt: &CStr, atom: JSAtom) -> JSValue {
//     let mut buf = [0u8; ATOM_GET_STR_BUF_SIZE];

//     libc::snprintf(
//         buf.as_mut_ptr() as *mut libc::c_char,
//         buf.len(),
//         fmt.as_ptr() as *const libc::c_char,
//         atom.to_str((*ctx).rt()),
//     );
//     (*ctx).JS_ThrowTypeError(CStr::from_bytes_with_nul(buf.as_slice()).unwrap())
// }

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

pub unsafe fn JS_ThrowSyntaxErrorAtom(ctx: *mut JSContext, fmt: &CStr, atom: JSAtom) -> JSValue {
    __JS_ThrowSyntaxErrorAtom(ctx, fmt, atom)
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

pub unsafe fn JS_ThrowTypeErrorDetachedArrayBuffer(ctx: *mut JSContext) -> JSValue {
    ctx.as_mut()
        .unwrap()
        .JS_ThrowTypeError(c"ArrayBuffer is detached")
}

pub unsafe fn JS_ThrowTypeErrorPrivateNotFound(ctx: *mut JSContext, atom: JSAtom) -> JSValue {
    (*ctx).JS_ThrowTypeError(CStr::from_ptr(
        format!(
            "private class field '{}' does not exist\0",
            atom.to_str((*ctx).rt())
        )
        .as_ptr()
        .cast(),
    ))
}

pub unsafe fn JS_ThrowRangeError(ctx: *mut JSContext, message: &CStr) -> JSValue {
    ctx.as_mut()
        .unwrap()
        .JS_ThrowError(JSErrorEnum::Range, message)
}

pub unsafe fn JS_ThrowSyntaxError(ctx: *mut JSContext, message: &CStr) -> JSValue {
    ctx.as_mut()
        .unwrap()
        .JS_ThrowError(JSErrorEnum::Syntax, message)
}

pub unsafe fn JS_ThrowSyntaxErrorVarRedeclaration(ctx: *mut JSContext, prop: JSAtom) -> JSValue {
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
    if !JS_IsObject(this_val) {
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
        msg = JS_ToStringFree(ctx, msg);
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
