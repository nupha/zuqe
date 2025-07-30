use crate::{
    atom::{AtomHintAndId, AtomTypeHint, JSAtom},
    context::JSContext,
    error::JSResult,
    string::JSString,
};

use super::value::JSValue;

impl JSContext {
    pub fn create_symbol(&self, str: JSString, atom_type: AtomTypeHint) -> JSResult {
        let rt = self.rt_mut();
        let a = rt.register_atom(Some(str), atom_type)?;
        let ss = rt.get_atom_desc(a).unwrap();
        Ok(JSValue::Symbol(ss))
    }

    /// `atom` must be a non-numeric string atom
    pub fn create_symbol_from_atom(&self, atom: JSAtom, atom_type: AtomTypeHint) -> JSResult {
        debug_assert!(!atom.is_int_type());
        let s = self.rt().get_atom_desc(atom).unwrap().clone();
        self.create_symbol(s, atom_type)
    }
}

/// returned atom is not cloned. caller should clone to keep its reference.
pub(crate) fn symbol_to_atom(val: &JSValue) -> JSAtom {
    if let JSValue::Symbol(s) = val {
        s.atom_info().unwrap().atom()
    } else {
        unreachable!()
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
                AtomHintAndId::Symbol(_) | AtomHintAndId::GlobalSymbol(_)
            ) && !(p.is_empty() && p.is_wide_char())
        })
        .unwrap()
}
