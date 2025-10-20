use std::ops::Deref;

use crate::{
    JS_CLASS_ASYNC_GENERATOR, JS_CLASS_GENERATOR,
    atom::{Atom, consts::*},
    context::JSContext,
    function::{
        BCFuncPayload, BCFunction, Bytecode, JSFunctionKindEnum, JSVarRef,
        js_function_set_properties,
    },
    object::{JS_PROP_WRITABLE, JS_SetConstructorBit, LazyInit, ObjectPayload},
    runtime::JS_MAX_LOCAL_VARS,
    stack_frame::StackFrame,
    value::JSValue,
};

use super::{ARG_SCOPE_END, JSFunctionDef, JSVarDef, JSVarKindEnum, is_var_in_arg_scope};

#[derive(Debug)]
pub struct JSClosureVar {
    // Combines is_local, is_arg, is_const, is_lexical, and var_kind
    //  is_local : 1;
    //  is_arg : 1;
    //  is_const : 1;
    //  is_lexical : 1;
    //  var_kind : 4; /* see JSVarKindEnum */
    bitfield: u8,

    // 8 bits available
    // is_local = TRUE: index to a normal variable of the parent function.
    // otherwise: index to a closure variable of the parent function
    // 0-based index
    pub var_idx: u16,

    pub var_name: Atom,
}

impl JSClosureVar {
    pub fn is_local(&self) -> bool {
        self.bitfield & 0x01 != 0
    }

    pub fn set_is_local(&mut self, value: bool) {
        if value {
            self.bitfield |= 0x01;
        } else {
            self.bitfield &= !0x01;
        }
    }

    pub fn is_arg(&self) -> bool {
        (self.bitfield >> 1) & 0x01 != 0
    }

    pub fn set_is_arg(&mut self, value: bool) {
        if value {
            self.bitfield |= 0x02;
        } else {
            self.bitfield &= !0x02;
        }
    }

    pub fn is_const(&self) -> bool {
        (self.bitfield >> 2) & 0x01 != 0
    }

    pub fn set_is_const(&mut self, value: bool) {
        if value {
            self.bitfield |= 0x04;
        } else {
            self.bitfield &= !0x04;
        }
    }

    pub fn is_lexical(&self) -> bool {
        (self.bitfield >> 3) & 0x01 != 0
    }

    pub fn set_is_lexical(&mut self, value: bool) {
        if value {
            self.bitfield |= 0x08;
        } else {
            self.bitfield &= !0x08;
        }
    }

    pub fn var_kind(&self) -> JSVarKindEnum {
        unsafe { std::mem::transmute((self.bitfield >> 4) & 0x0F) }
    }

    pub fn set_var_kind(&mut self, value: JSVarKindEnum) {
        self.bitfield = (self.bitfield & 0x0F) | ((value as u8) << 4);
    }
}

impl JSFunctionDef {
    /// `fd` must be an ancestor of `self`.
    /// Create in 'self' a closure referencing a local variable (is_local = TRUE)
    /// or a closure (is_local = FALSE) in `fd`
    pub(super) fn get_closure_var2(
        &mut self,
        ctx: &JSContext,
        fd: *const JSFunctionDef,
        mut is_local: bool,
        is_arg: bool,
        var_idx: u16,
        var_name: &Atom,
        is_const: bool,
        is_lexical: bool,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        debug_assert!(!self.parent.is_null());

        let vidx = if fd != self.parent {
            // search var_idx along ancestor path
            let i = unsafe {
                (&mut *self.parent).get_closure_var2(
                    ctx, fd, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
                )?
            };
            is_local = false;
            i
        } else {
            var_idx
        };

        // find idx in self
        if let Some(cvars) = self.closure_vars.as_ref() {
            if let Some(nth) = cvars.iter().position(|cv| {
                cv.var_idx == vidx && cv.is_arg() == is_arg && cv.is_local() == is_local
            }) {
                return Ok(nth as u16);
            }
        }

        // not found, create it.
        self.add_closure_var(
            ctx, is_local, is_arg, vidx, var_name, is_const, is_lexical, var_kind,
        )
    }

    /// `fd` must be an ancestor of `self`.
    #[inline(always)]
    pub(super) fn get_closure_var(
        &mut self,
        ctx: &JSContext,
        fd: *const JSFunctionDef,
        is_arg: bool,
        var_idx: u16,
        var_name: &Atom,
        is_const: bool,
        is_lexical: bool,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        self.get_closure_var2(
            ctx, fd, true, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
        )
    }

