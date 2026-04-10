// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{borrow::Cow, io::Write};

use crate::{
    JS_CLASS_ERROR, ScopedContext,
    atom::Atom,
    class::ClassID,
    context::ContextImpl,
    function::{Bytecode, get_func_name},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, ObjectPayload},
    stack_frame::StackFrameParentIter,
    value::JSValue,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorKind {
    Eval,
    Range,
    Reference,
    Syntax,
    Type,
    Uri,
    Internal,
    Aggregate,
}

/// Represent error in text
pub struct ErrorText {
    pub message: String,
    pub traceback: Option<String>,
}

impl ErrorText {
    /// create error text info from `value`
    pub fn from(ctx: &mut ScopedContext, value: JSValue) -> Self {
        let mut ret = Self {
            message: value.to_string(ctx).map_or("".to_string(), |s| {
                s.content().encode_utf8(false).to_string()
            }),
            traceback: None,
        };

        match value.get_property(ctx, Atom::CONST_stack) {
            Ok(JSValue::NULL) | Ok(JSValue::UNDEFINED) => {}
            Ok(s) => {
                if let Ok(ss) = s.inspect(ctx) {
                    ret.traceback.replace(ss);
                }
            }
            Err(_) => {}
        }

        ret
    }
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

const ERR_RUNTIME_BASE: i32 = 100;
pub const ERR_OUT_OF_MEMORY: i32 = ERR_RUNTIME_BASE;
pub const ERR_TOO_MANY_CLASSES: i32 = ERR_RUNTIME_BASE + 1;

impl ContextImpl {
    /// put `obj` to current_exception.
    #[cold]
    pub(crate) fn throw(&mut self, obj: JSValue) {
        self.rt_mut().set_exception(obj);
    }
}

impl<'c> ScopedContext<'c> {
    /// make error object and throw it as current exception.
    #[inline(never)]
    #[cold]
    pub(crate) fn throw_error_internal(
        &mut self,
        kind: ErrorKind,
        message: &str,
        add_backtrace: bool,
    ) {
        let proto = self.error_protos[kind as usize].as_object().copied();
        let exc_val = if let Ok(mut o) = self.create_object_with_proto(proto, ObjectPayload::ERROR)
        {
            let s = self.create_string_from_str(message);
            let _ = o.define_property_value(
                self,
                Atom::CONST_message,
                s.map_or(JSValue::EXCEPTION, JSValue::from),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            );

            if add_backtrace {
                self.build_backtrace_to(o.into(), None, 0);
            }

            o.into()
        } else {
            // out of memory: throw error code ERR_OUT_OF_MEMORY to avoid recursing
            JSValue::Int(ERR_OUT_OF_MEMORY)
        };

        self.throw(exc_val);
    }

    /// make error object and throw it as current exception.
    /// old exception will be freed.
    #[inline(never)]
    #[cold]
    pub fn throw_error(&mut self, kind: ErrorKind, message: &str) {
        let rt = self.rt();

        // add backtrace if called from a bytecode function
        let add_backtrace = !rt.out_of_memory
            && rt
                .current_stack_frame()
                .is_none_or(|sf| sf.func_obj.as_BCWrapper().is_some());

        self.throw_error_internal(kind, message, add_backtrace);
    }

    #[inline(always)]
    pub fn throw_range_error(&mut self, message: &str) {
        self.throw_error(ErrorKind::Range, message);
    }

    #[inline(always)]
    pub fn throw_internal_error(&mut self, message: &str) {
        self.throw_error(ErrorKind::Internal, message);
    }

    #[inline(always)]
    pub fn throw_stack_overflow(&mut self) {
        self.throw_internal_error("stack overflow");
    }

    #[inline(always)]
    pub fn throw_type_error(&mut self, message: &str) {
        self.throw_error(ErrorKind::Type, message);
    }

    #[inline(always)]
    pub fn throw_uri_error(&mut self, message: &str) {
        self.throw_error(ErrorKind::Uri, message);
    }

    #[inline(always)]
    pub fn throw_type_error_not_an_object(&mut self) {
        self.throw_type_error("not an object");
    }

    #[inline(always)]
    pub fn throw_type_error_not_a_symbol(&mut self) {
        self.throw_type_error("not a symbol");
    }

    #[inline(always)]
    pub fn throw_type_error_revoked_proxy(&mut self) {
        self.throw_type_error("revoked proxy");
    }

    pub fn throw_type_error_invalid_class(&mut self, class_id: ClassID) {
        let name = self.rt_ptr().class_registry[class_id as usize].name_atom;
        self.throw_type_error(&format!("{} object expected", name.to_utf8(self.rt())))
    }

    #[inline(always)]
    pub fn throw_type_error_read_only(&mut self, atom: Atom) {
        self.throw_type_error(&format!("'{}' is read-only", atom.to_utf8(self.rt())))
    }

    #[inline(always)]
    pub fn throw_reference_error(&mut self, message: &str) {
        self.throw_error(ErrorKind::Reference, message)
    }

