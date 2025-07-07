use crate::{
    atom::{JSAtom, JSAtomInfo, JSAtomTypeHint},
    context::JSContext,
    string::JSString,
};

use super::value::JSValue;

// `atom` must be a non-numeric string atom
pub unsafe fn JS_NewSymbolFromAtom(
    ctx: &JSContext,
    atom: JSAtom,
    atom_type: JSAtomTypeHint,
) -> JSValue {
    debug_assert!(!atom.is_int_type());

    let p = ctx.rt().get_atom_desc(atom).unwrap().clone();
    JS_NewSymbol(ctx, p, atom_type)
}

/// 'p' is consumed
pub fn JS_NewSymbol(ctx: &JSContext, p: JSString, atom_type: JSAtomTypeHint) -> JSValue {
    match ctx.rt_mut().register_atom(Some(p), atom_type) {
        Ok(a) => {
            let ss = ctx.rt().get_atom_desc(a).unwrap();
            JSValue::String(ss)
        }
        Err(_) => ctx.JS_ThrowOutOfMemory(),
    }
}

/// return TRUE if 'atom' is a symbol with a string description
pub fn JS_AtomSymbolHasDescription(ctx: &JSContext, atom: JSAtom) -> bool {
    if atom.is_int_type() {
        return false;
    }

    ctx.rt()
        .get_atom_desc(atom)
        .map(|p| {
            matches!(
                p.atom_info().unwrap(),
                JSAtomInfo::Symbol(_) | JSAtomInfo::GlobalSymbol(_)
            ) && !(p.is_empty() && p.is_wide_char())
        })
        .unwrap()
}
