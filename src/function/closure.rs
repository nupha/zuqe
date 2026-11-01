// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use gc_lite::{GcNode, GcTrace, GcTraceCtx};
use smallvec::SmallVec;

use crate::{
    JS_CLASS_ASYNC_GENERATOR, JS_CLASS_GENERATOR,
    atom::Atom,
    context::{ContextPtr, ScopedContext},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE,
        JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT, JS_PROP_THROW, JS_PROP_WRITABLE,
        JSProperty, LazyInit, Object, PayloadPtr, Shape,
    },
    value::JSValue,
    vm_stack::{FrameIndex, VmStack},
};

use super::{
    ARG_SCOPE_END, BCWrapper, BFuncPayload, Bytecode, DEFINE_GLOBAL_FUNC_VAR,
    DEFINE_GLOBAL_LEX_VAR, FunctionDef, FunctionKind, JS_MAX_LOCAL_VARS, JSVarKindEnum, VarRef,
    is_var_in_arg_scope, js_function_name_value, js_function_set_properties,
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
    pub(crate) var_idx: u16,

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

    /// For global_var_refs entries: marks this as a declaration (not just a reference).
    /// Uses bit 1 (same position as is_arg) — safe because global_var_refs never use is_arg.
    #[inline(always)]
    pub(crate) const fn is_global_decl(&self) -> bool {
        (self.flags >> 1) & 0x01 != 0
    }

    pub(crate) const fn set_global_decl(&mut self, value: bool) {
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

    #[inline(always)]
    pub const fn set_lexical(&mut self, b: bool) {
        if b {
            self.flags |= 0x08;
        } else {
            self.flags &= !0x08;
        }
    }

    #[inline(always)]
    pub const fn var_kind(&self) -> JSVarKindEnum {
        unsafe { std::mem::transmute::<u8, JSVarKindEnum>((self.flags >> 4) & 0x0F) }
    }

    #[inline(always)]
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
        ctx: &mut ScopedContext,
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
                ctx, is_local, is_arg, vidx, var_name, is_const, is_lexical, var_kind,
            )
        }
    }

    /// returns added closure var index, or Err for error.
    /// returned closure var index is 0-based.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_closure_var(
        &mut self,
        ctx: &mut ScopedContext,
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
                return Err(ctx.throw_internal_error("too many closure variables"));
            }
            if lst.len() == lst.capacity() {
                lst.try_reserve(1).map_err(|_| ctx.throw_out_of_memory())?;
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

    /// Register a global variable reference for var_ref encoding.
    /// Returns the index (0-based, maps to global_var_refs[idx] in BFuncPayload).
    pub fn get_or_add_global_var_ref(&mut self, var_name: Atom) -> u16 {
        // Look for existing entry
        if let Some(idx) = self
            .global_var_refs
            .iter()
            .position(|cv| cv.var_name == var_name)
        {
            return idx as u16;
        }
        // Add new entry
        let cv = ClosureVar {
            flags: 0, // global var ref: is_local=false, is_arg=false
            var_idx: 0,
            var_name,
        };
        self.global_var_refs.push(cv);
        (self.global_var_refs.len() - 1) as u16
    }

    /// Register a global variable declaration entry.
    /// Like `get_or_add_global_var_ref` but marks the entry with is_global_decl flag,
    /// so that closure creation will call define_global_var before creating the VarRef.
    /// `is_lexical`: true for let/const declarations
    /// `is_func_decl`: true for function declarations (sets var_kind to FUNCTION_DECL)
    pub fn get_or_add_global_var_ref_decl(
        &mut self,
        var_name: Atom,
        is_lexical: bool,
        is_const: bool,
        is_func_decl: bool,
    ) -> u16 {
        let idx = self.get_or_add_global_var_ref(var_name) as usize;
        self.global_var_refs[idx].set_global_decl(true);
        self.global_var_refs[idx].set_lexical(is_lexical);
        self.global_var_refs[idx].set_const(is_const);
        if is_func_decl {
            self.global_var_refs[idx].set_var_kind(JSVarKindEnum::JS_VAR_FUNCTION_DECL);
        }
        idx as u16
    }
}

