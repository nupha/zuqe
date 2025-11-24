use std::{borrow::Cow, io::Write};

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CLASS_ERROR, JS_PROP_STRING_DEF,
    atom::Atom,
    class::ClassID,
    context::{ContextImpl, ContextRef},
    function::{Bytecode, CallContext, FieldDef, get_func_name},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_WRITABLE, ObjectPayload,
    },
    stack_frame::StackFrameParentIter,
    string::{JS_ConcatString, JS_ConcatString3},
    value::JSValue,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ErrorKind {
    Eval = 0,
    Range,
    Reference,
    Syntax,
    Type,
    Uri,
    Internal,
    Aggregate,
}

pub type JSResult = Result<JSValue, i32>;

pub(crate) const STD_ERROR_COUNT: usize = ErrorKind::Aggregate as usize + 1;
pub(crate) const STD_ERROR_NAMES: [&str; STD_ERROR_COUNT] = [
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

impl ContextImpl {
    /// put `obj` to current_exception, free old exception.
    /// Note: `obj` is consumed.
    #[inline(never)]
    #[cold]
    pub fn throw(&self, obj: JSValue) {
        // strict debug guard: don't allow to throw null or undefined.
        // #[cfg(debug_assertions)]
        // if obj.is_undefined_or_null() {
        //     panic!("throw a null or undefined value is not recommended");
        // }

        self.rt().set_exception(obj);
    }

    /// make error object and throw it as current exception.
    /// old exception will be freed.
    #[cold]
    pub(crate) fn throw_error_ex(&self, kind: ErrorKind, message: &str, add_backtrace: bool) {
        let proto = self.native_error_proto[kind as usize].as_object().cloned();
        let exc = if let Ok(mut o) = self.create_object_with_proto(proto, ObjectPayload::ERROR) {
            let s = self.create_string_from_str(message);
            let _ = o.define_property_value(
                self,
                Atom::CONST_message,
                s.map_or(JSValue::EXCEPTION, JSValue::String),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            );

            if add_backtrace {
                self.build_backtrace_to(o.into(), None, 0);
            }

            o.into()
        } else {
            // out of memory: throw NULL to avoid recursing
            JSValue::NULL
        };

        self.throw(exc);
    }

    /// make error object and throw it as current exception.
    /// old exception will be freed.
    #[cold]
    pub fn throw_error(&self, kind: ErrorKind, message: &str) {
        let rt = self.rt_ref();

        // the backtrace is added later if called from a bytecode function
        let add_backtrace = !rt.in_out_of_memory
            && rt
                .current_stack_frame()
                .is_none_or(|sf| sf.cur_func.as_object_BCFunction().is_some());

        self.throw_error_ex(kind, message, add_backtrace);
    }

    #[inline(always)]
    pub fn throw_range_error(&self, message: &str) {
        self.throw_error(ErrorKind::Range, message);
    }

    #[inline(always)]
    pub fn throw_internal_error(&self, message: &str) {
        self.throw_error(ErrorKind::Internal, message);
    }

    #[inline(always)]
    pub fn throw_stack_overflow(&self) {
        self.throw_internal_error("stack overflow");
    }

    #[inline(always)]
    pub fn throw_type_error(&self, message: &str) {
        self.throw_error(ErrorKind::Type, message);
    }

    /// test if allowed to throw error according to `flags`
    #[inline(always)]
    pub fn can_throw(&self, flags: u32) -> bool {
        flags & JS_PROP_THROW != 0 || (flags & JS_PROP_THROW_STRICT != 0 && self.is_strict_mode())
    }

    /// returns: true=exception raised; false=no exception raised
    pub fn throw_type_error_or_false(&self, flags: u32, message: &str) -> bool {
        if self.can_throw(flags) {
            self.throw_type_error(message);
            true
        } else {
            false
        }
    }

    #[inline(always)]
    pub fn throw_type_error_not_an_object(&self) {
        self.throw_type_error("not an object");
    }

    #[inline(always)]
    pub fn throw_type_error_not_a_symbol(&self) {
        self.throw_type_error("not a symbol");
    }

    #[inline(always)]
    pub fn throw_type_error_revoked_proxy(&self) {
        self.throw_type_error("revoked proxy");
    }

    #[cold]
    pub fn throw_type_error_invalid_class(&self, class_id: ClassID) {
        let name = self.rt().class_registry[class_id as usize].class_name;
        self.throw_type_error(&format!("{} object expected", name.to_utf8()))
    }

    /// atom is not consumed
    pub fn throw_type_error_read_only(&self, atom: Atom) {
        self.throw_type_error(&format!("'{}' is read-only", atom.to_utf8()))
    }

    #[inline(always)]
    pub fn throw_reference_error(&self, message: &str) {
        self.throw_error(ErrorKind::Reference, message)
    }

    #[inline(always)]
    pub fn throw_syntax_error(&self, message: &str) {
        self.throw_error(ErrorKind::Syntax, message);
    }

    #[cold]
    pub fn throw_out_of_memory(&self) {
        let mut rt = self.rt();
        if !rt.in_out_of_memory {
            rt.in_out_of_memory = true;
            self.throw_internal_error("out of memory");
            rt.in_out_of_memory = false;
        }
    }
}

pub const JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL: i32 = 1 << 0;
/* only taken into account if filename is provided */
pub const JS_BACKTRACE_FLAG_SINGLE_LEVEL: i32 = 1 << 1;

/* Note: it is important that no exception is returned by this function */
#[cold]
pub fn is_backtrace_needed(obj: JSValue) -> bool {
    if let JSValue::Object(p) = obj {
        if p.class_id() != JS_CLASS_ERROR {
            false
        } else {
            !p.has_shape_property(Atom::CONST_stack)
        }
    } else {
        false
    }
}

impl ContextImpl {
    #[cold]
    fn do_build_backtrace(
        &self,
        mut error_obj: JSValue,
        file_line: Option<(&str, i32)>, // filename and line number
        mut backtrace_flags: i32,
    ) -> Result<String, i32> {
        let mut buf = Vec::<u8>::with_capacity(128);
        let mut dbuf = std::io::Cursor::new(&mut buf);

        if let Some((filename, line_num)) = file_line {
            // filename and line no
            dbuf.write_fmt(format_args!("    at {filename}:{line_num}\n"))
                .map_err(|_| -1)?;

            let str = self.create_string_from_str(filename);
            error_obj.define_property_value(
                self,
                Atom::CONST_fileName,
                str.map_or(JSValue::EXCEPTION, JSValue::String),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;
            error_obj.define_property_value(
                self,
                Atom::CONST_lineNumber,
                JSValue::from(line_num),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;

            if backtrace_flags & JS_BACKTRACE_FLAG_SINGLE_LEVEL != 0 {
                return Ok(String::from_utf8_lossy(dbuf.into_inner()).to_string());
            }
        }

        for sf in self
            .rt()
            .current_stack_frame()
            .map_or(StackFrameParentIter::EMPTY, |f| f.parent_iter(true))
        {
            if backtrace_flags & JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL != 0 {
                backtrace_flags &= !JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL;
            } else {
                let str1 = get_func_name(sf.cur_func).map_or(Cow::Borrowed("<anonymous>"), |x| {
                    if x.is_empty() {
                        Cow::Borrowed("<anonymous>")
                    } else {
                        Cow::Owned(x)
                    }
                });
                dbuf.write_fmt(format_args!("    at {}", str1))
                    .map_err(|_| -1)?;

                let mut backtrace_barrier = false;

                if let Some(bc) = sf
                    .cur_func
                    .as_object_BCFunction()
                    .map(|f| f.payload().bytecode())
                {
                    backtrace_barrier = bc.attrs.backtrace_barrier;

                    if let Some(debug) = bc.debug.as_ref() {
                        let line_num1 = bc.find_line_num(
                            (sf.cur_pc_offset as u32).wrapping_sub(1), // be u32::MAX when (*sf).cur_pc_offset==0
                        );

                        let atom_str = debug.filename.to_utf8();

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
            }
        }

        Ok(String::from_utf8_lossy(dbuf.into_inner()).to_string())
    }

    /// build traceback, set it as property of `error_obj`.
    /// if filename/line number is provided, they are defined as property of `error_obj` too.
    /// `error_obj` is not consumed.
    #[cold]
    pub fn build_backtrace_to(
        &self,
        mut target: JSValue,
        file_line: Option<(&str, i32)>,
        backtrace_flags: i32,
    ) {
        let tb_val = if let Ok(s) = self.do_build_backtrace(target, file_line, backtrace_flags) {
            self.create_string_from_utf8(s.into_bytes())
                .map_or(JSValue::UNDEFINED, JSValue::String)
        } else {
            JSValue::NULL
        };

        let _ = target.define_property_value(
            self,
            Atom::CONST_stack,
            tb_val,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );
    }
}

pub fn JS_ThrowReferenceErrorUninitialized(ctx: &ContextImpl, name: Atom) {
    if name.is_null() {
        ctx.throw_reference_error("lexical variable is not initialized");
    } else {
        ctx.throw_reference_error(&format!("{} is not initialized", name.to_utf8()));
    }
}

pub fn JS_ThrowReferenceErrorNotDefined(ctx: &ContextImpl, name: Atom) {
    ctx.throw_reference_error(&format!("'{}' is not defined", name.to_utf8()))
}

pub fn JS_ThrowReferenceErrorUninitialized2(
    ctx: &ContextImpl,
    b: &Bytecode,
    idx: u16,
    is_ref: bool,
) {
    let atom = if is_ref {
        b.get_closure_var(idx).map(|c| c.var_name)
    } else {
        // not present if the function is stripped and contains no eval()
        b.get_var(idx).map(|vd| vd.var_name)
    }
    .unwrap_or(Atom::NULL);

    JS_ThrowReferenceErrorUninitialized(ctx, atom);
}

pub fn JS_ThrowTypeErrorDetachedArrayBuffer(ctx: &ContextImpl) {
    ctx.throw_type_error("ArrayBuffer is detached");
}

pub fn JS_ThrowTypeErrorPrivateNotFound(ctx: &ContextImpl, atom: Atom) {
    ctx.throw_type_error(&format!(
        "private class field '{}' does not exist",
        atom.to_utf8()
    ));
}

pub fn JS_ThrowSyntaxErrorVarRedeclaration(ctx: &ContextImpl, prop: Atom) {
    ctx.throw_syntax_error(&format!("redeclaration of '{}'", prop.to_utf8()));
}

/// used to avoid catching interrupt exceptions
pub unsafe fn JS_IsUncatchableError(val: JSValue) -> bool {
    if let JSValue::Object(p) = val {
        p.class_id() == JS_CLASS_ERROR && p.flags().is_uncatchable_error()
    } else {
        false
    }
}

pub fn JS_SetUncatchableError(val: JSValue, b: bool) {
    if let JSValue::Object(mut p) = val
        && p.class_id() == JS_CLASS_ERROR
    {
        p.set_flag_uncatchable_error(b);
    }
}

pub fn js_error_toString(ctx: ContextRef, cc: CallContext) -> JSResult {
    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let name = cc.this().get_property(&ctx, Atom::CONST_name)?;
    let mut name = if name.is_undefined() {
        Atom::CONST_Error.force_string(&ctx)?
    } else {
        name.to_string(&ctx)?
    };

    let msg = cc.this().get_property(&ctx, Atom::CONST_message)?;
    let msg = if msg.is_undefined() {
        Atom::CONST_empty_string.force_string(&ctx)?
    } else {
        msg.to_string(&ctx)?
    };

    if !name.is_empty() && !msg.is_empty() {
        name = JS_ConcatString3(&ctx, c"", name.into(), c": ")?;
    }

    JS_ConcatString(&ctx, name.into(), msg.into()).map(JSValue::String)
}

pub static js_error_proto_funcs: [FieldDef; 3] = [
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_error_toString, 0),
    JS_PROP_STRING_DEF!("name", "Error", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
    JS_PROP_STRING_DEF!("message", "", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
];
