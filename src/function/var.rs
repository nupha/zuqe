use fallible_collections::FallibleVec;

use crate::{
    atom::{consts::*, JSAtom, JS_DupAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{opcode_info_intermediate, opcodes, OpCode, OpCodeTemp},
    context::{JSContext, JS_MODE_STRICT},
    error::JS_THROW_VAR_REDECL,
    eval::JS_EVAL_TYPE_GLOBAL,
    function::{
        add_var_this, get_label_pos, instantiate_hoisted_definitions, resolve_scope_private_field,
        resolve_scope_var, skip_dead_code, JSFunctionDef,
    },
    mem::js_resize_array,
    runtime::JS_MAX_LOCAL_VARS,
    utils::{get_u16, get_u32, push_vec_slice, push_vec_u16, push_vec_u32, push_vec_u8, M2},
};

use super::{
    find_var_in_scope,
    optimize::{CodeContext, OPTIMIZE},
    ARG_SCOPE_INDEX,
};

pub const DEFINE_GLOBAL_LEX_VAR: u8 = 1 << 7;
pub const DEFINE_GLOBAL_FUNC_VAR: u8 = 1 << 6;

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
    JS_VAR_FUNCTION_DECL, /* lexical var with function declaration */
    JS_VAR_NEW_FUNCTION_DECL, /* lexical var with async/generator
                          function declaration */
    JS_VAR_CATCH,
    JS_VAR_FUNCTION_NAME, /* function expression name */
    JS_VAR_PRIVATE_FIELD,
    JS_VAR_PRIVATE_METHOD,
    JS_VAR_PRIVATE_GETTER,
    JS_VAR_PRIVATE_SETTER,        /* must come after JS_VAR_PRIVATE_GETTER */
    JS_VAR_PRIVATE_GETTER_SETTER, /* must come after JS_VAR_PRIVATE_SETTER */
}

pub const GLOBAL_VAR_OFFSET: i32 = 0x40000000;
pub const ARGUMENT_VAR_OFFSET: i32 = 0x20000000;

#[repr(C)]
pub struct JSVarBinding {
    pub name: JSAtom,
    pub scope_level: i32,
    pub is_const: bool,
    pub is_let: bool,
    pub is_var: bool,
}

#[repr(C)]
#[derive(Debug)]
pub struct JSVarDef {
    pub var_name: JSAtom,
    pub scope_level: i32,
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

impl JSVarDef {
    pub fn is_const(&self) -> bool {
        self.bitfields & 0x01 != 0
    }

    pub fn set_is_const(&mut self, value: bool) {
        if value {
            self.bitfields |= 0x01;
        } else {
            self.bitfields &= !0x01;
        }
    }

    pub fn is_lexical(&self) -> bool {
        (self.bitfields >> 1) & 0x01 != 0
    }

    pub fn set_is_lexical(&mut self, value: bool) {
        if value {
            self.bitfields |= 0x02;
        } else {
            self.bitfields &= !0x02;
        }
    }

    pub fn is_captured(&self) -> bool {
        (self.bitfields >> 2) & 0x01 != 0
    }

    pub fn set_is_captured(&mut self, value: bool) {
        if value {
            self.bitfields |= 0x04;
        } else {
            self.bitfields &= !0x04;
        }
    }

    pub fn is_static_private(&self) -> bool {
        (self.bitfields >> 3) & 0x01 != 0
    }

    pub fn set_is_static_private(&mut self, value: bool) {
        if value {
            self.bitfields |= 0x08;
        } else {
            self.bitfields &= !0x08;
        }
    }

    pub fn var_kind(&self) -> JSVarKindEnum {
        unsafe { std::mem::transmute((self.bitfields >> 4) & 0x0F) }
    }

    pub fn set_var_kind(&mut self, kind: JSVarKindEnum) {
        let value = kind as u8;
        self.bitfields = (self.bitfields & 0x0F) | (value << 4);
    }
}

#[cfg(debug_assertions)]
impl Drop for JSVarDef {
    fn drop(&mut self) {
        debug_assert_eq!(self.var_name, JS_ATOM_NULL, "forgot free var name atom");
    }
}

/* XXX: should handle the argument scope generically */
#[inline(always)]
pub unsafe fn is_var_in_arg_scope(vd: *const JSVarDef) -> bool {
    matches!(
        (*vd).var_name,
        JS_ATOM__home_object_
            | JS_ATOM_this_active_func
            | JS_ATOM_new_target
            | JS_ATOM_this
            | JS_ATOM__arg_var_
    )
}

#[repr(C)]
pub struct JSGlobalVar {
    pub cpool_idx: i32, // if >= 0, index in the constant pool for hoisted