    #[inline(always)]
    pub fn throw_syntax_error(&mut self, message: &str) {
        self.throw_error(ErrorKind::Syntax, message);
    }

    #[cold]
    pub fn throw_out_of_memory(&mut self) {
        if !self.rt().out_of_memory {
            self.rt_mut().out_of_memory = true;
            self.throw_internal_error("out of memory");
            self.rt_mut().out_of_memory = false;
        }
    }

    #[inline(never)]
    #[cold]
    fn do_build_backtrace(
        &mut self,
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
                str.map_or(JSValue::UNDEFINED, JSValue::from),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;
            error_obj.define_property_value(
                self,
                Atom::CONST_lineNumber,
                JSValue::Int(line_num),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;

            if backtrace_flags & JS_BACKTRACE_FLAG_SINGLE_LEVEL != 0 {
                return Ok(String::from_utf8_lossy(dbuf.into_inner()).to_string());
            }
        }

        let rt = self.rt_ptr();

        for sf in rt
            .current_stack_frame()
            .map_or(StackFrameParentIter::EMPTY, |f| f.parent_iter(true))
        {
            if backtrace_flags & JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL != 0 {
                backtrace_flags &= !JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL;
            } else {
                let str1 = get_func_name(sf.func_obj).map_or(Cow::Borrowed("<anonymous>"), |x| {
                    if x.is_empty() {
                        Cow::Borrowed("<anonymous>")
                    } else {
                        Cow::Owned(x)
                    }
                });
                dbuf.write_fmt(format_args!("    at {}", str1))
                    .map_err(|_| -1)?;

                let mut backtrace_barrier = false;

                if let Some(bc) = sf.func_obj.as_BCWrapper().map(|f| f.payload().bytecode()) {
                    backtrace_barrier = bc.attrs.backtrace_barrier;

                    if let Some(debug) = bc.debug.as_ref() {
                        let line_num1 = bc.find_line_num(
                            (sf.cur_pc_offset as u32).wrapping_sub(1), // be u32::MAX when (*sf).cur_pc_offset==0
                        );
                        let atom_str = debug.filename.to_utf8(&rt);

                        dbuf.write_fmt(format_args!(" ({atom_str}"))
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
    #[inline(never)]
    #[cold]
    pub(crate) fn build_backtrace_to(
        &mut self,
        mut target: JSValue,
        file_line: Option<(&str, i32)>,
        backtrace_flags: i32,
    ) {
        let traceback = if let Ok(s) = self.do_build_backtrace(target, file_line, backtrace_flags) {
            self.create_string_from_utf8(s.into_bytes())
                .map_or(JSValue::UNDEFINED, JSValue::from)
        } else {
            JSValue::NULL
        };

        let _ = target.define_property_value(
            self,
            Atom::CONST_stack,
            traceback,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );
    }
}

pub(crate) const JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL: i32 = 1 << 0;
/* only taken into account if filename is provided */
pub(crate) const JS_BACKTRACE_FLAG_SINGLE_LEVEL: i32 = 1 << 1;

/* Note: it is important that no exception is returned by this function */
#[cold]
pub(crate) fn is_backtrace_needed(obj: JSValue) -> bool {
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

pub fn JS_ThrowReferenceErrorUninitialized(ctx: &mut ScopedContext, name: Atom) {
    if name.is_null() {
        ctx.throw_reference_error("lexical variable is not initialized");
    } else {
        ctx.throw_reference_error(&format!("{} is not initialized", name.to_utf8(ctx.rt())));
    }
}

pub fn JS_ThrowReferenceErrorNotDefined(ctx: &mut ScopedContext, name: Atom) {
    ctx.throw_reference_error(&format!("'{}' is not defined", name.to_utf8(ctx.rt())))
}

pub fn JS_ThrowReferenceErrorUninitialized2(
    ctx: &mut ScopedContext,
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

pub fn JS_ThrowTypeErrorDetachedArrayBuffer(ctx: &mut ScopedContext) {
    ctx.throw_type_error("ArrayBuffer is detached");
}

pub fn JS_ThrowTypeErrorPrivateNotFound(ctx: &mut ScopedContext, atom: Atom) {
    ctx.throw_type_error(&format!(
        "private class field '{}' does not exist",
        atom.to_utf8(ctx.rt())
    ));
}

pub fn JS_ThrowSyntaxErrorVarRedeclaration(ctx: &mut ScopedContext, prop: Atom) {
    ctx.throw_syntax_error(&format!("redeclaration of '{}'", prop.to_utf8(ctx.rt())));
}

/// used to avoid catching interrupt exceptions
pub unsafe fn JS_IsUncatchableError(val: JSValue) -> bool {
    if let JSValue::Object(p) = val {
        p.class_id() == JS_CLASS_ERROR && p.flags().is_uncatchable_error()
    } else {
        false
    }
}

pub fn JS_SetUncatchableError(error_val: JSValue, is_uncatchable: bool) {
    if let JSValue::Object(mut o) = error_val
        && o.class_id() == JS_CLASS_ERROR
    {
        o.set_flag_uncatchable_error(is_uncatchable);
    }
}
