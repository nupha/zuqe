// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    atom::Atom,
    context::{ContextImpl, ContextPtr},
    eval::EvalType,
    function::FunctionDef,
    stack_frame::ExecMode,
};

use super::ARG_SCOPE_INDEX;

pub(super) const JS_MAX_LOCAL_VARS: u16 = 65535;

pub(crate) const DEFINE_GLOBAL_LEX_VAR: u8 = 1 << 7;
pub(crate) const DEFINE_GLOBAL_FUNC_VAR: u8 = 1 << 6;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSVarDefEnum {
    JS_VAR_DEF_WITH,
    JS_VAR_DEF_LET,
    JS_VAR_DEF_CONST,
    JS_VAR_DEF_FUNCTION_DECL,     /* function declaration */
    JS_VAR_DEF_NEW_FUNCTION_DECL, /* async/generator function declaration */
    JS_VAR_DEF_CATCH,
    JS_VAR_DEF_VAR,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSVarKindEnum {
    /* XXX: add more variable kinds here instead of using bit fields */
    JS_VAR_NORMAL = 0,
    /// lexical var with function declaration
    JS_VAR_FUNCTION_DECL,
    /// lexical var with async/generator function declaration  
    JS_VAR_NEW_FUNCTION_DECL,
    JS_VAR_CATCH,
    /// function expression name
    JS_VAR_FUNCTION_NAME,
    JS_VAR_PRIVATE_FIELD,
    JS_VAR_PRIVATE_METHOD,
    JS_VAR_PRIVATE_GETTER,
    JS_VAR_PRIVATE_SETTER,        /* must come after JS_VAR_PRIVATE_GETTER */
    JS_VAR_PRIVATE_GETTER_SETTER, /* must come after JS_VAR_PRIVATE_SETTER */
}

pub const GLOBAL_VAR_OFFSET: i32 = 0x40000000;
pub const ARGUMENT_VAR_OFFSET: i32 = 0x20000000;

#[derive(Debug)]
pub struct JSVarDef {
    pub var_name: Atom,

    /// the scope in which this var exists.
    pub scope_level: i32,

    /// index of previous var in same scope.
    /// TODO: use Option<u16>
    pub scope_next: i32,

    // Combines is_const, is_lexical, is_captured, is_static_private, and var_kind
    //  is_const : 1;
    //  is_lexical : 1;
    //  is_captured : 1;
    //  is_static_private : 1; /* only used during private class field parsing */
    //  var_kind : 4; /* see JSVarKindEnum */
    bitfields: u8,

    // only used during compilation: function pool index for lexical
    // variables with var_kind = JS_VAR_FUNCTION_DECL/JS_VAR_NEW_FUNCTION_DECL or scope level of
    // the definition of the 'var' variables (they have scope_level = 0)
    pub func_pool_idx: i32,
}

impl GcTrace for JSVarDef {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.var_name.trace(tr);
    }
}

impl JSVarDef {
    #[inline(always)]
    pub const fn is_const(&self) -> bool {
        self.bitfields & 0x01 != 0
    }

    pub const fn set_is_const(&mut self, value: bool) {
        if value {
            self.bitfields |= 0x01;
        } else {
            self.bitfields &= !0x01;
        }
    }

    #[inline(always)]
    pub const fn is_lexical(&self) -> bool {
        (self.bitfields >> 1) & 0x01 != 0
    }

    pub const fn set_is_lexical(&mut self, value: bool) {
        if value {
            self.bitfields |= 0x02;
        } else {
            self.bitfields &= !0x02;
        }
    }

    #[inline(always)]
    pub const fn is_captured(&self) -> bool {
        (self.bitfields >> 2) & 0x01 != 0
    }

    pub const fn set_is_captured(&mut self, value: bool) {
        if value {
            self.bitfields |= 0x04;
        } else {
            self.bitfields &= !0x04;
        }
    }

    #[inline(always)]
    pub const fn is_static_private(&self) -> bool {
        (self.bitfields >> 3) & 0x01 != 0
    }

    pub const fn set_is_static_private(&mut self, value: bool) {
        if value {
            self.bitfields |= 0x08;
        } else {
            self.bitfields &= !0x08;
        }
    }

    #[inline(always)]
    pub fn var_kind(&self) -> JSVarKindEnum {
        unsafe { std::mem::transmute((self.bitfields >> 4) & 0x0F) }
    }

    pub const fn set_var_kind(&mut self, kind: JSVarKindEnum) {
        let value = kind as u8;
        self.bitfields = (self.bitfields & 0x0F) | (value << 4);
    }
}

/* XXX: should handle the argument scope generically */
#[inline]
pub(super) const fn is_var_in_arg_scope(vd: &JSVarDef) -> bool {
    matches!(
        vd.var_name,
        Atom::CONST__home_object_
            | Atom::CONST_this_active_func
            | Atom::CONST_new_target
            | Atom::CONST_this
            | Atom::CONST__arg_var_
    )
}