    // function definition
    // force_init: 1
    // is_lexical: 1
    // is_const: 1
    bitfields: u8,

    pub scope_level: i32, // scope of definition
    pub var_name: JSAtom, // variable name
}

impl JSGlobalVar {
    pub fn force_init(&self) -> bool {
        self.bitfields & 1 != 0
    }
    pub fn is_lexical(&self) -> bool {
        self.bitfields & 2 != 0
    }
    pub fn is_const(&self) -> bool {
        self.bitfields & 4 != 0
    }
    pub fn set_force_init(&mut self, v: bool) {
        self.bitfields = (self.bitfields & !1) | ((v as u8) << 0)
    }
    pub fn set_is_lexical(&mut self, v: bool) {
        self.bitfields = (self.bitfields & !2) | ((v as u8) << 1)
    }
    pub fn set_is_const(&mut self, v: bool) {
        self.bitfields = (self.bitfields & !4) | ((v as u8) << 2)
    }
}

impl JSFunctionDef {
    pub fn find_global_var(&self, name: JSAtom) -> Option<&JSGlobalVar> {
        if self.global_var_count != 0 {
            debug_assert!(!self.global_vars.is_null());
            unsafe { std::slice::from_raw_parts(self.global_vars, self.global_var_count as usize) }
                .iter()
                .find(|x| x.var_name == name)
        } else {
            None
        }
    }

    /// add new argument to the function, returns arg id or error
    pub unsafe fn add_arg(&mut self, ctx: &mut JSContext, name: JSAtom) -> Result<u16, i32> {
        // the local variable indexes are currently stored on 16 bits
        if self.arg_count >= JS_MAX_LOCAL_VARS {
            ctx.JS_ThrowInternalError(c"too many arguments");
            return Err(-1);
        }

        if js_resize_array(
            ctx,
            std::ptr::addr_of_mut!(self.args).cast(),
            std::mem::size_of::<JSVarDef>() as _,
            &mut self.arg_size,
            self.arg_count as i32 + 1,
        ) != 0
        {
            return Err(-1);
        }

        let vd = self.args.offset(self.arg_count as isize).as_mut().unwrap();
        std::ptr::write_bytes(vd, 0, 1);
        vd.var_name = JS_DupAtom(ctx, name);
        vd.func_pool_idx = -1;
        self.arg_count += 1;

        Ok(self.arg_count - 1)
    }

    /// return the new variable index or error
    pub unsafe fn add_var(&mut self, ctx: &mut JSContext, name: JSAtom) -> Result<u16, i32> {
        // the local variable indexes are currently stored on 16 bits
        if self.var_count >= JS_MAX_LOCAL_VARS {
            ctx.JS_ThrowInternalError(c"too many local variables");
            return Err(-1);
        }

        if js_resize_array(
            ctx,
            std::ptr::addr_of_mut!(self.vars).cast(),
            std::mem::size_of::<JSVarDef>() as i32,
            std::ptr::addr_of_mut!(self.var_size),
            self.var_count as i32 + 1,
        ) != 0
        {
            return Err(-1);
        }

        let vd = self.vars.offset(self.var_count as _).as_mut().unwrap();
        self.var_count += 1;
        std::ptr::write_bytes(vd, 0, 1);
        vd.var_name = JS_DupAtom(ctx, name);
        vd.func_pool_idx = -1;

        Ok(self.var_count - 1)
    }

    /// define pseudo var `this`
    pub unsafe fn add_var_this(&mut self, ctx: &mut JSContext) -> Result<u16, i32> {
        let idx = self.add_var(ctx, JS_ATOM_this)?;

        if self.is_derived_class_constructor {
            let vd = self.vars.add(idx as usize);

            // used to trigger 'uninitialized' checks
            // in a derived class constructor
            // XXX: should have is_this flag or var type
            (*vd).set_is_lexical(true);
        }

        Ok(idx)
    }

