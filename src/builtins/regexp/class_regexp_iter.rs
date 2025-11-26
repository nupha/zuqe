use std::ops::Deref;

use crate::{
    JS_CLASS_REGEXP_STRING_ITERATOR,
    atom::Atom,
    context::ContextRef,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::JS_RegExpExec;

fn js_regexp_string_iterator_next(
    ctx: ContextRef,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let cx = ctx.deref();

    if let ObjectPayload::REGEXP_STRING_ITERATOR(it) = cc
        .this()
        .retrive_object_payload(cx, JS_CLASS_REGEXP_STRING_ITERATOR)?
    {
        if it.done() {
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        // initialize state
        *state = IteratorState::NotDone;

        let res = JS_RegExpExec(cx, it.regexp(), it.string())?;

        if res.is_null() {
            it.set_done(true);
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        } else if it.is_global() {
            let str = res.get_property_i64(cx, 0).and_then(|x| x.to_string(cx))?;

            if str.is_empty() {
                // matched is empty
                let last_index = it
                    .regexp()
                    .get_property(cx, Atom::CONST_lastIndex)
                    .and_then(|v| v.to_u32(cx))
                    .inspect_err(|_| {
                        *state = IteratorState::NotDone;
                    })? as usize;

                // advance index to next char
                let next_index = it
                    .string()
                    .index_of_next(last_index, it.is_unicode())
                    .unwrap_or(last_index + 1);

                it.regexp()
                    .set_property(cx, Atom::CONST_lastIndex, JSValue::from(next_index as u32))
                    .inspect_err(|_| {
                        *state = IteratorState::NotDone;
                    })?;
            }
        } else {
            it.set_done(true);
        }

        Ok(res)
    } else {
        unreachable!()
    }
}

pub static js_regexp_string_iterator_proto_funcs: [FieldDef; 2] = [
    FieldDef::ITERATOR_NEXT("next", 0, js_regexp_string_iterator_next, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "RegExp String Iterator",
        JS_PROP_CONFIGURABLE,
    ),
];
