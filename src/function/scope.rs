use std::{marker::PhantomData, ptr::NonNull};

use crate::{
    atom::{JSAtom, consts::JS_ATOM__with_},
    bytecode::OpCodeTemp,
    context::JSContext,
    function::{JSFunctionDef, JSVarKindEnum},
};

use super::{ARG_SCOPE_END, JSVarDef};

#[derive(Debug, Clone, Copy)]
pub struct JSVarScope {
    /// id of parent scope. -1 for none.
    pub parent: i32,

    /// index of the last variable defined in `vars` list up to this scope.
    /// -1 for none.
    pub first: i32,
}

impl JSFunctionDef {
    /// Enters a new lexical scope for variable declarations.
    ///
    /// Create a new scope and push to scopes stack in the function def
    /// and emits the corresponding bytecode.
    ///
    /// Returns:
    ///
    ///  * The new scope id.
    /// * Err(-1) when alloc fail, in this case, current_exception has the exception.
    pub fn push_scope(&mut self, last_line_num: i32) -> Result<u16, i32> {
        let scope_id = self.scopes.len(); // new scope id

        if self.scopes.len() == self.scopes.capacity() {
            if self
                .scopes
                .try_reserve_exact(self.scopes.capacity() >> 2)
                .is_err()
            {
                self.ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        self.scopes.push(JSVarScope {
            parent: self.scope_level as i32,
            first: self.scope_first,
        });
        self.scope_level = scope_id as i32;

        self.emit_op_temp(OpCodeTemp::ENTER_SCOPE, last_line_num);
        self.emit_u16(scope_id as u16);

        Ok(scope_id as _)
    }

    /// Exits the current lexical scope
    ///
    /// Restores the parent scope and updates variable accessibility.
    /// Emits bytecode for scope cleanup.
    pub fn pop_scope(&mut self, last_line_num: i32) {
        // disable scoped variables
        let scope = self.scope_level;
        self.scope_level = self.scopes[scope as usize].parent;
        self.scope_first = self
            .get_first_lexical_var(self.scope_level)
            .map_or(-1, |x| x as i32);

        self.emit_op_temp(OpCodeTemp::LEAVE_SCOPE, last_line_num);
        self.emit_u16(scope as _);
    }

    /// Closes multiple scopes from `scope_from` up to `scope_stop` point.
    ///
    /// Emits leave scope operations for all scopes from current until scope_stop.
    /// Used during control flow changes that require multiple scope exits.
    ///
    /// # Arguments
    /// * `scope_from` - Starting scope ID to close from
    /// * `scope_stop` - Stop closing scopes at this ID
    pub fn close_scopes(&mut self, scope_from: u16, scope_stop: u16, last_line_num: i32) {
        let mut scope = scope_from as i32;

        while scope > scope_stop as i32 {
            self.emit_op_temp(OpCodeTemp::LEAVE_SCOPE, last_line_num);
            self.emit_u16(scope as _);

            scope = self.scopes[scope as usize].parent; // -1 means no parent
        }
    }

    /// define a variable in the current scope
    pub unsafe fn add_scope_var(
        &mut self,
        ctx: &JSContext,
        name: JSAtom,
        var_kind: JSVarKindEnum,
    ) -> Result<u16, i32> {
        let idx = self.add_var(ctx, name)?;

        let vd = self.vars.get_mut(idx as usize).unwrap();
        vd.set_var_kind(var_kind);
        vd.scope_level = self.scope_level;
        vd.scope_next = self.scope_first;

        self.scopes[self.scope_level as usize].first = idx as i32;
        self.scope_first = idx as i32;

        Ok(idx)
    }

    /// find a variable declaration in specified scope
    pub fn find_var_in_scope(&mut self, name: JSAtom, scope: u16) -> Option<u16> {
        let mut vidx = self.scopes[scope as usize].first;

        while vidx >= 0 {
            let vd = self.vars.get(vidx as usize).unwrap();
            if vd.scope_level != scope as i32 {
                break;
            } else if vd.var_name == name {
                return Some(vidx as u16);
            }

            vidx = vd.scope_next;
        }

        None
    }

    /// find a variable declaration in same or descendant scope of `scope`
    pub fn find_var_in_child_scope(&self, name: JSAtom, scope: i32) -> Option<u16> {
        self.vars
            .iter()
            .position(|vd| {
                vd.var_name == name
                    && vd.scope_level == 0
                    && self.is_child_scope(vd.scope_next, scope)
            })
            .map(|x| x as u16)
    }

    /// Test if `scope` is or is descendant of `parent_scope`
    pub fn is_child_scope(&self, scope: i32, parent_scope: i32) -> bool {
        let mut scope = scope;
        while scope >= 0 {
            if scope == parent_scope {
                return true;
            }
            scope = self.scopes[scope as usize].parent;
        }

        false
    }

    pub fn has_with_scope(&self, scope_level: i32) -> bool {
        let mut s = self as *const Self;
        let mut scope_level = scope_level;

        // check if scope chain contains a with statement
        while !s.is_null() {
            let mut scope_idx = unsafe { (&(*s).scopes)[scope_level as usize].first };
            while scope_idx >= 0 {
                let vd = unsafe { (&(*s).vars).get(scope_idx as usize).unwrap() };
                if vd.var_name == JS_ATOM__with_ {
                    return true;
                }
                scope_idx = vd.scope_next;
            }

            // check parent scopes
            scope_level = unsafe { (*s).parent_scope_level };
            s = unsafe { (*s).parent };
        }

        false
    }
}

pub struct ScopedVarsIter<'s> {
    fd: *const JSFunctionDef,
    cur_vardef: Option<i32>,
    _mark: PhantomData<&'s ()>,
}

impl<'s> Iterator for ScopedVarsIter<'s> {
    type Item = (i32, &'s super::JSVarDef);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(idx) = self.cur_vardef {
            let vd = unsafe { (*self.fd).vars.get(idx as usize).unwrap() };
            self.cur_vardef = if vd.scope_next >= 0 {
                Some(vd.scope_next)
            } else {
                None
            };
            Some((idx, vd))
        } else {
            None
        }
    }
}

