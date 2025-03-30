use std::ptr::NonNull;

use crate::{
    atom::{JSAtom, JSAtomType, JSAtomTypeHint, JS_ATOM_HASH_SYMBOL},
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
    JS_MKPTR!(JSTag::String, p.as_ptr().cast()).dup();

    JS_NewSymbol(ctx, p.as_ptr(), atom_type)
}

// 'p' is freed
pub unsafe fn JS_NewSymbol(
    ctx: *mut JSContext,
    p: *mut JSString,
    atom_type: JSAtomTypeHint,
) -> JSValue {
    let rt = (*ctx).rt.as_mut();

    match rt.add_atom(
        if p.is_null() {
            None
        } else {
            Some(Box::from_raw(p))
        },
        atom_type,
    ) {
        Ok((a, ss)) => {
            if let Some(s) = ss {
                Box::leak(s);
            }
            JS_MKPTR!(JSTag::Symbol, rt.get_atom_desc(a).unwrap().as_ptr().cast())
        }
        Err((_, ss)) => {
            if let Some(s) = ss {
                Box::leak(s);
            }
            ctx.as_mut().unwrap().JS_ThrowOutOfMemory()
        }
    }
}

/// return TRUE if 'atom' is a symbol with a string description
pub unsafe fn JS_AtomSymbolHasDescription(ctx: *mut JSContext, atom: JSAtom) -> bool {
    if atom.is_int_type() {
        return false;
    }

    let p = (*ctx).rt().get_atom_desc(atom).unwrap().as_ref();

    return matches!(
        p.atom_type().unwrap(),
        JSAtomType::Symbol(_) | JSAtomType::GlobalSymbol(_)
    ) && !((*p).len() == 0 && (*p).is_wide_char());
}
