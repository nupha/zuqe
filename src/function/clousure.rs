use fallible_collections::FallibleVec;

use crate::{
    JS_CLASS_ASYNC_GENERATOR, JS_CLASS_GENERATOR,
    atom::{JS_ATOM_NULL, JSAtom, consts::*},
    context::JSContext,
    function::{
        JSFunctionBytecode, JSFunctionKindEnum, JSObjectFunc, JSVarRef, func_kind_to_class_id,
        js_function_set_properties,
    },
    runtime::JS_MAX_LOCAL_VARS,
    stack_frame::JSStackFrame,
    value::{
        JSValue,
        object::{AutoInitType, JS_PROP_WRITABLE, JS_SetConstructorBit, JSObjectIsa},
    },
};

use super::{ARG_SCOPE_END, JSFunctionDef, JSVarDef, JSVarKindEnum, is_var_in_arg_scope};

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JSClosureVar {
    // Combines is_local, is_arg, is_const, is_lexical, and var_kind
    //  is_local : 1;
    //  is_arg : 1;
    //  is_const : 1;
    //  is_lexical : 1;
    //  var_kind : 4; /* see JSVarKindEnum */
    bitfield: u8,

    // 8 bits available
    // is_local = TRUE: index to a normal variable of the parent function. otherwise: index to a closure
    // variable of the parent function
    pub var_idx: u16,

    pub var_name: JSAtom,
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
    /// 'up_fd' must be an ancestor of 'self'. Create in 'self' a closure referencing a
    /// local variable (is_local = TRUE) or a closure (is_local = FALSE) in 'up_fd'
    pub fn get_closure_var2(
        &mut self,
        ctx: &JSContext,
        up_fd: *const JSFunctionDef,
        mut is_local: bool,
        is_arg: bool,
        var_idx: u16,
        var_name: JSAtom,
        is_const: bool,
        is_lexical: bool,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        let vi = if up_fd != self.parent {
            // search var_idx along ancestor path
            let i = unsafe {
                (*self.parent).get_closure_var2(
                    ctx, up_fd, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
                )?
            };
            is_local = false;
            i
        } else {
            var_idx
        };

        // find variable idx in JSFunctionDef `self`
        if let Some(cvars) = self.closure_vars.as_ref() {
            if let Some(nth) = cvars.iter().position(|cv| {
                cv.var_idx == vi && cv.is_arg() == is_arg && cv.is_local() == is_local
            }) {
                return Ok(nth as u16);
            }
        }

        // not found, create it.
        self.add_closure_var(
            ctx, is_local, is_arg, vi, var_name, is_const, is_lexical, var_kind,
        )
    }

    #[inline(always)]
    pub fn get_closure_var(
        &mut self,
        ctx: &JSContext,
        up_fd: *const JSFunctionDef,
        is_arg: bool,
        var_idx: u16,
        var_name: JSAtom,
        is_const: bool,
        is_lexical: bool,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        self.get_closure_var2(
            ctx, up_fd, true, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
        )
    }

    /// returns added closure var index, or Err for error.
    pub fn add_closure_var(
        &mut self,
        ctx: &JSContext,
        is_local: bool,
        is_arg: bool,
        var_idx: u16,
        var_name: JSAtom,
        is_const: bool,
        is_lexical: bool,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        let mut cv = JSClosureVar {
            bitfield: 0,
            var_idx,
            var_name: var_name.dup(ctx.rt()),
        };
        cv.set_is_local(is_local);
        cv.set_is_arg(is_arg);
        cv.set_is_const(is_const);
        cv.set_is_lexical(is_lexical);
        cv.set_var_kind(var_kind);

        if let Some(cvars) = self.closure_vars.as_mut() {
            if cvars.len() == JS_MAX_LOCAL_VARS as usize {
                ctx.JS_ThrowInternalError("too many closure variables");
                return Err(-1);
            }

            if cvars.try_reserve(8).is_err() {
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }

            cvars.push(cv);
            Ok(cvars.len() as u16 - 1)
        } else {
            if let Ok(mut cvars) = Vec::<JSClosureVar>::try_with_capacity(8) {
                cvars.push(cv);
                self.closure_vars.replace(cvars);
                Ok(0) // first index is 0
            } else {
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }
    }

    /// find closure var by name, returning index of the closure var
    pub unsafe fn find_closure_var(&self, var_name: JSAtom) -> Option<u16> {
        self.closure_vars
            .as_ref()
            .and_then(|lst| lst.iter().position(|cv| cv.var_name == var_name))
            .map(|idx| idx as u16)
    }
}

/// Build func object closure with captured variables, returns function object.
/// `bfunc` is consumed.
pub unsafe fn js_closure(
    ctx: &JSContext,
    bc: &JSFunctionBytecode,
    cur_var_refs: &[JSVarRef],
    sf: *mut JSStackFrame,
) -> JSValue {
    let func_kind = bc.0.borrow().func_kind;

    let func_obj = match ctx.create_instance(match func_kind {
        JSFunctionKindEnum::JS_FUNC_NORMAL => {
            JSObjectIsa::BYTECODE_FUNCTION(JSObjectFunc::default())
        }
        JSFunctionKindEnum::JS_FUNC_GENERATOR => {
            todo!("[O.o] TODO create generator closure");
            //JSObjectIsa::GENERATOR(JSGeneratorData::default())
        }
        JSFunctionKindEnum::JS_FUNC_ASYNC => JSObjectIsa::ASYNC_FUNCTION(JSObjectFunc::default()),
        JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR => JSObjectIsa::ASYNC_GENERATOR(None),
    }) {
        Ok(o) => o,
        Err(_) => {
            return JSValue::EXCEPTION;
        }
    };

    let closure = js_closure2(ctx, func_obj, bc.clone(), cur_var_refs, sf);
    if closure.is_exception() {
        return JSValue::EXCEPTION;
    }

    let mut name_atom = bc.0.borrow().func_name;
    if name_atom == JS_ATOM_NULL {
        name_atom = JS_ATOM_empty_string;
    }
    js_function_set_properties(
        ctx,
        &closure,
        name_atom,
        bc.0.borrow().defined_arg_count as _,
    );

    if matches!(
        func_kind,
        JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
    ) {
        // generators have a prototype field which is used as
        // prototype for the generator object
        let proto_class_id = func_kind_to_class_id[func_kind as usize];
        debug_assert!(matches!(
            proto_class_id,
            JS_CLASS_GENERATOR | JS_CLASS_ASYNC_GENERATOR
        ));

        let proto = if let Ok(o) =
            ctx.create_base_object_with_proto(&ctx.0.borrow().class_proto[proto_class_id as usize])
        {
            o
        } else {
            return JSValue::EXCEPTION;
        };

        closure
            .define_property_value(ctx, JS_ATOM_prototype, proto, JS_PROP_WRITABLE)
            .unwrap();
    } else if bc.0.borrow().has_prototype {
        // add the 'prototype' property: delay instantiation to avoid
        // creating cycles for every javascript function. The prototype
        // object is created on the fly when first accessed
        JS_SetConstructorBit(ctx, &closure, true);

        unsafe {
            closure
                .object_unchecked()
                .define_auto_init_property(
                    ctx,
                    JS_ATOM_prototype,
                    AutoInitType::Prototype,
                    JS_PROP_WRITABLE,
                )
                .unwrap();
        }
    }

    debug_assert!(unsafe { closure.object_unchecked().0.isa.borrow().func().is_some() });

    closure
}

/// `func_obj` is consumed
pub fn js_closure2(
    ctx: &JSContext,
    func_obj: JSValue,
    bc: JSFunctionBytecode,
    cur_var_refs: &[JSVarRef],
    sf: *mut JSStackFrame,
) -> JSValue {
    {
        // isa lives in this block only.
        let mut isa = unsafe { func_obj.object_unchecked().0.isa.borrow_mut() };

        let func = isa.func_mut().unwrap();
        debug_assert!(func.var_refs.is_empty());

        func.function_bytecode.replace(bc.clone());
        func.home_object.take();

        let closure_var_count = bc.0.borrow().closure_var_count;
        if closure_var_count != 0 {
            if func
                .var_refs
                .try_reserve_exact(closure_var_count as usize)
                .is_err()
            {
                ctx.JS_ThrowOutOfMemory();
                return JSValue::EXCEPTION;
            };

            // initialize function object var_refs with closure variables
            for cv in bc.closure_vars_slice().iter() {
                let var_ref = if cv.is_local() {
                    // reuse the existing variable reference if it already exists
                    if let Ok(r) = unsafe { (*sf).get_var_ref(ctx, cv.var_idx, cv.is_arg()) } {
                        r
                    } else {
                        return JSValue::EXCEPTION;
                    }
                } else {
                    cur_var_refs[cv.var_idx as usize].clone()
                };

                func.var_refs.push(var_ref);
            }
        }
    }

    func_obj
}

#[deprecated(note = "use JSFunctionDef::add_closure_var() instead")]
/// returns added closure var index, or Err for error.
pub unsafe fn add_closure_var(
    ctx: &JSContext,
    s: &mut JSFunctionDef,
    is_local: bool,
    is_arg: bool,
    var_idx: u16,
    var_name: JSAtom,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    s.add_closure_var(
        ctx, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    )
}

/// for direct eval compilation: add references to the variables of the calling function.
/// scope_idx=-1 means end of scope chain at top.
pub unsafe fn add_closure_variables(
    ctx: &JSContext,
    s: &mut JSFunctionDef,
    bc: &JSFunctionBytecode,
    scope_idx: i32,
) -> Result<(), i32> {
    debug_assert!(s.closure_vars.is_none());

    let b = bc.0.borrow();
    let count = b.arg_count + b.var_count + b.closure_var_count;
    if count == 0 {
        return Ok(());
    }

    let mut cvars = Vec::<JSClosureVar>::try_with_capacity(count as usize).map_err(|_| {
        ctx.JS_ThrowOutOfMemory();
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
                var_name: vd.var_name.dup(ctx.rt()),
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
        for (i, vd) in b.args_slice().iter().enumerate() {
            let mut cv = JSClosureVar {
                bitfield: 0,
                var_idx: i as _,
                var_name: vd.var_name.dup(ctx.rt()),
            };
            cv.set_is_local(true);
            cv.set_is_arg(true);
            cv.set_is_const(false);
            cv.set_is_lexical(false);
            cv.set_var_kind(JSVarKindEnum::JS_VAR_NORMAL);

            cvars.push(cv);
        }

        // Add local non lexical variables
        for (i, vd) in b.vars_slice().iter().enumerate() {
            if vd.scope_level == 0 && vd.var_name != JS_ATOM__ret_ {
                let mut cv = JSClosureVar {
                    bitfield: 0,
                    var_idx: i as _,
                    var_name: vd.var_name.dup(ctx.rt()),
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
        for (i, vd) in b.vars_slice().iter().enumerate() {
            if vd.scope_level == 0 && is_var_in_arg_scope(vd) {
                let mut cv = JSClosureVar {
                    bitfield: 0,
                    var_idx: i as _,
                    var_name: vd.var_name.dup(ctx.rt()),
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

    for (i, cv0) in b.closure_vars_slice().iter().enumerate() {
        let mut cv = JSClosureVar {
            bitfield: 0,
            var_idx: i as _,
            var_name: cv0.var_name.dup(ctx.rt()),
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
    (*cv).var_name = (*vd).var_name.dup(ctx.rt());
}