impl Bytecode {
    /// Create closure for arena-based execution (stackless trampoline path).
    /// Used by direct eval when the caller's frame is in the arena instead of
    /// a legacy StackFrame. Creates VarRefs pointing into the arena's data buffer.
    pub(crate) fn make_closure_for_arena(
        &self,
        ctx: &mut ScopedContext,
        proto: Option<Object>,
        captured_vars: &[VarRef],
        arena: &mut VmStack,
        parent_frame_idx: FrameIndex,
    ) -> Result<BCWrapper, i32> {
        // === Global variable declarations ===
        // Declarations are materialized at closure-creation time (not via
        // inline DEFINE_VAR/DEFINE_FUNC opcodes). This is pure property
        // definition on global_obj/global_var_obj — no VarRefs are created
        // for globals; GET_VAR/PUT_VAR resolve by name at runtime, like
        // QuickJS OP_get_var/OP_put_var.
        if self.global_var_ref_count != 0 {
            // Phase 1: validate all global declarations BEFORE creating any bindings:
            // first check (JS_CheckDefineGlobalVar), then create
            // (js_closure_define_global_var). Without pre-validation, a
            // failure in a later declaration would leave bindings from
            // earlier declarations leaked on global_obj/global_var_obj.
            for cv in self.global_var_refs() {
                if cv.is_global_decl() {
                    let mut check_flags = 0u32;
                    if cv.is_lexical() {
                        check_flags |= DEFINE_GLOBAL_LEX_VAR as u32;
                    }
                    if cv.var_kind() == JSVarKindEnum::JS_VAR_FUNCTION_DECL {
                        check_flags |= DEFINE_GLOBAL_FUNC_VAR as u32;
                    }
                    ctx.define_global_var_checked(cv.var_name, check_flags)?;
                }
            }

            // Phase 2: create all bindings.
            for cv in self.global_var_refs() {
                if cv.is_global_decl() {
                    let def_flags = if cv.is_lexical() {
                        (DEFINE_GLOBAL_LEX_VAR as u32)
                            | if !cv.is_const() { JS_PROP_WRITABLE } else { 0 }
                    } else {
                        // var/func decl adds JS_PROP_CONFIGURABLE only for eval code.
                        if self.is_direct_or_indirect_eval {
                            JS_PROP_CONFIGURABLE
                        } else {
                            0
                        }
                    };
                    ctx.define_global_var(cv.var_name, def_flags)?;

                    // For function declarations on existing configurable properties,
                    // update the property flags (writable, enumerable, non-configurable).
                    // This mimics QuickJS's special handling in js_closure_define_global_var
                    // for JS_VAR_GLOBAL_FUNCTION_DECL.
                    if !cv.is_lexical() && cv.var_kind() == JSVarKindEnum::JS_VAR_FUNCTION_DECL {
                        let mut gobj = ctx.global_obj();
                        if let Some((_, existing_flags)) = gobj.find_shape_property(cv.var_name)
                            && existing_flags & JS_PROP_CONFIGURABLE != 0
                        {
                            gobj.define_property(
                                ctx,
                                cv.var_name,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                JS_PROP_HAS_VALUE
                                    | JS_PROP_THROW
                                    | JS_PROP_ENUMERABLE
                                    | JS_PROP_WRITABLE
                                    | def_flags
                                    | JS_PROP_HAS_CONFIGURABLE
                                    | JS_PROP_HAS_WRITABLE
                                    | JS_PROP_HAS_ENUMERABLE,
                            )?;
                        }
                    }
                }
            }
        }

        let closure_count = self.closure_var_count as usize;

        let captures: SmallVec<[VarRef; 2]> = if closure_count != 0 {
            debug_assert!(
                ctx.is_arena_mode(),
                "closure vars require active arena mode"
            );

            // Extract frame buffer range before any mutable borrow on arena.
            #[cfg(debug_assertions)]
            let buf_range = {
                let head = arena.frame(parent_frame_idx);
                (head.buf.data_ptr(), head.buf.capacity())
            };

            // Extract frame metadata for closure var resolution.
            let (arg_base, var_base, arg_count, var_count) = {
                let head = arena.frame(parent_frame_idx);
                let data_ptr = head.buf.data_ptr();
                (
                    data_ptr,
                    unsafe { data_ptr.add(head.var_base_offset as usize) },
                    head.arg_count,
                    head.var_count,
                )
            };
            let parent_var_refs = arena.var_refs_or_default(parent_frame_idx);

            let mut lst = SmallVec::<[VarRef; 2]>::new();
            // `try_reserve_exact` is relative to the current length (0), so pass
            // the whole capture count: ≤2 stays inline, more is one allocation
            // of exactly the needed size (passing `count - 2` would under-reserve
            // and still regrow for 3+ captures).
            lst.try_reserve_exact(closure_count)
                .map_err(|_| ctx.throw_out_of_memory())?;

            for cv in self.closure_vars() {
                lst.push(if cv.is_local() {
                    // Create VarRef pointing to the local variable in the arena frame
                    if let Some(vr) = parent_var_refs
                        .iter()
                        .find(|x| x.is_match(cv.var_idx, cv.is_arg()))
                        .copied()
                    {
                        vr
                    } else {
                        let pvalue = unsafe {
                            NonNull::new_unchecked(if cv.is_arg() {
                                debug_assert!(cv.var_idx < arg_count);
                                arg_base.add(cv.var_idx as usize)
                            } else {
                                debug_assert!(cv.var_idx < var_count);
                                var_base.add(cv.var_idx as usize)
                            })
                        };
                        let vref = VarRef::create_bind(ctx, cv.var_idx, cv.is_arg(), pvalue)?;

                        #[cfg(debug_assertions)]
                        {
                            let (buf_start, buf_cap) = buf_range;
                            let buf_start: *const JSValue = buf_start as *const JSValue;
                            let buf_end = unsafe { buf_start.add(buf_cap as usize) };
                            let ptr: *const JSValue = pvalue.as_ptr() as *const JSValue;
                            debug_assert!(
                                ptr >= buf_start && ptr < buf_end,
                                "make_closure_for_arena: VarRef::Bind pvalue {:p} out of frame buf [{:p}, {:p})",
                                ptr, buf_start, buf_end
                            );
                        }

                        parent_var_refs.push(vref);
                        vref
                    }
                } else {
                    debug_assert!((cv.var_idx as usize) < captured_vars.len());
                    captured_vars[cv.var_idx as usize]
                });
            }

            lst
        } else {
            SmallVec::new()
        };

        let class_id = self.func_kind().class_id();
        let has_proto_prop =
            self.has_prototype() || self.func_kind().generator_class_id().is_some();

        // Fast path: the function object's own properties are fixed (length,
        // name, and optionally prototype), so install a cached three-slot
        // shape and fill the slots directly instead of walking three separate
        // shape transitions. proto == None is every current caller; an explicit
        // prototype keeps the general path below (its shape is not cacheable
        // per class).
        let obj = if proto.is_none() {
            let shape = ctx.function_object_shape(self.func_kind(), has_proto_prop)?;
            let o = ctx.create_instance_with_shape(
                shape,
                PayloadPtr::new(class_id, BFuncPayload::new(*self, captures)),
            )?;
            fill_function_slots(ctx, o, self, has_proto_prop)?;
            o
        } else {
            let mut o = ctx.create_instance_with_proto(
                proto.or_else(|| ctx.class_proto(class_id)),
                PayloadPtr::new(class_id, BFuncPayload::new(*self, captures)),
            )?;
            js_function_set_properties(
                ctx,
                o,
                if self.func_name.is_null() {
                    Atom::CONST_empty_string
                } else {
                    self.func_name
                },
                self.attrs.defined_arg_count as _,
            )?;
            if let Some(gen_class_id) = self.func_kind().generator_class_id() {
                let proto = ctx.create_base_object_with_proto(ctx.class_proto(gen_class_id))?;
                o.define_property_value(
                    ctx,
                    Atom::CONST_prototype,
                    proto.into(),
                    JS_PROP_WRITABLE,
                )?;
            } else if self.has_prototype() {
                o.set_flag_constructor(true);
                o.define_lazy_property(
                    ctx,
                    Atom::CONST_prototype,
                    LazyInit::Prototype,
                    JS_PROP_WRITABLE,
                )?;
            }
            o
        };

        Ok(BCWrapper(obj.gc_ref()))
    }
}