#[derive(Debug)]
pub struct JSGlobalVar {
    /// if >= 0, index in the constant pool for hoisted
    pub cpool_idx: i32,

    // function definition
    // force_init: 1
    // is_lexical: 1
    // is_const: 1
    bitfields: u8,

    pub scope_level: i32, // scope of definition
    pub var_name: Atom,   // variable name
}

impl JSGlobalVar {
    #[inline(always)]
    pub const fn force_init(&self) -> bool {
        self.bitfields & 1 != 0
    }

    #[inline(always)]
    pub const fn is_lexical(&self) -> bool {
        self.bitfields & 2 != 0
    }

    #[inline(always)]
    pub const fn is_const(&self) -> bool {
        self.bitfields & 4 != 0
    }

    #[inline(always)]
    pub const fn set_force_init(&mut self, v: bool) {
        self.bitfields = (self.bitfields & !1) | (v as u8)
    }

    #[inline(always)]
    pub const fn set_is_lexical(&mut self, v: bool) {
        self.bitfields = (self.bitfields & !2) | ((v as u8) << 1)
    }

    #[inline(always)]
    pub const fn set_is_const(&mut self, v: bool) {
        self.bitfields = (self.bitfields & !4) | ((v as u8) << 2)
    }
}

impl FunctionDef {
    /// add a global variable definition
    pub(crate) fn add_global_var(
        &mut self,
        mut ctx: ContextPtr,
        name: Atom,
    ) -> Result<&mut JSGlobalVar, i32> {
        if self.global_vars.len() == self.global_vars.capacity()
            && self.global_vars.try_reserve_exact(2).is_err()
        {
            ctx.throw_out_of_memory();
            return Err(-1);
        }

        let mut hf = JSGlobalVar {
            cpool_idx: -1,
            bitfields: 0,
            scope_level: self.scope_level,
            var_name: name,
        };
        hf.set_force_init(false);
        hf.set_is_lexical(false);
        hf.set_is_const(false);
        self.global_vars.push(hf);

        Ok(self.global_vars.last_mut().unwrap())
    }

    #[inline]
    pub fn find_global_var(&self, name: Atom) -> Option<&JSGlobalVar> {
        self.global_vars.iter().find(|x| x.var_name == name)
    }

    #[inline]
    pub(crate) fn find_global_var_mut(&mut self, name: Atom) -> Option<&mut JSGlobalVar> {
        self.global_vars.iter_mut().find(|x| x.var_name == name)
    }

    #[inline]
    pub fn find_lexical_global_var(&self, name: Atom) -> Option<&JSGlobalVar> {
        self.find_global_var(name)
            .and_then(|v| if v.is_lexical() { Some(v) } else { None })
    }

    #[inline]
    /// `name` is not consumed.
    pub(crate) fn find_lexical_global_var_mut(&mut self, name: Atom) -> Option<&mut JSGlobalVar> {
        self.find_global_var_mut(name)
            .and_then(|v| if v.is_lexical() { Some(v) } else { None })
    }

    /// add new argument to the function, returns arg index or error.
    pub fn add_arg(&mut self, ctx: &mut ContextImpl, name: Atom) -> Result<u16, i32> {
        // the local variable indexes are currently stored on 16 bits
        if self.args.len() >= JS_MAX_LOCAL_VARS as usize {
            ctx.throw_internal_error("too many arguments");
            return Err(-1);
        }

        if self.args.len() == self.args.capacity() {
            self.args.try_reserve_exact(2).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }

        self.args.push(JSVarDef {
            var_name: name,
            func_pool_idx: -1,
            scope_level: 0,
            scope_next: 0,
            bitfields: 0,
        });

        Ok(self.args.len() as u16 - 1)
    }

    /// add variable defination. return the new variable index or error.
    /// `name` is not consumed
    pub fn add_var(&mut self, mut ctx: ContextPtr, name: Atom) -> Result<u16, i32> {
        if self.vars.len() >= JS_MAX_LOCAL_VARS as usize {
            ctx.throw_internal_error("too many local variables");
            return Err(-1);
        }

        if self.vars.len() == self.vars.capacity() && self.vars.try_reserve_exact(4).is_err() {
            ctx.throw_out_of_memory();
            return Err(-1);
        }

        self.vars.push(JSVarDef {
            var_name: name,
            func_pool_idx: -1,
            scope_level: 0,
            scope_next: 0,
            bitfields: 0,
        });

        Ok(self.vars.len() as u16 - 1)
    }

    /// define pseudo variable named `this`
    pub fn add_var_this(&mut self, ctx: ContextPtr) -> Result<u16, i32> {
        let idx = self.add_var(ctx, Atom::CONST_this)?;
        if self.attrs.is_derived_class_constructor {
            // used to trigger 'uninitialized' checks
            // in a derived class constructor
            // XXX: should have is_this flag or var type
            self.vars[idx as usize].set_is_lexical(true);
        }
        Ok(idx)
    }

