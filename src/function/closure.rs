// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_ASYNC_GENERATOR, JS_CLASS_GENERATOR,
    atom::Atom,
    context::{ContextImpl, ContextPtr},
    object::{JS_PROP_WRITABLE, LazyInit, Object, ObjectPayload},
    stack_frame::StackFrame,
};

use super::{
    ARG_SCOPE_END, BCWrapper, BFuncPayload, Bytecode, FunctionDef, FunctionKind, JS_MAX_LOCAL_VARS,
    JSVarKindEnum, VarRef, is_var_in_arg_scope, js_function_set_properties,
};

#[derive(Debug)]
pub(crate) struct ClosureVar {
    //  is_local : 1;
    //  is_arg : 1;
    //  is_const : 1;
    //  is_lexical : 1;
    //  var_kind : 4; /* see JSVarKindEnum */
    flags: u8,

    // 0-based index
    // when flags `is_local` is set: index to a normal variable in the parent function.
    // otherwise: index to a closure variable of the parent function.
    var_idx: u16,

    pub(crate) var_name: Atom,
}

impl GcTrace for ClosureVar {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.var_name.trace(tr);
    }
}

impl ClosureVar {
    #[inline(always)]
    pub const fn is_local(&self) -> bool {
        self.flags & 0x01 != 0
    }

    pub const fn set_local(&mut self, value: bool) {
        if value {
            self.flags |= 0x01;
        } else {
            self.flags &= !0x01;
        }
    }

    #[inline(always)]
    pub const fn is_arg(&self) -> bool {
        (self.flags >> 1) & 0x01 != 0
    }

    pub const fn set_arg(&mut self, value: bool) {
        if value {
            self.flags |= 0x02;
        } else {
            self.flags &= !0x02;
        }
    }

    #[inline(always)]
    pub const fn is_const(&self) -> bool {
        (self.flags >> 2) & 0x01 != 0
    }

    pub const fn set_const(&mut self, value: bool) {
        if value {
            self.flags |= 0x04;
        } else {
            self.flags &= !0x04;
        }
    }

    #[inline(always)]
    pub const fn is_lexical(&self) -> bool {
        (self.flags >> 3) & 0x01 != 0
    }

    pub const fn set_lexical(&mut self, value: bool) {
        if value {
            self.flags |= 0x08;
        } else {
            self.flags &= !0x08;
        }
    }

    #[inline(always)]
    pub fn var_kind(&self) -> JSVarKindEnum {
        unsafe { std::mem::transmute::<u8, JSVarKindEnum>((self.flags >> 4) & 0x0F) }
    }

    pub const fn set_var_kind(&mut self, value: JSVarKindEnum) {
        self.flags = (self.flags & 0x0F) | ((value as u8) << 4);
    }

    #[inline(always)]
    pub(crate) const fn var_index(&self) -> u16 {
        self.var_idx
    }
}

impl FunctionDef {
    /// Create a closure referencing a local variable (is_local = TRUE) or a closure (is_local = FALSE) in `fd`.
    ///
    /// `fd` must be an ancestor of `self`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn get_closure_var(
        &mut self,
        ctx: &mut ContextImpl,
        fd: *const Self,
        mut is_local: bool,
        is_arg: bool,
        var_idx: u16,
        var_name: Atom,
        is_const: bool,
        is_lexical: bool,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        debug_assert!(!self.parent.is_null());

        let vidx = if fd != self.parent {
            // search var_idx along ancestor path
            let i = unsafe {
                (&mut *self.parent).get_closure_var(
                    ctx, fd, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
                )?
            };
            is_local = false;
            i
        } else {
            var_idx
        };