/// Fill the `length`/`name`(/`prototype`) slots of a function object created
/// with [`ScopedContext::function_object_shape`].
fn fill_function_slots(
    ctx: &mut ScopedContext,
    mut obj: Object,
    bc: &Bytecode,
    has_proto_prop: bool,
) -> Result<(), i32> {
    let name = if bc.func_name.is_null() {
        Atom::CONST_empty_string
    } else {
        bc.func_name
    };
    let name_val = js_function_name_value(ctx, name)?;

    // Slot order follows the shape built by `function_object_shape`.
    obj.set_prop_slot_non_gc(
        0,
        JSProperty::Normal(JSValue::from(bc.attrs.defined_arg_count as i32)),
    );
    obj.set_prop_slot(1, JSProperty::Normal(name_val));

    if has_proto_prop {
        if let Some(gen_class_id) = bc.func_kind().generator_class_id() {
            debug_assert!(matches!(
                gen_class_id,
                JS_CLASS_GENERATOR | JS_CLASS_ASYNC_GENERATOR
            ));
            let proto = ctx.create_base_object_with_proto(ctx.class_proto(gen_class_id))?;
            obj.set_prop_slot(2, JSProperty::Normal(proto.into()));
        } else {
            debug_assert!(bc.has_prototype());
            obj.set_flag_constructor(true);
            obj.set_prop_slot(2, JSProperty::LazyPrototype(ContextPtr::from(ctx.gc_ref())));
        }
    }

    Ok(())
}

