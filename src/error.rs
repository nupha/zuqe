// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{borrow::Cow, io::Write, process::ExitCode};

use crate::{
    Context, JS_CLASS_ERROR, Object, ScopedContext,
    atom::Atom,
    class::ClassID,
    context::ContextImpl,
    function::{Bytecode, get_func_name},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, PayloadPtr},
    value::JSValue,
};

pub type ErrCode = i32;

#[repr(u8)]
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum Exception {
    /// No exception
    #[default]
    No,
    /// Out of memory in runtime level
    OutOfMemory,
    /// Too many classes registered
    TooManyClasses,
    /// User atom table exhausted (cannot allocate a new atom)
    TooManyAtoms,
    /// JavaScript error value from one of context
    Error(JSValue),
}

/// Result of the complete event loop execution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExitStatus {
    /// A value produced by the executed script.
    /// Use Value(JSValue::UNDEFINED) for normal stop with no meaningful value.
    Value(JSValue),
    /// Process was interrupted via interrupt().
    Interrupted(ExitCode),
    /// An unrecoverable runtime exception occurred.
    Exception(Exception),
}

impl Exception {
    pub fn is_uncatchable(&self) -> bool {
        match self {
            Self::Error(v) => v.as_object().is_some_and(|o| {
                o.class_id() == JS_CLASS_ERROR && o.flags().is_uncatchable_error()
            }),
            Self::No => false,
            _ => true,
        }
    }

    pub fn set_uncatchable(error: JSValue, is_uncatchable: bool) {
        if let Some(mut o) = error.as_object()
            && o.class_id() == JS_CLASS_ERROR
        {
            o.set_flag_uncatchable_error(is_uncatchable);
        }
    }

    pub fn to_text(self, ctx: &mut ScopedContext) -> ExceptionText {
        match self {
            Self::No => ExceptionText {
                message: String::new(),
                traceback: None,
            },
            Self::OutOfMemory => ExceptionText {
                message: String::from("Out of memory"),
                traceback: None,
            },
            Self::TooManyClasses => ExceptionText {
                message: String::from("Too many classes"),
                traceback: None,
            },
            Self::TooManyAtoms => ExceptionText {
                message: String::from("Too many atoms"),
                traceback: None,
            },
            Self::Error(e) => {
                let mut t = ExceptionText {
                    message: e
                        .to_string(ctx)
                        .map_or(String::from("<can't get error message>"), |s| {
                            s.content().encode_utf8(false).to_string()
                        }),
                    traceback: None,
                };

                if let Ok(s) = e.get_property(ctx, Atom::CONST_stack)
                    && !s.is_undefined_or_null()
                    && let Some(ss) = s.as_string(false)
                {
                    t.traceback
                        .replace(ss.content().encode_utf8(false).to_string());
                }

                t
            }
        }
    }

    /// align exception to jsvalue
    pub fn to_value(self, ctx: &mut ScopedContext) -> JSValue {
        match self {
            Self::No => JSValue::UNDEFINED,
            Self::OutOfMemory => ctx
                .create_string_interned("out of memory")
                .map_or(JSValue::from(-1), JSValue::make_string),
            Self::TooManyClasses => ctx
                .create_string_interned("too many classes")
                .map_or(JSValue::from(-1), JSValue::make_string),
            Self::TooManyAtoms => ctx
                .create_string_interned("too many atoms")
                .map_or(JSValue::from(-1), JSValue::make_string),
            Self::Error(v) => v,
        }
    }
}

/// Represent exception in text
#[derive(Debug)]
pub struct ExceptionText {
    pub message: String,
    pub traceback: Option<String>,
}

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

pub type JSResult = Result<JSValue, ErrCode>;

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
    #[inline(always)]
    pub fn clear_exception(&mut self) {
        self.rt_mut().clear_exception();
    }
}

impl<'c> ScopedContext<'c> {
    /// Create Error object.
    /// Return None if can't create error object, usually due ot out of memory.
    #[cold]
    pub fn create_error_object(
        &mut self,
        kind: ErrorKind,
        message: &str,
        add_backtrace: bool,
    ) -> Option<Object> {
        let proto = self.error_protos[kind as usize].as_object();
        if let Ok(mut err_obj) =
            self.create_instance_with_proto(proto, PayloadPtr::none(JS_CLASS_ERROR))
        {
            let s = self.create_string_from_utf8(message.as_bytes().to_vec());
            let _ = err_obj.define_property_value(
                self,
                Atom::CONST_message,
                s.map_or(JSValue::EXCEPTION, JSValue::make_string),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            );

            if add_backtrace {
                self.build_backtrace_to(err_obj.into(), None, 0);
            }

            Some(err_obj)
        } else {
            None // out of memory
        }
    }