    /// Add defination named `arguments` for special object.
    ///
    /// `in_arg_scope`:
    /// - false: will define in the variable scope, returns var idx for `arguments`
    /// - true: will define in the argument scope. this is only needed when "eval()" may be called in the argument scope. Return 0 if OK.
    pub fn add_arguments(&mut self, ctx: ContextPtr, in_arg_scope: bool) -> Result<u16, i32> {
        if in_arg_scope {
            if self.arguments_arg_idx.is_none()
                && self
                    .find_var_in_scope(Atom::CONST_arguments, ARG_SCOPE_INDEX as _)
                    .is_none()
            {
                // XXX: the scope links are not fully updated. May be an
                // issue if there are child scopes of the argument scope
                let idx = self.add_var(ctx, Atom::CONST_arguments)?;

                let vd = self.vars.get_mut(idx as usize).unwrap();
                vd.scope_next = self.scopes[ARG_SCOPE_INDEX as usize].first;
                vd.scope_level = ARG_SCOPE_INDEX;
                vd.set_is_lexical(true);

                self.scopes.get_mut(ARG_SCOPE_INDEX as usize).unwrap().first = idx as i32;
                self.arguments_arg_idx.replace(idx);
            }

            Ok(0)
        } else if let Some(idx) = self.arguments_var_idx {
            Ok(idx)
        } else {
            let idx = self.add_var(ctx, Atom::CONST_arguments)?;
            self.arguments_var_idx.replace(idx);
            Ok(idx)
        }
    }

    /// get or create const variable used as function/expr name.
    /// returns the variable id.
    pub fn add_func_var(&mut self, ctx: ContextPtr, name: Atom) -> Result<u16, i32> {
        if let Some(i) = self.func_var_idx {
            Ok(i)
        } else {
            let vi = self.add_var(ctx, name)?;
            self.func_var_idx.replace(vi);

            let vd = self.vars.get_mut(vi as usize).unwrap();
            vd.set_var_kind(JSVarKindEnum::JS_VAR_FUNCTION_NAME as _);
            if self.attrs.exec_mode.contains(ExecMode::STRICT) {
                vd.set_is_const(true);
            }

            Ok(vi)
        }
    }

    /// find argument var by `name`.
    ///
    /// returns
    /// * the variable index or'ed with ARGUMENT_VAR_OFFSET as var id
    /// * None if not found.
    pub fn find_arg(&self, name: Atom) -> Option<u32> {
        self.args
            .iter()
            .enumerate()
            .rev() // search in reveresed order
            .find_map(|(i, vd)| {
                if vd.var_name == name {
                    Some(i as u32 | (ARGUMENT_VAR_OFFSET as u32))
                } else {
                    None
                }
            })
    }

    /// find variable by `name`. if `name` is not found in vars, find it in arguments.
    /// for argument var, returned id is or'ed with ARGUMENT_VAR_OFFSET.
    pub fn find_var(&self, name: Atom) -> Option<u32> {
        self.vars
            .iter()
            .enumerate()
            .rev() // search in reveresed order
            .find_map(|(i, vd)| {
                if vd.var_name == name && vd.scope_level == 0 {
                    Some(i as u32)
                } else {
                    None
                }
            })
            .or(self.find_arg(name)) // not found is vars, search in args
    }

    /// will check global lexcical only when `scope` is minus.
    /// `name` is not consumed.
    pub fn find_lexical_decl(
        &mut self,
        name: Atom,
        scope: i32,
        check_catch_var: bool,
    ) -> Option<u32> {
        let mut scope_idx = scope;
        while scope_idx >= 0 {
            let vd = self.vars.get(scope_idx as usize).unwrap();
            if vd.var_name == name
                && (vd.is_lexical()
                    || (vd.var_kind() == JSVarKindEnum::JS_VAR_CATCH && check_catch_var))
            {
                return Some(scope_idx as u32);
            }

            scope_idx = vd.scope_next;
        }

        if self.is_eval
            && self.eval_type == EvalType::Global
            && self.find_lexical_global_var(name).is_some()
        {
            return Some(GLOBAL_VAR_OFFSET as u32);
        }

        None
    }

    /// Get head of variables-chain from `scope`.
    ///
    /// head of variables-chain is actually the index of last JSVarDef in `scope`.
    pub fn get_first_lexical_var(&mut self, scope: i32) -> Option<u16> {
        let mut scope = scope;
        while scope >= 0 {
            let head = self.scopes[scope as usize].first;
            if head >= 0 {
                return Some(head as u16);
            }
            scope = self.scopes[scope as usize].parent;
        }

        None
    }
}