        // find in existing, or create if not found.
        if let Some(cvars) = self.closure_vars.as_ref()
            && let Some(i) = cvars.iter().position(|cv| {
                cv.var_index() == vidx && cv.is_arg() == is_arg && cv.is_local() == is_local
            })
        {
            Ok(i as u16)
        } else {
            self.add_closure_var(
                ctx.into(),
                is_local,
                is_arg,
                vidx,
                var_name,
                is_const,
                is_lexical,
                var_kind,
            )
        }
    }

    /// returns added closure var index, or Err for error.
    /// returned closure var index is 0-based.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_closure_var(
        &mut self,
        mut ctx: ContextPtr,
        is_local: bool,
        is_arg: bool,
        var_idx: u16,
        var_name: Atom,
        is_const: bool,
        is_lexical: bool,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        let mut cv = ClosureVar {
            flags: 0,
            var_idx,
            var_name,
        };
        cv.set_local(is_local);
        cv.set_arg(is_arg);
        cv.set_const(is_const);
        cv.set_lexical(is_lexical);
        cv.set_var_kind(var_kind);

        if let Some(lst) = self.closure_vars.as_mut() {
            if lst.len() == JS_MAX_LOCAL_VARS as usize {
                ctx.throw_internal_error("too many closure variables");
                return Err(-1);
            }
            if lst.len() == lst.capacity() {
                lst.try_reserve_exact(8).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            lst.push(cv);
            Ok(lst.len() as u16 - 1)
        } else {
            self.closure_vars.replace(vec![cv]);
            Ok(0)
        }
    }

    /// find closure var by name, returning index of the closure var
    pub fn find_closure_var(&self, var_name: Atom) -> Option<u16> {
        self.closure_vars
            .as_ref()
            .and_then(|lst| lst.iter().position(|cv| cv.var_name == var_name))
            .map(|idx| idx as u16)
    }
}

impl Bytecode {
    /// Create a function object with Bytecode and captured variables.
    pub(crate) fn make_closure(
        &self,
        ctx: &mut ContextImpl,
        proto: Option<Object>,
        captured_vars: &[VarRef],
        parent_stack_frame: Option<&mut StackFrame>,
    ) -> Result<BCWrapper, i32> {
        let captures: Vec<VarRef> = if self.closure_var_count != 0 {
            debug_assert!(
                parent_stack_frame.is_some(),
                "must have parent stack frame if there is any closure variable"
            );
            let up_sf = parent_stack_frame.unwrap();

            // initialize var_refs with closure variables
            let mut lst = Vec::new();
            lst.try_reserve_exact(self.closure_var_count as usize)
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

            for cv in self.closure_vars() {
                lst.push(if cv.is_local() {
                    // the existing variable reference in parent stack frame
                    up_sf.get_var_ref(ctx, cv.var_idx, cv.is_arg())?
                } else {
                    captured_vars[cv.var_idx as usize]
                });
            }

            lst
        } else {
            Vec::new()
        };

        let mut obj = {
            let p = BFuncPayload::new(*self, captures);
            let isa = match self.func_kind() {
                FunctionKind::Normal => ObjectPayload::BYTECODE_FUNCTION(p),
                FunctionKind::Generator => ObjectPayload::GENERATOR_FUNCTION(p),
                FunctionKind::Async => ObjectPayload::ASYNC_FUNCTION(p),
                FunctionKind::AsyncGenerator => ObjectPayload::ASYNC_GENERATOR_FUNCTION(p),
            };
            BCWrapper(
                ctx.create_object_with_proto(
                    proto.or_else(|| ctx.class_proto(isa.class_id())),
                    isa,
                )
                .map_err(|_| -1)?
                .gc_ref(),
            )
        };

        js_function_set_properties(
            ctx,
            obj.into(),
            if self.func_name.is_null() {
                Atom::CONST_empty_string
            } else {
                self.func_name
            },
            self.attrs.defined_arg_count as _,
        );

        if let Some(gen_class_id) = self.func_kind().generator_class_id() {
            // generator functions have a prototype field which is used as
            // prototype for the generator object
            debug_assert!(matches!(
                gen_class_id,
                JS_CLASS_GENERATOR | JS_CLASS_ASYNC_GENERATOR
            ));

            let proto = ctx.create_base_object_with_proto(ctx.class_proto(gen_class_id))?;
            obj.define_property_value(ctx, Atom::CONST_prototype, proto.into(), JS_PROP_WRITABLE)?;
        } else if self.has_prototype() {
            // add the 'prototype' property: delay instantiation to avoid
            // creating cycles for every javascript function. The prototype
            // object is created on the fly when first accessed
            obj.set_flag_constructor(true);

            obj.define_lazy_property(
                ctx,
                Atom::CONST_prototype,
                LazyInit::Prototype,
                JS_PROP_WRITABLE,
            )?;
        }

        Ok(obj)
    }
}