    /// Throw an exception.
    ///
    /// For value exception: the value is removed from current scope (its LOCAL flag is cleared)
    /// and stored in Runtime's cur_exception.
    ///
    /// The old exception (if any) is simply replaced — it needs no protection:
    /// if other GC references exist, GC will trace it; otherwise it should be collected.
    pub fn throw(&mut self, exc: Exception) {
        if let Exception::Error(val) = &exc
            && let Some(mut n) = val.as_gc_node()
        {
            // If the node is LOCAL, remove it from the current scope cache
            // and clear the LOCAL flag so the exception slot keeps it alive.
            //
            // Search from current scope upward through ancestors.
            // For cross-realm errors (e.g. `throw new otherRealm.TypeError()`),
            // the node is LOCAL on a *different* scope stack and won't be
            // found here — clear the LOCAL flag via GcHead::clear_local_flag().
            // The node stays protected by the other scope's cache; it will be
            // cleaned up when that scope is eventually dropped.
            if unsafe { n.as_ref().is_local() } {
                let mut found = false;
                let mut scope = Some(self.gc_state()); // start from self.gc() - the most-inner scope.
                while let Some(s) = scope {
                    if s.remove_node(n) {
                        found = true;
                        break;
                    }
                    scope = s.parent().map(|(p, _)| p);
                }

                if !found {
                    // Cross-realm: node lives on another scope stack.
                    // Clear LOCAL flag so set_exception won't assert.
                    unsafe {
                        n.as_mut().clear_local_flag();
                    }
                }
            }
        }

        self.rt_mut().set_exception(exc);
    }

    /// Throw a JS value as exception.
    #[must_use]
    #[inline(always)]
    pub(crate) fn throw_value(&mut self, val: JSValue) -> ErrCode {
        self.throw(Exception::Error(val));
        -1
    }

    /// make error object and throw it as current exception.
    #[must_use]
    #[inline(never)]
    #[cold]
    pub(crate) fn throw_error_internal(
        &mut self,
        kind: ErrorKind,
        message: &str,
        add_backtrace: bool,
    ) -> ErrCode {
        if let Some(err_obj) = self.create_error_object(kind, message, add_backtrace) {
            self.throw_value(err_obj.into())
        } else {
            // out of memory: set exception directly to avoid recursing
            self.rt_mut().set_exception(Exception::OutOfMemory);
            -1
        }
    }

    /// test if throw error should add backtrace
    pub(crate) fn throw_need_backtrace(&self) -> bool {
        let rt = self.rt();
        // In stackless (arena) mode, all bytecode execution goes through the
        // trampoline — active_trampoline_frame is non-null whenever we are
        // executing JS bytecode (including native callbacks called from it).
        rt.exception() != Exception::OutOfMemory && !rt.active_frame_index.is_null()
    }