impl<'c> ScopedContext<'c> {
    /// Cached shape of a function object: `length` and `name` (both
    /// configurable, non-writable, non-enumerable) followed by `prototype`
    /// when the function owns one.
    ///
    /// The shape is per context because its `[[Prototype]]` is this realm's
    /// class prototype, and per function kind/`has_prototype` because the
    /// `prototype` property differs (lazy for constructors, a plain data
    /// property for generators) or is absent (arrow/async). The flags mirror
    /// what the incremental define path stores, so both converge on the same
    /// registry entry instead of splitting the transition graph.
    pub(crate) fn function_object_shape(
        &mut self,
        kind: FunctionKind,
        has_prototype: bool,
    ) -> Result<Shape, i32> {
        let slot = kind as usize * 2 + has_prototype as usize;
        debug_assert!(slot < crate::context::FUNC_SHAPE_COUNT);

        if let Some(sh) = self.func_shapes[slot].as_ref() {
            return Ok(sh.clone());
        }

        let class_id = kind.class_id();
        let proto = self.class_proto(class_id);
        let extras = [
            (Atom::CONST_length, JS_PROP_CONFIGURABLE),
            (Atom::CONST_name, JS_PROP_CONFIGURABLE),
            (
                Atom::CONST_prototype,
                if kind.generator_class_id().is_some() {
                    JS_PROP_WRITABLE
                } else {
                    JS_PROP_WRITABLE | JS_PROP_LAZYINIT
                },
            ),
        ];
        let extras = &extras[..if has_prototype { 3 } else { 2 }];

        let empty = match self.rt().find_shape_by_proto(proto) {
            Some(sh) => sh,
            None => Shape::create(self, proto, 0)?,
        };

        let shape = if let Some(found) = self.rt().shape_registry.find_extend(&empty, extras) {
            found
        } else {
            let mut sh = empty.dup(self)?;
            sh.add_shape_properties(self, extras);
            self.rt_mut().register_shape(&sh)?;
            sh
        };

        self.func_shapes[slot] = Some(shape.clone());
        Ok(shape)
    }
}