/// for direct eval compilation: add references to the variables of the calling function.
/// scope_idx=-1 means end of scope chain at top.
pub(crate) fn add_closure_variables(
    ctx: &mut ContextImpl,
    s: &mut FunctionDef,
    bc: &Bytecode,
    scope_idx: i32,
) -> Result<(), i32> {
    debug_assert!(s.closure_vars.is_none());

    let count = bc.arg_count + bc.var_count + bc.closure_var_count;
    if count == 0 {
        return Ok(());
    }

    let mut cvars = Vec::<ClosureVar>::new();
    cvars.try_reserve_exact(count as usize).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    // Add lexical variables in scope at the point of evaluation
    let vardefs = bc.local_vars();
    let mut i = scope_idx;
    while i >= 0 {
        let vd = vardefs.get(i as usize).unwrap();
        if vd.scope_level > 0 {
            let mut cv = ClosureVar {
                flags: 0,
                var_idx: i as _,
                var_name: vd.var_name,
            };
            cv.set_local(true);
            cv.set_arg(false);
            cv.set_const(vd.is_const());
            cv.set_lexical(vd.is_lexical());
            cv.set_var_kind(vd.var_kind());

            cvars.push(cv);
        }
        i = vd.scope_next;
    }

    let is_arg_scope = i == ARG_SCOPE_END;
    if !is_arg_scope {
        // Add argument variables
        for (i, vd) in bc.arguments().iter().enumerate() {
            let mut cv = ClosureVar {
                flags: 0,
                var_idx: i as _,
                var_name: vd.var_name,
            };
            cv.set_local(true);
            cv.set_arg(true);
            cv.set_const(false);
            cv.set_lexical(false);
            cv.set_var_kind(JSVarKindEnum::JS_VAR_NORMAL);

            cvars.push(cv);
        }

        // Add local non lexical variables
        for (i, vd) in bc.local_vars().iter().enumerate() {
            if vd.scope_level == 0 && vd.var_name != Atom::CONST__ret_ {
                let mut cv = ClosureVar {
                    flags: 0,
                    var_idx: i as _,
                    var_name: vd.var_name,
                };
                cv.set_local(true);
                cv.set_arg(false);
                cv.set_const(vd.is_const());
                cv.set_lexical(vd.is_lexical());
                cv.set_var_kind(vd.var_kind());

                cvars.push(cv);
            }
        }
    } else {
        // only add pseudo variables
        for (i, vd) in bc.local_vars().iter().enumerate() {
            if vd.scope_level == 0 && is_var_in_arg_scope(vd) {
                let mut cv = ClosureVar {
                    flags: 0,
                    var_idx: i as _,
                    var_name: vd.var_name,
                };
                cv.set_local(true);
                cv.set_arg(false);
                cv.set_const(vd.is_const());
                cv.set_lexical(vd.is_lexical());
                cv.set_var_kind(vd.var_kind());

                cvars.push(cv);
            }
        }
    }

    for (i, cv0) in bc.closure_vars().iter().enumerate() {
        let mut cv = ClosureVar {
            flags: 0,
            var_idx: i as _,
            var_name: cv0.var_name,
        };
        cv.set_local(false);
        cv.set_arg(cv0.is_arg());
        cv.set_const(cv0.is_const());
        cv.set_lexical(cv0.is_lexical());
        cv.set_var_kind(cv0.var_kind());

        cvars.push(cv);
    }

    s.closure_vars.replace(cvars);
    Ok(())
}