    /// to add an `argument` definition.
    /// - in_arg_scope=false, will define in the variable scope, returns var idx for `arguments`
    /// - in_arg_scope=true, will define in the argument scope. this is only needed when "eval()" may be called in the argument scope. Return 0 if OK.
    pub unsafe fn add_arguments(
        &mut self,
        ctx: &mut JSContext,
        in_arg_scope: bool,
    ) -> Result<u16, i32> {
        if in_arg_scope {
            if self.arguments_arg_idx.is_none() {
                if find_var_in_scope(ctx, self, JS_ATOM_arguments, ARG_SCOPE_INDEX).is_none() {
                    // XXX: the scope links are not fully updated. May be an
                    // issue if there are child scopes of the argument
                    // scope
                    let idx = self.add_var(ctx, JS_ATOM_arguments)?;

                    (*(self.vars.add(idx as usize))).scope_next =
                        self.scopes[ARG_SCOPE_INDEX as usize].first;
                    self.scopes.get_mut(ARG_SCOPE_INDEX as usize).unwrap().first = idx as i32;
                    (*self.vars.add(idx as usize)).scope_level = ARG_SCOPE_INDEX;
                    (*self.vars.add(idx as usize)).set_is_lexical(true);

                    self.arguments_arg_idx.replace(idx);
                }
            }

            Ok(0)
        } else {
            if let Some(idx) = self.arguments_var_idx {
                Ok(idx)
            } else {
                let idx = self.add_var(ctx, JS_ATOM_arguments)?;
                self.arguments_var_idx.replace(idx);
                Ok(idx)
            }
        }
    }

    pub unsafe fn add_func_var(&mut self, ctx: &mut JSContext, name: JSAtom) -> Result<u16, i32> {
        if let Some(i) = self.func_var_idx {
            Ok(i)
        } else {
            let i = self.add_var(ctx, name)?;
            self.func_var_idx.replace(i);

            let vd = self.vars.add(i as usize);
            (*vd).set_var_kind(JSVarKindEnum::JS_VAR_FUNCTION_NAME as _);
            if self.js_mode & JS_MODE_STRICT as u8 != 0 {
                (*vd).set_is_const(true);
            }

            Ok(i)
        }
    }

    /// return the variable index (add ARGUMENT_VAR_OFFSET for argument variables)
    /// or None if not found.
    pub fn find_arg(&self, name: JSAtom) -> Option<i32> {
        for i in (0..self.arg_count).rev() {
            if unsafe { (*self.args.offset(i as _)).var_name } == name {
                return Some(i as i32 | ARGUMENT_VAR_OFFSET);
            }
        }
        None
    }

    /// find variable by name
    pub fn find_var(&self, name: JSAtom) -> Option<i32> {
        unsafe {
            for i in (0..self.var_count).rev() {
                let vd = self.vars.add(i as usize);
                if (*vd).var_name == name && (*vd).scope_level == 0 {
                    return Some(i as _);
                }
            }
        }

        self.find_arg(name)
    }
}

#[deprecated(note = "use JSFunctionDef::find_global_var() instead")]
pub unsafe fn find_global_var(fd: *mut JSFunctionDef, name: JSAtom) -> *mut JSGlobalVar {
    (*fd)
        .find_global_var(name)
        .map_or(std::ptr::null_mut(), |v| v as *const JSGlobalVar as *mut _)
}

/* add a global variable definition */
pub unsafe fn add_global_var(
    ctx: *mut JSContext,
    s: *mut JSFunctionDef,
    name: JSAtom,
) -> *mut JSGlobalVar {
    if js_resize_array(
        ctx,
        std::ptr::addr_of_mut!((*s).global_vars).cast(),
        std::mem::size_of::<JSGlobalVar>() as _,
        std::ptr::addr_of_mut!((*s).global_var_size),
        (*s).global_var_count as i32 + 1,
    ) != 0
    {
        return std::ptr::null_mut();
    }

    let i = (*s).global_var_count;
    (*s).global_var_count += 1;

    let hf = (*s).global_vars.offset(i as _);
    (*hf).cpool_idx = -1;
    (*hf).set_force_init(false);
    (*hf).set_is_lexical(false);
    (*hf).set_is_const(false);
    (*hf).scope_level = (*s).scope_level;
    (*hf).var_name = JS_DupAtom(ctx, name);

    return hf;
}

#[deprecated(note = "use JSFunctionDef::add_arg() instead")]
pub unsafe fn add_arg(ctx: *mut JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd)
        .add_arg(ctx.as_mut().unwrap(), name)
        .map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::find_arg() instead")]
/* return the variable index or -1 if not found,
add ARGUMENT_VAR_OFFSET for argument variables */
pub unsafe fn find_arg(ctx: *const JSContext, fd: *const JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).find_arg(name).unwrap_or(-1)
}

