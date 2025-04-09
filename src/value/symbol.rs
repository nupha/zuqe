use std::{ffi::c_void, rc::Rc};

use crate::{
    atom::{JSAtom, JSAtomInfo, JSAtomTypeHint},
    context::JSContext,
    JS_MKPTR,
};

use super::{
    string::JSString,
    value::{JSTag, JSValue},
};

// `atom` must be a non-numeric string atom
pub unsafe fn JS_NewSymbolFromAtom(
    ctx: *mut JSContext,
    atom: JSAtom,
    atom_type: JSAtomTypeHint,
) -> JSValue {
    let rt = (*ctx).rt.as_ref();
    let p: *mut JSString;

    debug_assert!(!atom.is_int_type());

    let p = (*rt).get_atom_desc(atom).unwrap();
    JS_NewSymbol(ctx, p.clone(), atom_type)
}

/// 'p' is consumed
pub unsafe fn JS_NewSymbol(
    ctx: *mut JSContext,
    p: Rc<JSString>,
    atom_type: JSAtomTypeHint,
) -> JSValue {
    match (*ctx).rt_mut().add_atom(Some(p), atom_type) {
        Ok(a) => {
            JS_MKPTR!(
                JSTag::Symbol,
                Rc::into_raw((*ctx).rt().get_atom_desc(a).unwrap().clone()).cast::<c_void>()
                    as *mut _
            )
        }
        Err(_) => ctx.as_mut().unwrap().JS_ThrowOutOfMemory(),
    }
}

/// return TRUE if 'atom' is a symbol with a string description
pub unsafe fn JS_AtomSymbolHasDescription(ctx: *mut JSContext, atom: JSAtom) -> bool {
    if atom.is_int_type() {
        return false;
    }

    let p = (*ctx).rt().get_atom_desc(atom).unwrap().as_ref();

    matches!(
        p.atom_info().unwrap(),
        JSAtomInfo::Symbol(_) | JSAtomInfo::GlobalSymbol(_)
    ) && !((*p).len() == 0 && (*p).is_wide_char())
}
