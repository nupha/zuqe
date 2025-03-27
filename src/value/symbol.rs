use crate::{
    atom::{JSAtom, JSAtomType, JS_ATOM_HASH_SYMBOL},
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
    atom_type: JSAtomType,
) -> JSValue {
    let rt = (*ctx).rt.as_ref();
    let p: *mut JSString;

    debug_assert!(!atom.is_int_type());
    debug_assert!(atom.0 < rt.atom_size as u32);

    let p = *(rt.atom_array.offset(atom.0 as isize));
    JS_MKPTR!(JSTag::String, p.cast()).dup();

    JS_NewSymbol(ctx, p, atom_type)
}

// 'p' is freed
pub unsafe fn JS_NewSymbol(
    ctx: *mut JSContext,
    p: *mut JSString,
    atom_type: JSAtomType,
) -> JSValue {
    let rt = (*ctx).rt.as_mut();
    // if let Ok(atom) = __JS_NewAtom(rt, p, atom_type) {
    //     JS_MKPTR!(
    //         JSTag::Symbol,
    //         (*rt).atom_array.offset(atom.0 as isize).cast()
    //     )
    // } else {
    //     ctx.as_mut().unwrap().JS_ThrowOutOfMemory()
    // }

    match JSAtom::new(
        rt,
        if p.is_null() {
            None
        } else {
            Some(Box::from_raw(p))
        },
        atom_type,
    ) {
        Ok((a, _ss)) => {
            if let Some(ss) = _ss {
                std::mem::forget(ss);
            }
            JS_MKPTR!(JSTag::Symbol, (*rt.atom_array.offset(a.0 as isize)).cast())
        }
        Err((e, _ss)) => {
            if let Some(ss) = _ss {
                std::mem::forget(ss);
            }
            ctx.as_mut().unwrap().JS_ThrowOutOfMemory()
        }
    }
}

// return TRUE if 'atom' is a symbol with a string description
pub unsafe fn JS_AtomSymbolHasDescription(ctx: *mut JSContext, atom: JSAtom) -> bool {
    if atom.is_int_type() {
        return false;
    }

    let p = *(*ctx).rt().atom_array.offset(atom.0 as isize);
    return (((*p).atom_type() == JSAtomType::Symbol && (*p).hash() == JS_ATOM_HASH_SYMBOL)
        || (*p).atom_type() == JSAtomType::GlobalSymbol)
        && !((*p).len() == 0 && (*p).is_wide_char());
}
