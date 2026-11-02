//! The global-variable inline guard: the two global objects, their
//! shapes and the per-site cache.

use super::*;

/// The cache key a global-variable site records: the shape's payload pointer
/// and its id. `None` when the object has no shape (no own properties).
fn shape_key(obj: &Object) -> Option<(*const crate::object::ShapeImpl, u32)> {
    let shape = obj.shape()?;
    // SAFETY: the shape is owned by the object; the pointer is only ever
    // compared (see `GlobalVarCache`).
    Some((unsafe { shape.gc_ref().as_ptr().as_ptr() }, shape.uid))
}

/// Whether the own property at `slot` is an ordinary data property the inline
/// guard can serve: the shape says `JS_PROP_NORMAL` and the slot still holds a
/// `Normal` value (not an accessor, a variable reference, a lazy initializer
/// or a tombstone).
fn cacheable_slot(obj: &Object, slot: usize, flags: u32) -> bool {
    flags & JS_PROP_TMASK == JS_PROP_NORMAL && matches!(obj.prop_slot(slot), JSProperty::Normal(_))
}

/// Refresh this site's inline-guard cache after a helper call: record
/// the own data property the helper used, or clear the entry so the guard
/// stops paying for a lookup it can never serve.
///
/// A lexical binding shadows `global_obj`, so the global-object arm also
/// records `global_var_obj`'s shape.
unsafe fn refresh_global_cache(f: &mut JitExecFrame, scope: &mut ScopedContext) {
    unsafe {
        if f.global_var_cache.is_null() || f.bytecode.is_null() {
            return;
        }
        let idx = f.imm as usize;
        let Some(var_ref) = (*f.bytecode).global_var_refs().get(idx) else {
            return;
        };
        let name = var_ref.var_name;
        let entry = &mut *f.global_var_cache.add(idx);
        *entry = GlobalVarCache::default();

        let gv = scope.global_var_obj();
        if let Some((slot, flags)) = gv.find_shape_property(name) {
            if cacheable_slot(&gv, slot, flags)
                && let Some((shape, uid)) = shape_key(&gv)
            {
                *entry = GlobalVarCache {
                    owner_shape: shape,
                    lexical_shape: shape,
                    owner_uid: uid,
                    lexical_uid: uid,
                    slot: slot as u32,
                    kind: GLOBAL_CACHE_LEXICAL,
                    flags: flags as u8,
                    _pad: 0,
                };
            }
            return;
        }

        let gobj = scope.global_obj();
        let Some((slot, flags)) = gobj.find_shape_property(name) else {
            return;
        };
        if !cacheable_slot(&gobj, slot, flags) {
            return;
        }
        let Some((owner_shape, owner_uid)) = shape_key(&gobj) else {
            return;
        };
        let Some((lexical_shape, lexical_uid)) = shape_key(&gv) else {
            return;
        };
        *entry = GlobalVarCache {
            owner_shape,
            lexical_shape,
            owner_uid,
            lexical_uid,
            slot: slot as u32,
            kind: GLOBAL_CACHE_GLOBAL,
            flags: flags as u8,
            _pad: 0,
        };
    }
}

/// `get_var` / `get_var_undef`: push a global binding, reusing the interpreter's
/// `get_global_var`. `throw` is true only for `get_var`.
unsafe fn global_get(f: *mut JitExecFrame, throw: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let name = (*f.bytecode).global_var_refs()[f.imm as usize].var_name;
        let result = scope.get_global_var(name, throw);
        refresh_global_cache(f, scope);
        match result {
            Ok(value) => {
                *f.sp = value;
                f.sp = f.sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

pub(super) unsafe extern "C" fn h_get_var(f: *mut JitExecFrame) -> u32 {
    unsafe { global_get(f, true) }
}
pub(super) unsafe extern "C" fn h_get_var_undef(f: *mut JitExecFrame) -> u32 {
    unsafe { global_get(f, false) }
}

/// `put_var` / `put_var_init`: `set_global_var` handles TDZ and read-only checks.
pub(super) unsafe fn global_put(f: *mut JitExecFrame, init: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let name = (*f.bytecode).global_var_refs()[f.imm as usize].var_name;
        let value = *f.sp.offset(-1);
        let mode = if init {
            GlobalVarSetMode::Init
        } else {
            GlobalVarSetMode::Normal
        };
        let result = scope.set_global_var(name, value, mode);
        refresh_global_cache(f, scope);
        f.sp = f.sp.offset(-1);
        if result.is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}