#[deprecated(note = "use JSFunctionDef::find_var_or_arg() instead")]
pub unsafe fn find_var(ctx: *const JSContext, fd: *const JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).find_var(name).unwrap_or(-1)
}

#[deprecated(note = "use JSFunctionDef::add_var() instead")]
/* return the variable index or -1 if error */
pub unsafe fn add_var(ctx: &mut JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).add_var(ctx, name).map_or(-1, |x| x as i32)
}

pub unsafe fn find_lexical_global_var(fd: *mut JSFunctionDef, name: JSAtom) -> *mut JSGlobalVar {
    if let Some(hf) =
        (*fd)
            .find_global_var(name)
            .and_then(|v| if v.is_lexical() { Some(v) } else { None })
    {
        hf as *const JSGlobalVar as *mut _
    } else {
        std::ptr::null_mut()
    }
}

pub unsafe fn find_lexical_decl(
    ctx: *const JSContext,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    mut scope_idx: i32,
    check_catch_var: bool,
) -> i32 {
    while scope_idx >= 0 {
        let vd = (*fd).vars.offset(scope_idx as isize).as_mut().unwrap();
        if vd.var_name == name
            && (vd.is_lexical()
                || (vd.var_kind() == JSVarKindEnum::JS_VAR_CATCH && check_catch_var))
        {
            return scope_idx;
        }
        scope_idx = vd.scope_next;
    }

    if (*fd).is_eval && (*fd).eval_type == JS_EVAL_TYPE_GLOBAL {
        if !find_lexical_global_var(fd, name).is_null() {
            return GLOBAL_VAR_OFFSET;
        }
    }

    -1
}

// static int get_first_lexical_var(JSFunctionDef *fd, int scope)
pub unsafe fn get_first_lexical_var(fd: *mut JSFunctionDef, mut scope: i32) -> i32 {
    while scope >= 0 {
        let scope_idx = (*fd).scopes[scope as usize].first;
        if scope_idx >= 0 {
            return scope_idx;
        }
        scope = (*fd).scopes[scope as usize].parent;
    }
    return -1;
}

impl JSFunctionDef {
    /// first pass for runtime checks (must be done before the variables are created)
    unsafe fn resolve_variables_check_global_vars(
        &self,
        ctx: &mut JSContext,
        bc_buf: &mut Vec<u8>,
    ) {
        'global_var: for i in 0..self.global_var_count {
            let hf = self.global_vars.add(i as usize);

            // check if global variable (XXX: simplify)
            if let Some(cvars) = self.closure_vars.as_ref() {
                for cv in cvars {
                    match cv.var_name {
                        JS_ATOM__var_ | JS_ATOM__arg_var_ => {
                            continue 'global_var;
                        }
                        name if name == (*hf).var_name => {
                            if (*cv).is_lexical() {
                                // Check if a lexical variable is
                                // redefined as 'var'. XXX: Could abort
                                // compilation here, but for consistency
                                // with the other checks, we delay the
                                // error generation.
                                push_vec_u8(bc_buf, OpCode::THROW_ERROR as u8);
                                push_vec_u32(bc_buf, JS_DupAtom(ctx, (*hf).var_name).0);
                                push_vec_u8(bc_buf, JS_THROW_VAR_REDECL);
                            }
                            continue 'global_var;
                        }
                        _ => {}
                    }
                }
            }

            // for idx in 0..self.closure_var_count {
            //     let cv = self.closure_vars.add(idx as usize);
            //     match (*cv).var_name {
            //         name if name == (*hf).var_name => {
            //             if (*cv).is_lexical() {
            //                 // Check if a lexical variable is
            //                 // redefined as 'var'. XXX: Could abort
            //                 // compilation here, but for consistency
            //                 // with the other checks, we delay the
            //                 // error generation.
            //                 push_vec_u8(bc_buf, OpCode::THROW_ERROR as u8);
            //                 push_vec_u32(bc_buf, JS_DupAtom(ctx, (*hf).var_name).0);
            //                 push_vec_u8(bc_buf, JS_THROW_VAR_REDECL);
            //             }
            //             continue 'global_var;
            //         }
            //         JS_ATOM__var_ | JS_ATOM__arg_var_ => {
            //             continue 'global_var;
            //         }
            //         _ => {}
            //     }
            // }

            push_vec_u8(bc_buf, OpCode::CHECK_DEFINE_VAR as u8);
            push_vec_u32(bc_buf, JS_DupAtom(ctx, (*hf).var_name).0);

            let mut flags = 0;
            if (*hf).is_lexical() {
                flags |= DEFINE_GLOBAL_LEX_VAR;
            }
            if (*hf).cpool_idx >= 0 {
                flags |= DEFINE_GLOBAL_FUNC_VAR;
            }
            push_vec_u8(bc_buf, flags);
        }
    }
}