pub struct ScopedVarsIterMut<'s> {
    fd: *mut JSFunctionDef,
    cur_idx: i32,
    _mark: PhantomData<&'s ()>,
}

impl ScopedVarsIterMut<'_> {
    pub fn is_arg_scope(&self) -> bool {
        self.cur_idx == ARG_SCOPE_END
    }
}

pub struct ScopedVarsIterItem<'a> {
    pub fd: &'a mut JSFunctionDef,
    pub idx: i32,
    pub vd: &'a mut JSVarDef,
    pub arg_scope_end: bool,
}

impl<'s> Iterator for ScopedVarsIterMut<'s> {
    type Item = ScopedVarsIterItem<'s>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cur_idx >= 0 {
            let idx = self.cur_idx;
            let vd = unsafe { (*self.fd).vars.get_mut(idx as usize).unwrap() };
            self.cur_idx = vd.scope_next;

            Some(ScopedVarsIterItem {
                idx,
                fd: unsafe { self.fd.as_mut().unwrap() },
                vd,
                arg_scope_end: self.cur_idx == ARG_SCOPE_END,
            })
        } else {
            None
        }
    }
}

impl JSFunctionDef {
    pub fn scoped_vars_iter<'a>(&self, scope: u16) -> ScopedVarsIter<'a> {
        let idx = self.scopes[scope as usize].first;
        ScopedVarsIter {
            fd: self,
            cur_vardef: if idx >= 0 { Some(idx) } else { None },
            _mark: PhantomData,
        }
    }

    pub fn scoped_vars_iter_mut<'a>(&mut self, scope: u16) -> ScopedVarsIterMut<'a> {
        let idx = self.scopes[scope as usize].first;
        ScopedVarsIterMut {
            fd: self,
            cur_idx: idx,
            _mark: PhantomData,
        }
    }
}
