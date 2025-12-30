use crate::{
    JS_CLASS_REGEXP_STRING_ITERATOR,
    atom::Atom,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::JS_RegExpExec;

fn js_regexp_string_iterator_next(cc: CallContext, state: &mut IteratorState) -> JSResult {
    let ctx = cc.ctx();

    if let ObjectPayload::REGEXP_STRING_ITERATOR(it) = cc
        .this()
        .retrive_object_payload(ctx, JS_CLASS_REGEXP_STRING_ITERATOR)?
    {
        if it.done() {
            *state = IteratorState::Done;
            return Ok(JSValue::UNDEFINED);
        }

        // initialize state
        *state = IteratorState::NotDone;

        match JS_RegExpExec(cc.ctx_ptr(), it.regexp().into(), it.string())? {
            JSValue::Object(mat) => {
                if it.is_global() {
                    let str = mat
                        .get_property_i64(ctx, 0)
                        .and_then(|x| x.to_string(ctx))?;

                    if str.is_empty() {
                        // if matched is empty, advance index to next char
                        let last_index = it
                            .regexp()
                            .get_property(ctx, Atom::CONST_lastIndex)
                            .and_then(|v| v.to_length(ctx))
                            .inspect_err(|_| {
                                *state = IteratorState::NotDone;
                            })?;

                        let next_index = it
                            .string()
                            .index_of_next_i64(last_index, it.is_unicode())
                            .unwrap_or(last_index + 1);

                        it.regexp()
                            .set_property(ctx, Atom::CONST_lastIndex, JSValue::from(next_index))
                            .inspect_err(|_| {
                                *state = IteratorState::NotDone;
                            })?;
                    }
                } else {
                    it.set_done(true);
                }

                Ok(mat.into())
            }
            JSValue::NULL => {
                it.set_done(true);
                *state = IteratorState::Done;
                Ok(JSValue::UNDEFINED)
            }
            _ => {
                unreachable!()
            }
        }
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