    /// returns added closure var index, or Err for error.
    pub fn add_closure_var(
        &mut self,
        ctx: &JSContext,
        is_local: bool,
        is_arg: bool,
        var_idx: u16,
        var_name: &Atom,
        is_const: bool,
        is_lexical: bool,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        let mut cv = JSClosureVar {
            bitfield: 0,
            var_idx,
            var_name: var_name.clone(),
        };
        cv.set_is_local(is_local);
        cv.set_is_arg(is_arg);
        cv.set_is_const(is_const);
        cv.set_is_lexical(is_lexical);
        cv.set_var_kind(var_kind);

        if let Some(cvars) = self.closure_vars.as_mut() {
            if cvars.len() == JS_MAX_LOCAL_VARS as usize {
                ctx.throw_internal_error("too many closure variables");
                return Err(-1);
            }
            if cvars.len() == cvars.capacity() {
                cvars.try_reserve_exact(8).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            cvars.push(cv);
            Ok(cvars.len() as u16 - 1) // var index is 0-based
        } else {
            let mut lst = Vec::<JSClosureVar>::new();
            lst.try_reserve_exact(16).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            lst.push(cv);
            self.closure_vars.replace(lst);
            Ok(0) // first index is 0
        }
    }

    /// find closure var by name, returning index of the closure var
    pub fn find_closure_var(&self, var_name: &Atom) -> Option<u16> {
        self.closure_vars
            .as_ref()
            .and_then(|lst| lst.iter().position(|cv| cv.var_name.eq(var_name)))
            .map(|idx| idx as u16)
    }
}

impl Bytecode {
    /// Create function (a.k.a. closure) with `self` Bytecode and captured variables.
    pub fn make_closure(
        &self,
        ctx: &JSContext,
        cur_var_refs: &[JSVarRef],
        parent_frame: Option<&mut StackFrame>,
        proto: Option<JSValue>,
    ) -> Result<BCFunction, i32> {
        let var_refs: Vec<JSVarRef> = if self.closure_var_count != 0 {
            debug_assert!(
                parent_frame.is_some(),
                "parent stack frame must be present if there is any closure variable"
            );
            let sf = parent_frame.unwrap();

            // initialize var_refs with closure variables
            let mut lst = Vec::new();
            lst.try_reserve_exact(self.closure_var_count as usize)
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            for cv in self.closure_vars().iter() {
                let var_ref = if cv.is_local() {
                    // reuse the existing variable reference if it already exists
                    sf.get_var_ref(ctx, cv.var_idx, cv.is_arg())?
                } else {
                    cur_var_refs[cv.var_idx as usize].clone()
                };
                lst.push(var_ref);
            }

            lst
        } else {
            Vec::new()
        };

        let payload = {
            let isa = BCFuncPayload::new(self.clone(), var_refs);
            match self.func_kind() {
                JSFunctionKindEnum::JS_FUNC_NORMAL => ObjectPayload::BYTECODE_FUNCTION(isa),
                JSFunctionKindEnum::JS_FUNC_GENERATOR => ObjectPayload::GENERATOR_FUNCTION(isa),
                JSFunctionKindEnum::JS_FUNC_ASYNC => ObjectPayload::ASYNC_FUNCTION(isa),
                JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR => {
                    ObjectPayload::ASYNC_GENERATOR_FUNCTION(isa)
                }
            }
        };
        let func = BCFunction(
            ctx.create_object_from_proto(
                proto.unwrap_or_else(|| {
                    ctx.0.borrow().class_proto[payload.class_id() as usize].clone()
                }),
                payload,
            )
            .map_err(|_| -1)?,
        );

        js_function_set_properties(
            ctx,
            &func,
            if self.func_name.is_null() {
                Atom::new_const(JS_ATOM_empty_string)
            } else {
                self.func_name.clone()
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

            let proto = ctx.create_base_object_from_proto(
                ctx.0.borrow().class_proto[gen_class_id as usize].clone(),
            )?;
            func.define_property_value(
                ctx,
                JS_ATOM_prototype.deref(),
                proto.into(),
                JS_PROP_WRITABLE,
            )
            .unwrap();
        } else if self.has_prototype() {
            // add the 'prototype' property: delay instantiation to avoid
            // creating cycles for every javascript function. The prototype
            // object is created on the fly when first accessed
            JS_SetConstructorBit(ctx, &func.clone().into(), true);

            func.define_lazy_property(
                ctx,
                JS_ATOM_prototype.deref(),
                LazyInit::Prototype,
                JS_PROP_WRITABLE,
            )?;
        }

        Ok(func)
    }
}

/// for direct eval compilation: add references to the variables of the calling function.
/// scope_idx=-1 means end of scope chain at top.
pub unsafe fn add_closure_variables(
    ctx: &JSContext,
    s: &mut JSFunctionDef,
    bc: &Bytecode,
    scope_idx: i32,
) -> Result<(), i32> {
    debug_assert!(s.closure_vars.is_none());

    let count = bc.arg_count + bc.var_count + bc.closure_var_count;
    if count == 0 {
        return Ok(());
    }

    let mut cvars = Vec::<JSClosureVar>::new();
    cvars.try_reserve_exact(count as usize).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    // Add lexical variables in scope at the point of evaluation
    let vardefs = bc.vars_slice();
    let mut i = scope_idx;
    while i >= 0 {
        let vd = vardefs.get(i as usize).unwrap();
        if vd.scope_level > 0 {
            let mut cv = JSClosureVar {
                bitfield: 0,
                var_idx: i as _,
                var_name: vd.var_name.clone(),
            };
            cv.set_is_local(true);
            cv.set_is_arg(false);
            cv.set_is_const(vd.is_const());
            cv.set_is_lexical(vd.is_lexical());
            cv.set_var_kind(vd.var_kind());

            cvars.push(cv);
        }
        i = vd.scope_next;
    }

    let is_arg_scope = i == ARG_SCOPE_END;
    if !is_arg_scope {
        // Add argument variables
        for (i, vd) in bc.args_slice().iter().enumerate() {
            let mut cv = JSClosureVar {
                bitfield: 0,
                var_idx: i as _,
                var_name: vd.var_name.clone(),
            };
            cv.set_is_local(true);
            cv.set_is_arg(true);
            cv.set_is_const(false);
            cv.set_is_lexical(false);
            cv.set_var_kind(JSVarKindEnum::JS_VAR_NORMAL);

            cvars.push(cv);
        }

        // Add local non lexical variables
        for (i, vd) in bc.vars_slice().iter().enumerate() {
            if vd.scope_level == 0 && vd.var_name.to_ref() != JS_ATOM__ret_ {
                let mut cv = JSClosureVar {
                    bitfield: 0,
                    var_idx: i as _,
                    var_name: vd.var_name.clone(),
                };
                cv.set_is_local(true);
                cv.set_is_arg(false);
                cv.set_is_const(vd.is_const());
                cv.set_is_lexical(vd.is_lexical());
                cv.set_var_kind(vd.var_kind());

                cvars.push(cv);
            }
        }
    } else {
        // only add pseudo variables
        for (i, vd) in bc.vars_slice().iter().enumerate() {
            if vd.scope_level == 0 && is_var_in_arg_scope(vd) {
                let mut cv = JSClosureVar {
                    bitfield: 0,
                    var_idx: i as _,
                    var_name: vd.var_name.clone(),
                };
                cv.set_is_local(true);
                cv.set_is_arg(false);
                cv.set_is_const(vd.is_const());
                cv.set_is_lexical(vd.is_lexical());
                cv.set_var_kind(vd.var_kind());

                cvars.push(cv);
            }
        }
    }

    for (i, cv0) in bc.closure_vars().iter().enumerate() {
        let mut cv = JSClosureVar {
            bitfield: 0,
            var_idx: i as _,
            var_name: cv0.var_name.clone(),
        };
        cv.set_is_local(false);
        cv.set_is_arg(cv0.is_arg());
        cv.set_is_const(cv0.is_const());
        cv.set_is_lexical(cv0.is_lexical());
        cv.set_var_kind(cv0.var_kind());

        cvars.push(cv);
    }

    (*s).closure_vars.replace(cvars);
    Ok(())
}

pub unsafe fn set_closure_from_var(
    ctx: &JSContext,
    cv: *mut JSClosureVar,
    vd: *mut JSVarDef,
    var_idx: u16,
) {
    (*cv).set_is_local(true);
    (*cv).set_is_arg(false);
    (*cv).set_is_const((*vd).is_const());
    (*cv).set_is_lexical((*vd).is_lexical());
    (*cv).set_var_kind((*vd).var_kind());
    (*cv).var_idx = var_idx;
    (*cv).var_name = (*vd).var_name.clone();
}
