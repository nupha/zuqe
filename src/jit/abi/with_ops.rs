//! Reference objects and `with`-environment access.

use super::*;

/// `make_loc_ref` / `make_arg_ref` / `make_var_ref_ref`: build the ref object
/// and push `[obj, prop]` (`imm` = atom id, `imm2` = slot index). Shared with
/// the interpreter's arm (`ref_ops::make_ref`).
unsafe fn make_ref(f: *mut JitExecFrame, kind: crate::bytecode::ref_ops::RefKind) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::ref_ops::make_ref(
            scope_of(f),
            kind,
            crate::atom::Atom::from_id(f.imm),
            f.imm2 as u16,
            f.arg_base,
            f.var_base,
            &mut *f.arena,
            FrameIndex::from_u32(f.cur_frame_idx),
            (*f.bytecode).arg_count,
            (*f.bytecode).var_count,
            captured_vars_of(f),
        );
        match r {
            Ok((obj, prop)) => {
                *sp = obj;
                *sp.add(1) = prop;
                f.sp = sp.add(2);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `make_loc_ref`.
pub(super) unsafe extern "C" fn h_make_loc_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { make_ref(f, crate::bytecode::ref_ops::RefKind::Loc) }
}

/// `make_arg_ref`.
pub(super) unsafe extern "C" fn h_make_arg_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { make_ref(f, crate::bytecode::ref_ops::RefKind::Arg) }
}

/// `make_var_ref_ref`.
pub(super) unsafe extern "C" fn h_make_var_ref_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { make_ref(f, crate::bytecode::ref_ops::RefKind::VarRefRef) }
}

/// `make_var_ref`: `[^] -> [base, prop, ^]` (`imm` = atom id).
pub(super) unsafe extern "C" fn h_make_var_ref(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        match crate::bytecode::ref_ops::make_global_ref(scope_of(f), atom) {
            Ok((base, prop)) => {
                *sp = base;
                *sp.add(1) = prop;
                f.sp = sp.add(2);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `get_ref_value`: `[obj, prop, ^] -> [obj, value, ^]`.
pub(super) unsafe extern "C" fn h_get_ref_value(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::ref_ops::get_ref_value(scope_of(f), *sp.offset(-2), *sp.offset(-1)) {
            Ok(value) => {
                *sp = value;
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `put_ref_value`: `[obj, prop, val, ^] -> [^]`.
pub(super) unsafe extern "C" fn h_put_ref_value(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::ref_ops::put_ref_value(
            scope_of(f),
            *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-3);
        if r.is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `delete_var`: `[^] -> [bool, ^]` — `delete name` resolves against the global
/// object; it is the fall-through of `with_delete_var` (which only reaches the
/// global object when the with object does not have the name).
pub(super) unsafe extern "C" fn h_delete_var(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        let scope = scope_of(f);
        let mut global = scope.global_obj();
        match global.delete_property(scope, atom) {
            Ok(deleted) => {
                *sp = JSValue::from(deleted);
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `with_get_var` / `with_put_var` / `with_delete_var` / `with_make_ref` /
/// `with_get_ref`: resolve `imm` (atom) on the environment object on top of the
/// stack, with `imm2` as the `is_with` flag (`@@unscopables` applies). Shared
/// with the interpreter (`bytecode::with_ops`).
unsafe fn with_var(f: *mut JitExecFrame, op: WithOp) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        let mut env_obj = *sp.offset(-1);
        let scope = scope_of(f);
        let handled = match crate::bytecode::with_ops::in_env(scope, env_obj, atom, f.imm2 != 0) {
            Ok(handled) => handled,
            Err(_) => return HELPER_EXCEPTION,
        };
        if !handled {
            // Fall through to the ordinary variable access below this op.
            f.sp = sp.offset(-1);
            return HELPER_WITH_MISS;
        }
        match op {
            WithOp::GetVar => match crate::bytecode::with_ops::get_var(scope, env_obj, atom) {
                Ok(value) => {
                    *sp.offset(-1) = value;
                    HELPER_OK
                }
                Err(_) => HELPER_EXCEPTION,
            },
            WithOp::PutVar => {
                let r = crate::bytecode::with_ops::put_var(scope, env_obj, atom, *sp.offset(-2));
                // The window is consumed before the error is examined (the
                // interpreter's `SP_GO!(-2)` runs before its `is_err` check).
                f.sp = sp.offset(-2);
                if r.is_err() {
                    HELPER_EXCEPTION
                } else {
                    HELPER_OK
                }
            }
            WithOp::DeleteVar => {
                match crate::bytecode::with_ops::delete_var(scope, &mut env_obj, atom) {
                    Ok(deleted) => {
                        *sp.offset(-1) = JSValue::from(deleted);
                        HELPER_OK
                    }
                    Err(_) => HELPER_EXCEPTION,
                }
            }
            WithOp::MakeRef => {
                // The object stays; its property name is appended above it.
                match crate::bytecode::with_ops::prop_name(scope, atom) {
                    Ok(prop) => {
                        *sp = prop;
                        f.sp = sp.add(1);
                        HELPER_OK
                    }
                    Err(_) => HELPER_EXCEPTION,
                }
            }
            WithOp::GetRef => match crate::bytecode::with_ops::get_ref(scope, env_obj, atom) {
                Ok(value) => {
                    *sp = value;
                    f.sp = sp.add(1);
                    HELPER_OK
                }
                Err(_) => HELPER_EXCEPTION,
            },
        }
    }
}

/// `with_get_var`: `[env_obj, ^] -> [value, ^]` on a hit.
pub(super) unsafe extern "C" fn h_with_get_var(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::GetVar) }
}

/// `with_put_var`: `[value, env_obj, ^] -> [^]` on a hit.
pub(super) unsafe extern "C" fn h_with_put_var(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::PutVar) }
}

/// `with_delete_var`: `[env_obj, ^] -> [bool, ^]` on a hit.
pub(super) unsafe extern "C" fn h_with_delete_var(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::DeleteVar) }
}

/// `with_make_ref`: `[env_obj, ^] -> [env_obj, prop, ^]` on a hit.
pub(super) unsafe extern "C" fn h_with_make_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::MakeRef) }
}

/// `with_get_ref`: `[env_obj, ^] -> [env_obj, value, ^]` on a hit.
pub(super) unsafe extern "C" fn h_with_get_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::GetRef) }
}

/// Which `with_*` access [`with_var`] performs.
#[derive(Clone, Copy)]
enum WithOp {
    GetVar,
    PutVar,
    DeleteVar,
    MakeRef,
    GetRef,
}
