use std::ops::DerefMut;

use crate::{
    JS_ITERATOR_NEXT_DEF, JS_PROP_STRING_DEF,
    atom::consts::JS_ATOM_lastIndex,
    context::JSContext,
    error::JSResult,
    function::{CallContext, IteratorState, FieldDef},
    object::{JS_PROP_CONFIGURABLE, ObjectDetail},
    value::JSValue,
};

use super::JS_RegExpExec;

fn js_regexp_string_iterator_next(
    ctx: &JSContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let this = cc.this().object().unwrap();

    if let ObjectDetail::REGEXP_STRING_ITERATOR(it) = this.0.payload.borrow_mut().deref_mut() {
        if it.done {
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        let regexp = &it.iterating_regexp;
        let str = &it.iterated_string;

        // initialize state
        *state = IteratorState::NotDone;

        let match_val = JS_RegExpExec(
            ctx,
            regexp.clone(),
            ctx.create_string_with_content(str.clone())?,
        )?;

        if match_val.is_null() {
            it.done = true;
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        if it.global {
            if let JSValue::String(str) = match_val.get_property_u32(ctx, 0)?
                && str.is_empty()
            {
                // matched is empty
                let last_index = regexp
                    .get_property(ctx, JS_ATOM_lastIndex)
                    .and_then(|v| v.coerce_to_u32(ctx))
                    .inspect_err(|_| {
                        *state = IteratorState::NotDone;
                    })? as usize;

                // advance index to next char
                let next_index = it
                    .iterated_string
                    .index_of_next(last_index, it.unicode)
                    .unwrap_or(last_index + 1);

                regexp
                    .set_property(ctx, JS_ATOM_lastIndex, JSValue::from(next_index as u32))
                    .inspect_err(|_| {
                        *state = IteratorState::NotDone;
                    })?;
            }
        } else {
            it.done = true;
        }

        Ok(match_val)
    } else {
        Err(-1)
    }
}

pub static js_regexp_string_iterator_proto_funcs: [FieldDef; 2] = [
    JS_ITERATOR_NEXT_DEF!("next", 0, js_regexp_string_iterator_next, 0),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "RegExp String Iterator",
        JS_PROP_CONFIGURABLE
    ),
];