    /// make error object and throw it as current exception.
    /// old exception will be freed.
    #[must_use]
    pub fn throw_error(&mut self, kind: ErrorKind, message: &str) -> ErrCode {
        self.throw_error_internal(kind, message, self.throw_need_backtrace())
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_range_error(&mut self, message: &str) -> ErrCode {
        self.throw_error(ErrorKind::Range, message)
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_internal_error(&mut self, message: &str) -> ErrCode {
        self.throw_error(ErrorKind::Internal, message)
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_stack_overflow(&mut self) -> ErrCode {
        self.throw_internal_error("stack overflow")
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_stack_fail(&mut self) -> ErrCode {
        self.throw_internal_error("execution stack allocation failed")
    }

    #[cold]
    #[inline(never)]
    #[must_use]
    pub fn throw_stack_exceeded(&mut self) -> ErrCode {
        // Do NOT build a backtrace here: this is thrown from a CALL instruction
        // mid-dispatch (pc still points at CALL, sp holds unconsumed args), so
        // the frame state is not clean. Walking the deep virtual stack for a
        // backtrace would read freed/garbage frame data and produce bogus
        // "invalid opcode" panics. The error message already states the cause,
        // so we mark the error with a `stack` property (empty) to stop any
        // later backtrace pass (is_backtrace_needed) from re-walking the frames.
        if let Some(mut err_obj) = self.create_error_object(
            ErrorKind::Internal,
            "maximum call stack size exceeded",
            false,
        ) {
            let stack_val = Atom::CONST_empty_string
                .force_string(self)
                .map_or(JSValue::UNDEFINED, JSValue::make_string);
            let _ = err_obj.define_property_value(
                self,
                Atom::CONST_stack,
                stack_val,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            );
            self.throw_value(err_obj.into())
        } else {
            self.rt_mut().set_exception(Exception::OutOfMemory);
            -1
        }
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_type_error(&mut self, message: &str) -> ErrCode {
        self.throw_error(ErrorKind::Type, message)
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_uri_error(&mut self, message: &str) -> ErrCode {
        self.throw_error(ErrorKind::Uri, message)
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_type_error_not_an_object(&mut self) -> ErrCode {
        self.throw_type_error("not an object")
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_type_error_not_a_symbol(&mut self) -> ErrCode {
        self.throw_type_error("not a symbol")
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_type_error_revoked_proxy(&mut self) -> ErrCode {
        self.throw_type_error("revoked proxy")
    }

    #[must_use]
    pub fn throw_type_error_invalid_class(&mut self, class_id: ClassID) -> ErrCode {
        let name = self.rt_ptr().class_registry[class_id as usize].name_atom;
        self.throw_type_error(&format!("{} object expected", name.to_utf8(self.rt())))
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_type_error_read_only(&mut self, atom: Atom) -> ErrCode {
        self.throw_type_error(&format!("'{}' is read-only", atom.to_utf8(self.rt())))
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_reference_error(&mut self, message: &str) -> ErrCode {
        self.throw_error(ErrorKind::Reference, message)
    }

    #[must_use]
    #[inline(always)]
    pub fn throw_syntax_error(&mut self, message: &str) -> ErrCode {
        self.throw_error(ErrorKind::Syntax, message)
    }

    #[must_use]
    #[cold]
    pub fn throw_out_of_memory(&mut self) -> ErrCode {
        if self.rt().exception() != Exception::OutOfMemory {
            self.throw_internal_error("out of memory")
        } else {
            -1
        }
    }

    #[must_use]
    #[cold]
    pub(crate) fn throw_reference_error_uninitialized2(
        &mut self,
        b: &Bytecode,
        idx: u16,
        is_ref: bool,
    ) -> ErrCode {
        let atom = if is_ref {
            b.get_closure_var(idx).map(|c| c.var_name)
        } else {
            // not present if the function is stripped and contains no eval()
            b.get_var(idx).map(|vd| vd.var_name)
        }
        .unwrap_or(Atom::NULL);

        JS_ThrowReferenceErrorUninitialized(self, atom)
    }

    #[inline(never)]
    #[cold]
    fn do_build_backtrace(
        &mut self,
        error_obj: Option<Object>,
        file_line: Option<(&str, i32, u32)>, // filename, line number, column
        mut backtrace_flags: i32,
    ) -> Result<String, ErrCode> {
        let mut buf = Vec::<u8>::with_capacity(128);
        let mut dbuf = std::io::Cursor::new(&mut buf);

        if let Some((filename, line_num, col)) = file_line {
            // filename, line no and column
            dbuf.write_fmt(format_args!("    at {filename}:{line_num}:{col}\n"))
                .map_err(|_| -1)?;

            if let Some(mut obj) = error_obj {
                let str = self.create_string_from_utf8(filename.as_bytes().to_vec());
                obj.define_property_value(
                    self,
                    Atom::CONST_fileName,
                    str.map_or(JSValue::UNDEFINED, JSValue::make_string),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;
                obj.define_property_value(
                    self,
                    Atom::CONST_lineNumber,
                    JSValue::from(line_num),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;
            }

            if backtrace_flags & JS_BACKTRACE_FLAG_SINGLE_LEVEL != 0 {
                return Ok(String::from_utf8_lossy(dbuf.into_inner()).to_string());
            }
        }

        let rt = self.rt_ptr();

        // ── Arena mode: traverse FrameHead.prev chain (if active) ──
        if !rt.active_frame_index.is_null() {
            let arena = &rt.frame_arena;
            let mut frame_idx = rt.active_frame_index;
            let mut first = true;
            while !frame_idx.is_null() {
                let head = arena.frame(frame_idx);
                if backtrace_flags & JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL != 0 && first {
                    backtrace_flags &= !JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL;
                } else {
                    let str1 =
                        get_func_name(head.func_obj).map_or(Cow::Borrowed("<anonymous>"), |x| {
                            if x.is_empty() {
                                Cow::Borrowed("<anonymous>")
                            } else {
                                Cow::Owned(x)
                            }
                        });
                    dbuf.write_fmt(format_args!("    at {}", str1))
                        .map_err(|_| -1)?;

                    let mut backtrace_barrier = false;

                    if let Some(bc) = head.func_obj.as_BCWrapper().map(|f| f.payload().bytecode()) {
                        backtrace_barrier = bc.attrs.backtrace_barrier;

                        if let Some(debug) = bc.debug.as_ref() {
                            let line_col =
                                bc.find_line_num_col((head.cur_pc_offset as u32).wrapping_sub(1));
                            let atom_str = debug.filename.to_utf8(&rt);

                            dbuf.write_fmt(format_args!(" ({atom_str}"))
                                .map_err(|_| -1)?;
                            if let Some((ln, col)) = line_col {
                                dbuf.write_fmt(format_args!(":{ln}:{col}"))
                                    .map_err(|_| -1)?;
                            }
                            dbuf.write_all(b")").map_err(|_| -1)?;
                        }
                    } else {
                        dbuf.write_all(b" (native)").map_err(|_| -1)?;
                    }
                    dbuf.write_all(b"\n").map_err(|_| -1)?;

                    if backtrace_barrier {
                        break;
                    }
                }
                frame_idx = head.prev;
                first = false;
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
        file_line: Option<(&str, i32, u32)>, // filename, line number, column
        backtrace_flags: i32,
    ) {
        let traceback = if let Ok(s) =
            self.do_build_backtrace(target.as_object(), file_line, backtrace_flags)
        {
            self.create_string_from_utf8(s.into_bytes())
                .map_or(JSValue::UNDEFINED, JSValue::make_string)
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

    /// Build the current call stack as a string without allocating an `Error`
    /// object or going through the `Error.prototype.stack` getter. Used by
    /// host-side facilities (e.g. `console.trace`) that need the live stack.
    /// Passes no error object, so the returned text contains only the stack
    /// frames (no `fileName`/`lineNumber` props). Returns `None` when no JS
    /// frame is active (e.g. called from pure native context).
    #[inline(never)]
    #[cold]
    pub fn backtrace(&mut self) -> Option<String> {
        self.do_build_backtrace(None, None, 0).ok()
    }

    /// Take the current exception and add it to `self` scope for protection.
    ///
    /// After this call, the exception value (if any) becomes a LOCAL node
    /// in the `self` scope and will be protected from GC until the scope ends.
    pub fn take_exception(&mut self) -> Exception {
        let exc = std::mem::take(&mut self.rt_mut().cur_exception);
        if let Exception::Error(val) = &exc
            && let Some(n) = val.as_gc_node()
        {
            self.gc_state().add_non_local(n);
        }
        exc
    }

    /// Take the current exception as text
    pub fn take_exception_as_text(&mut self) -> ExceptionText {
        let exc = std::mem::take(&mut self.rt_mut().cur_exception);
        exc.to_text(self)
    }

    /// Take the current exception as JSValue
    pub fn take_exception_as_value(&mut self) -> JSValue {
        let exc = std::mem::take(&mut self.rt_mut().cur_exception);
        exc.to_value(self)
    }
}

impl<'c> Context<'c> {
    /// Take the current exception and add it to root scope for protection.
    ///
    /// After this call, the exception value (if any) becomes a LOCAL node
    /// in the root scope and will be protected from GC until the scope ends.
    pub fn take_exception(&mut self) -> Exception {
        self.root_scope().take_exception()
    }

    /// Take the current exception as text
    pub fn take_exception_as_text(&mut self) -> ExceptionText {
        self.root_scope().take_exception_as_text()
    }

    /// Take the current exception as JSValue
    pub fn take_exception_as_value(&mut self) -> JSValue {
        self.root_scope().take_exception_as_value()
    }
}

pub(crate) const JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL: i32 = 1 << 0;
/// only taken into account if filename is provided
pub(crate) const JS_BACKTRACE_FLAG_SINGLE_LEVEL: i32 = 1 << 1;

/// Note: it is important that no exception is returned by this function
#[cold]
pub(crate) fn is_backtrace_needed(obj: JSValue) -> bool {
    if let Some(p) = obj.as_object() {
        if p.class_id() != JS_CLASS_ERROR {
            false
        } else {
            !p.has_shape_property(Atom::CONST_stack)
        }
    } else {
        false
    }
}

#[must_use]
pub fn JS_ThrowReferenceErrorUninitialized(ctx: &mut ScopedContext, name: Atom) -> ErrCode {
    if name.is_null() {
        ctx.throw_reference_error("lexical variable is not initialized")
    } else {
        ctx.throw_reference_error(&format!("{} is not initialized", name.to_utf8(ctx.rt())))
    }
}

#[must_use]
pub fn JS_ThrowReferenceErrorNotDefined(ctx: &mut ScopedContext, name: Atom) -> ErrCode {
    ctx.throw_reference_error(&format!("'{}' is not defined", name.to_utf8(ctx.rt())))
}

#[must_use]
pub fn JS_ThrowTypeErrorDetachedArrayBuffer(ctx: &mut ScopedContext) -> ErrCode {
    ctx.throw_type_error("ArrayBuffer is detached")
}

#[must_use]
pub fn JS_ThrowTypeErrorPrivateNotFound(ctx: &mut ScopedContext, atom: Atom) -> ErrCode {
    ctx.throw_type_error(&format!(
        "private class field '{}' does not exist",
        atom.to_utf8(ctx.rt())
    ))
}

#[must_use]
pub fn JS_ThrowSyntaxErrorVarRedeclaration(ctx: &mut ScopedContext, prop: Atom) -> ErrCode {
    ctx.throw_syntax_error(&format!("redeclaration of '{}'", prop.to_utf8(ctx.rt())))
}
