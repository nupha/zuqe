use std::ops::{Deref, DerefMut};

use crate::{
    JS_ITERATOR_NEXT_DEF, JS_PROP_STRING_DEF,
    atom::consts::JS_ATOM_lastIndex,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::JS_RegExpExec;

fn js_regexp_string_iterator_next(
    ctx: &JSContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let this = cc.this().as_object().unwrap();

    if let ObjectPayload::REGEXP_STRING_ITERATOR(it) = this.payload() {
        if it.borrow().done {
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        let (regexp, iterated_string, global, unicode) = {
            let b = it.borrow();
            (
                b.iterating_regexp.clone(),
                b.iterated_string.clone(),
                b.global,
                b.unicode,
            )
        };

        // initialize state
        *state = IteratorState::NotDone;

        let match_val = JS_RegExpExec(
            ctx,
            regexp.clone(),
            ctx.create_string_with_content(iterated_string.clone())?,
        )?;

        if match_val.is_null() {
            it.borrow_mut().done = true;
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        if global {
            if let JSValue::String(str) = match_val.get_property_u32(ctx, 0)?
                && str.is_empty()
            {
                // matched is empty
                let last_index = regexp
                    .get_property(ctx, JS_ATOM_lastIndex)
                    .and_then(|v| v.to_u32(ctx))
                    .inspect_err(|_| {
                        *state = IteratorState::NotDone;
                    })? as usize;

                // advance index to next char
                let next_index = iterated_string
                    .index_of_next(last_index, unicode)
                    .unwrap_or(last_index + 1);

                regexp
                    .set_property(ctx, JS_ATOM_lastIndex, JSValue::from(next_index as u32))
                    .inspect_err(|_| {
                        *state = IteratorState::NotDone;
                    })?;
            }
        } else {
            it.borrow_mut().done = true;
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