/// convert global variable accesses to local variables or closure variables when necessary
pub unsafe fn resolve_variables(ctx: *mut JSContext, s: *mut JSFunctionDef) -> Result<(), i32> {
    let bc_buf = (*s).byte_code.as_mut_ptr();
    let bc_len = (*s).byte_code.len();

    let mut cc: CodeContext = std::mem::zeroed();
    cc.bc_buf = bc_buf;
    cc.bc_len = bc_len;

    // TODO: use SmallVec
    let mut bc_out = Vec::<u8>::try_with_capacity(bc_len + 64).map_err(|_| {
        (*ctx).JS_ThrowOutOfMemory();
        -1
    })?;

    // first pass for runtime checks (must be done before the variables are created)
    (*s).resolve_variables_check_global_vars(ctx.as_mut().unwrap(), &mut bc_out);

    macro_rules! no_change {
        ($pos:expr, $len:expr) => {
            push_vec_slice(&mut bc_out, bc_buf.add($pos as usize), $len as usize)
                .expect("TODO: handle bytecode alloc exception");
        };
    }

    let mut line_num = 0_i32; // avoid warning
    let mut pos = 0_usize;
    let mut pos_next: usize;

    while pos < bc_len {
        let op = *bc_buf.add(pos as usize);
        let oi = opcode_info_intermediate(op);
        let len = oi.size as usize;
        pos_next = pos + len;

        match op {
            opcodes::op_line_num => {
                line_num = get_u32(bc_buf.add(pos + 1)) as i32;
                (*s).line_number_size += 1;
                no_change!(pos, len);
            }
            opcodes::OP_eval => {
                // convert scope index to adjusted variable index
                let call_argc = get_u16(bc_buf.add(pos + 1));
                let scope = get_u16(bc_buf.add(pos + 1 + 2));
                mark_eval_captured_variables(ctx, s, scope as _);
                push_vec_u8(&mut bc_out, op);
                push_vec_u16(&mut bc_out, call_argc);
                push_vec_u16(&mut bc_out, ((*s).scopes[scope as usize].first + 1) as _);
            }
            opcodes::OP_apply_eval => {
                // convert scope index to adjusted variable index
                let scope = get_u16(bc_buf.add(pos + 1));
                mark_eval_captured_variables(ctx, s, scope as _);
                push_vec_u8(&mut bc_out, op);
                push_vec_u16(&mut bc_out, ((*s).scopes[scope as usize].first + 1) as _);
            }
            opcodes::op_scope_get_var_checkthis
            | opcodes::op_scope_get_var_undef
            | opcodes::op_scope_get_var
            | opcodes::op_scope_put_var
            | opcodes::op_scope_delete_var
            | opcodes::op_scope_get_ref
            | opcodes::op_scope_put_var_init => {
                let var_name = JSAtom(get_u32(bc_buf.add(pos + 1)));
                let scope = get_u16(bc_buf.add(pos + 5));
                pos_next = resolve_scope_var(
                    ctx,
                    s,
                    var_name,
                    scope as _,
                    op,
                    &mut bc_out,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    pos_next,
                ) as usize;
                JS_FreeAtom(ctx, var_name);
            }
            opcodes::op_scope_make_ref => {
                let var_name = JSAtom(get_u32(bc_buf.add(pos + 1)));
                let label = get_u32(bc_buf.add(pos + 5));
                let scope = get_u16(bc_buf.add(pos + 9));
                let ls = (*s).label_slots.add(label as usize);
                (*ls).ref_count -= 1; // always remove label reference

                // get atom refcount before resolve_scope_var()
                #[cfg(debug_assertions)]
                let _name_refcnt = var_name.ref_count((*ctx).rt());

                pos_next = resolve_scope_var(
                    ctx,
                    s,
                    var_name,
                    scope as _,
                    op,
                    &mut bc_out,
                    bc_buf,
                    ls,
                    pos_next,
                );

                // check atom refcount after resolve_scope_var()
                #[cfg(debug_assertions)]
                debug_assert!(
                    var_name.ref_count((*ctx).rt()) >= _name_refcnt,
                    "[O.o] {var_name:?}, {}",
                    var_name.to_str((*ctx).rt())
                );

                JS_FreeAtom(ctx, var_name);
            }
            opcodes::op_scope_get_private_field
            | opcodes::op_scope_get_private_field2
            | opcodes::op_scope_put_private_field
            | opcodes::op_scope_in_private_field => {
                let var_name = JSAtom(get_u32(bc_buf.add(pos + 1)));
                let scope = get_u16(bc_buf.add(pos + 5));
                if resolve_scope_private_field(
                    ctx.as_mut().unwrap(),
                    s,
                    var_name,
                    scope as i32,
                    op,
                    &mut bc_out,
                ) < 0
                {
                    // fail: continue the copy to keep the atom refcounts consistent
                    // XXX: find a better solution ? */
                    while pos < bc_len {
                        let op_len1 =
                            opcode_info_intermediate(*bc_buf.add(pos as usize)).size as usize;
                        pos_next = pos + op_len1;
                        if push_vec_slice(&mut bc_out, bc_buf.add(pos as usize), op_len1).is_err() {
                            ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
                            break;
                        }
                        pos = pos_next;
                    }
                    bc_out.shrink_to_fit();
                    (*s).byte_code = bc_out;
                    return Err(-1);
                }

                JS_FreeAtom(ctx, var_name);
            }
            opcodes::OP_gosub => {
                (*s).jump_size += 1;
                if OPTIMIZE {
                    // remove calls to empty finalizers
                    let label = get_u32(bc_buf.add(pos + 1));
                    debug_assert!(label < (*s).label_count as u32);
                    let ls = (*s).label_slots.add(label as usize);
                    if cc.code_match((*ls).pos as _, &[OpCode::RET as i32]) {
                        (*ls).ref_count -= 1;
                    } else {
                        no_change!(pos, len);
                    }
                } else {
                    no_change!(pos, len);
                }
            }
            opcodes::OP_drop => {
                // if (0) {
                //     /* remove drops before return_undef */
                //     /* do not perform this optimization in pass2 because
                //        it breaks patterns recognised in resolve_labels */
                //     int pos1 = pos_next;
                //     int line1 = line_num;
                //     while (code_match(&cc, pos1, OP_drop, -1)) {
                //         if (cc.line_num >= 0) line1 = cc.line_num;
                //         pos1 = cc.pos;
                //     }
                //     if (code_match(&cc, pos1, OP_return_undef, -1)) {
                //         pos_next = pos1;
                //         if (line1 != -1 && line1 != line_num) {
                //             line_num = line1;
                //             s->line_number_size++;
                //             dbuf_putc(&bc_out, op_line_num);
                //             dbuf_put_u32(&bc_out, line_num);
                //         }
                //         break;
                //     }
                // }
                no_change!(pos, len);
            }
            opcodes::OP_insert3 => {
                // Optimize transformation:
                // insert3 put_array_el|put_ref_value drop -> put_array_el|put_ref_value */
                if OPTIMIZE
                    && cc.code_match(
                        pos_next,
                        &[
                            M2(OpCode::PUT_ARRAY_EL as i32, OpCode::PUT_REF_VALUE as i32),
                            OpCode::DROP as i32,
                        ],
                    )
                {
                    push_vec_u8(&mut bc_out, cc.op as u8);
                    pos_next = cc.pos;
                    if cc.line_num != -1 && cc.line_num != line_num {
                        line_num = cc.line_num;
                        (*s).line_number_size += 1;
                        push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                        push_vec_u32(&mut bc_out, line_num as u32);
                    }
                } else {
                    no_change!(pos, len);
                }
            }
            opcodes::OP_goto
            | opcodes::OP_tail_call
            | opcodes::OP_tail_call_method
            | opcodes::OP_return
            | opcodes::OP_return_undef
            | opcodes::OP_throw
            | opcodes::OP_throw_error
            | opcodes::OP_ret => {
                if op == opcodes::OP_goto {
                    (*s).jump_size += 1;
                }

                if OPTIMIZE {
                    // remove dead code
                    push_vec_slice(&mut bc_out, bc_buf.add(pos), len as _).expect("TODO");

                    let mut line = -1_i32;
                    pos = skip_dead_code(s, bc_buf, bc_len, pos + len, &mut line);
                    pos_next = pos;
                    if pos < bc_len && line >= 0 && line_num != line {
                        line_num = line;
                        (*s).line_number_size += 1;
                        push_vec_u8(&mut bc_out, OpCodeTemp::LINE_NUM as u8);
                        push_vec_u32(&mut bc_out, line_num as u32);
                    }
                } else {
                    no_change!(pos, len);
                }
            }
            opcodes::op_label => {
                let label = get_u32(bc_buf.add(pos + 1));
                debug_assert!(label < (*s).label_count as u32);
                let ls = (*s).label_slots.add(label as usize);
                debug_assert_eq!((*ls).pos2, -1);
                (*ls).pos2 = bc_out.len() as i32 + oi.size as i32;
                no_change!(pos, len);
            }
            opcodes::op_enter_scope => {
                let scope = get_u16(bc_buf.add(pos as usize + 1)) as i32;
                if scope == (*s).body_scope {
                    instantiate_hoisted_definitions(ctx, s, &mut bc_out);
                }

                let mut scope_idx = (*s).scopes[scope as usize].first;
                while scope_idx >= 0 {
                    let vd = (*s).vars.add(scope_idx as usize);
                    if (*vd).scope_level == scope {
                        if scope_idx as u16 != (*s).arguments_arg_idx.unwrap_or(u16::MAX) {
                            if matches!(
                                (*vd).var_kind(),
                                JSVarKindEnum::JS_VAR_FUNCTION_DECL
                                    | JSVarKindEnum::JS_VAR_NEW_FUNCTION_DECL
                            ) {
                                push_vec_u8(&mut bc_out, OpCode::FCLOSURE as u8);
                                push_vec_u32(&mut bc_out, (*vd).func_pool_idx as u32);
                                push_vec_u8(&mut bc_out, OpCode::PUT_LOC as u8);
                                push_vec_u16(&mut bc_out, scope_idx as u16);
                            } else {
                                /* XXX: should check if variable can be used
                                before initialization */
                                push_vec_u8(&mut bc_out, OpCode::SET_LOC_UNINITIALIZED as u8);
                                push_vec_u16(&mut bc_out, scope_idx as u16);
                            }
                        }
                        scope_idx = (*vd).scope_next;
                    } else {
                        break;
                    }
                }
            }
            opcodes::op_leave_scope => {
                let scope = get_u16(bc_buf.add(pos as usize + 1)) as i32;
                let mut scope_idx = (*s).scopes[scope as usize].first;
                while scope_idx >= 0 {
                    let vd = (*s).vars.add(scope_idx as usize);
                    if (*vd).scope_level == scope {
                        if (*vd).is_captured() {
                            push_vec_u8(&mut bc_out, OpCode::CLOSE_LOC as u8);
                            push_vec_u16(&mut bc_out, scope_idx as u16);
                        }
                        scope_idx = (*vd).scope_next;
                    } else {
                        break;
                    }
                }
            }
            opcodes::OP_set_name => {
                /* remove dummy set_name opcodes */
                let name = JSAtom(get_u32(bc_buf.add(pos as usize + 1)));
                if name != JS_ATOM_NULL {
                    no_change!(pos, len);
                }
            }
            opcodes::OP_if_false | opcodes::OP_if_true | opcodes::OP_catch => {
                (*s).jump_size += 1;
                no_change!(pos, len);
            }
            opcodes::OP_dup => {
                // Optimize:
                // Transformation: dup if_false(l1) drop, l1: if_false(l2) -> if_false(l2) /
                // Transformation: dup if_true(l1) drop, l1: if_true(l2) -> if_true(l2)
                if OPTIMIZE
                    && cc.code_match(
                        pos_next,
                        &[
                            M2(opcodes::OP_if_false as i32, opcodes::OP_if_true as i32),
                            opcodes::OP_drop as i32,
                        ],
                    )
                {
                    let lab0 = cc.label;
                    let mut lab1 = lab0;
                    debug_assert!(lab1 >= 0 && lab1 < (*s).label_count as i32);

                    let op1 = cc.op;
                    let pos1 = cc.pos;
                    let line1 = cc.line_num;
                    let mut pos2: usize;
                    while cc.code_match(
                        {
                            pos2 = get_label_pos(s, lab1) as _;
                            pos2
                        },
                        &[OpCode::DUP as i32, op1, OpCode::DROP as i32],
                    ) {
                        lab1 = cc.label;
                    }

                    if cc.code_match(pos2, &[op1]) {
                        (*s).jump_size += 1;

                        (*s).update_label_ref_count(lab0 as _, -1);
                        (*s).update_label_ref_count(cc.label as _, 1);

                        push_vec_u8(&mut bc_out, op1 as u8);
                        push_vec_u32(&mut bc_out, cc.label as u32);

                        pos_next = pos1;
                        if line1 != -1 && line1 != line_num {
                            line_num = line1;
                            (*s).line_number_size += 1;

                            push_vec_u8(&mut bc_out, opcodes::op_line_num);
                            push_vec_u32(&mut bc_out, line_num as u32);
                        }
                    } else {
                        no_change!(pos, len);
                    }
                } else {
                    no_change!(pos, len);
                }
            }
            opcodes::OP_nop => { /* remove erased code */ }
            opcodes::op_set_class_name => { /* only used during parsing */ }
            opcodes::op_get_field_opt_chain => {
                let name = get_u32(bc_buf.add(pos as usize + 1));
                bc_out.push(opcodes::OP_get_field);
                bc_out.extend_from_slice(name.to_ne_bytes().as_slice());
            }
            opcodes::op_get_array_el_opt_chain => {
                bc_out.push(opcodes::OP_get_array_el);
            }
            _ => {
                // Default: copy opcode as-is
                no_change!(pos, len);
            }
        }

        // check buffer capacity, if no more space left, grow the buffer
        if bc_out.capacity() - bc_out.len() < 16 {
            if bc_out.try_reserve(64).is_err() {
                // can't alloc memory, fail
                ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        pos = pos_next;
    } // while

    // New bytecode is done, set it to JSFunctionDef
    // (*s).byte_code.clear(); // not nessicary, rust will drop the vec automatically
    bc_out.shrink_to_fit();
    (*s).byte_code = bc_out;

    Ok(())
}

pub unsafe fn resolve_pseudo_var(
    ctx: *mut JSContext,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
) -> Option<u16> {
    if !(*s).has_this_binding {
        return None;
    }

    let ctx = ctx.as_mut().unwrap();

    match var_name {
        JS_ATOM__home_object_ => {
            // 'home_object' pseudo variable
            if (*s).home_object_var_idx.is_none() {
                (*s).home_object_var_idx
                    .replace((*s).add_var(ctx, var_name).expect("TODO"));
            }
            (*s).home_object_var_idx
        }
        JS_ATOM_this_active_func => {
            // 'this.active_func' pseudo variable
            if (*s).this_active_func_var_idx.is_none() {
                (*s).this_active_func_var_idx
                    .replace((*s).add_var(ctx, var_name).expect("TODO"));
            }
            (*s).this_active_func_var_idx
        }
        JS_ATOM_new_target => {
            // 'new.target' pseudo variable
            if (*s).new_target_var_idx.is_none() {
                (*s).new_target_var_idx
                    .replace((*s).add_var(ctx, var_name).expect("TODO"));
            }
            (*s).new_target_var_idx
        }
        JS_ATOM_this => {
            // 'this' pseudo variable
            if (*s).this_var_idx.is_none() {
                (*s).this_var_idx = Some((*s).add_var_this(ctx).unwrap());
            }
            (*s).this_var_idx
        }
        _ => None,
    }
}

// test if 'var_name' is in the variable object on the stack. If is it
// the case, handle it and jump to 'label_done'.
// If argument `label_done` is None, new label will be created.
pub unsafe fn var_object_test(
    ctx: *mut JSContext,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
    op: u8,
    bc: &mut Vec<u8>,
    label_done: &mut Option<u32>,
    is_with: bool,
) {
    push_vec_u8(bc, get_with_scope_opcode(op));
    push_vec_u32(bc, JS_DupAtom(ctx, var_name).0);
    let label = if let Some(l) = label_done {
        *l
    } else {
        // create label
        let l = (*s).add_label().expect("TODO: unhandled add_label() error");
        label_done.replace(l);
        l
    };
    push_vec_u32(bc, label);
    push_vec_u8(bc, is_with as u8);

    (*s).update_label_ref_count(label as _, 1);
    (*s).jump_size += 1;
}

pub unsafe fn get_with_scope_opcode(op: u8) -> u8 {
    if op == opcodes::op_scope_get_var_undef {
        opcodes::OP_with_get_var as u8
    } else {
        opcodes::OP_with_get_var + (op as u8 - opcodes::op_scope_get_var)
    }
}

pub unsafe fn mark_eval_captured_variables(
    ctx: *const JSContext,
    s: *mut JSFunctionDef,
    scope_level: i32,
) {
    let mut idx = (*s).scopes[scope_level as usize].first;
    while idx >= 0 {
        let vd = (*s).vars.offset(idx as isize);
        (*vd).set_is_captured(true);
        idx = (*vd).scope_next;
    }
}