/// for direct eval compilation: add references to the variables of the calling function.
/// scope_idx=-1 means end of scope chain at top.
pub(crate) fn add_closure_variables(
    ctx: &mut ScopedContext,
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
    cvars
        .try_reserve_exact(count as usize)
        .map_err(|_| ctx.throw_out_of_memory())?;

    // Add lexical variables in scope at the point of evaluation
    let vardefs = bc.local_vars();
    debug_assert!(
        scope_idx < vardefs.len() as i32,
        "scope_idx={scope_idx}, vardefs.len={}",
        vardefs.len()
    );

    let mut i = scope_idx;
    while i >= 0 {
        let vd = &vardefs[i as usize];
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
            var_idx: i as u16,
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

#[cfg(test)]
mod tests {
    use crate::{
        Context, Runtime, ScopedContext,
        atom::Atom,
        eval::{EvalFlags, EvalName, EvalType},
        function::FunctionKind,
        object::{
            JS_PROP_CONFIGURABLE, JS_PROP_LAZYINIT, JS_PROP_NORMAL, JS_PROP_TMASK, JS_PROP_WRITABLE,
        },
    };

    fn eval(ctx: &mut Context, src: &str) -> crate::JSResult {
        ctx.eval(
            src.as_bytes(),
            &EvalName::Name("closure-shape.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
    }

    /// VM-P7a: the cached shape is built once per (kind, has_prototype) and
    /// reused; a repeat call must not allocate another ShapeImpl.
    #[test]
    fn function_object_shape_is_cached() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        ctx.with_root_scope(|mut cx| {
            let plain = cx.function_object_shape(FunctionKind::Normal, true)?;
            let before = cx.rt().shape_registry.count();

            let again = cx.function_object_shape(FunctionKind::Normal, true)?;
            assert_eq!(plain.gc_ref(), again.gc_ref(), "cached shape not reused");
            assert_eq!(
                cx.rt().shape_registry.count(),
                before,
                "a repeat call allocated a shape"
            );

            // length, name, prototype — in that order (observable own order).
            let atoms: Vec<Atom> = plain.prop_defs.iter().map(|(_, p)| p.atom).collect();
            assert_eq!(
                atoms,
                [Atom::CONST_length, Atom::CONST_name, Atom::CONST_prototype]
            );
            let flags: Vec<u32> = plain.prop_defs.iter().map(|(_, p)| p.flags()).collect();
            assert_eq!(flags[0], JS_PROP_CONFIGURABLE);
            assert_eq!(flags[1], JS_PROP_CONFIGURABLE);
            assert_eq!(
                flags[2] & JS_PROP_TMASK,
                JS_PROP_LAZYINIT,
                "the prototype slot must start lazy"
            );
            assert_eq!(flags[2] & JS_PROP_WRITABLE, JS_PROP_WRITABLE);

            // Arrow/async-like functions own no prototype slot.
            let arrow = cx.function_object_shape(FunctionKind::Normal, false)?;
            assert_eq!(arrow.len(), 2);
            assert_ne!(arrow.gc_ref(), plain.gc_ref());

            // Generator functions own a *data* prototype (not lazy).
            let generator = cx.function_object_shape(FunctionKind::Generator, true)?;
            let gen_flags = generator.prop_defs.get(2).unwrap().flags();
            assert_eq!(
                gen_flags & JS_PROP_TMASK,
                JS_PROP_NORMAL,
                "generator prototype must be a data property"
            );
            assert_eq!(gen_flags & JS_PROP_WRITABLE, JS_PROP_WRITABLE);
            Ok::<(), i32>(())
        })
        .unwrap();
    }

    /// The fast path must produce the same observable function object as the
    /// incremental define path: own order, flags, and a prototype that is
    /// materialized once with a `constructor` back-link.
    #[test]
    fn function_object_fast_path_preserves_property_contract() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        let res = eval(
            &mut ctx,
            "var f = function (a, b) {}; \
             var d = Object.getOwnPropertyDescriptor(f, 'prototype'); \
             [Object.getOwnPropertyNames(f).join(','), \
              f.length, f.name, \
              d.writable, d.enumerable, d.configurable, \
              f.prototype === f.prototype, f.prototype.constructor === f].join('|');",
        )
        .unwrap();

        let string = res.as_string(false).unwrap();
        let content = string.content().encode_utf8(false);
        assert_eq!(
            content.as_ref(),
            "length,name,prototype|2|f|true|false|false|true|true",
            "function object contract changed"
        );
    }
}
