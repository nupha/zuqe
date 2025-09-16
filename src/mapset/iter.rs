use crate::{
    JS_CLASS_MAP, JS_CLASS_MAP_ITERATOR, JS_ITERATOR_NEXT_DEF, JS_PROP_STRING_DEF,
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    iterator::JSIteratorKindEnum,
    mapset::MapPair,
    object::{JS_PROP_CONFIGURABLE, JSObject, ObjectPayload},
    object_cast,
    runtime::JSRuntime,
    value::JSValue,
};

#[derive(Debug)]
pub struct JSMapIteratorData {
    /// None if iterator is done.
    pub obj: Option<JSObject>,

    /// result kind
    pub kind: JSIteratorKindEnum,

    /// current index of mapset slot
    pub(crate) index: usize,
}

#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct MapIterator(pub(crate) JSObject);

object_cast!(MapIterator, JS_CLASS_MAP_ITERATOR);

pub fn js_map_iterator_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {}

pub fn js_map_iterator_mark(val: JSValue) {
    if let Some(o) = val.as_object() {
        if let Some(it) = o.0.payload.borrow().map_iterator_state() {
            if let Some(o) = &it.obj {
                o.gc_mark()
            }
        }
    }
}

fn js_map_iterator_next(ctx: &JSContext, cc: CallContext, state: &mut IteratorState) -> JSResult {
    let is_set = cc.magic() & 1 != 0;
    *state = IteratorState::NotDone;

    let mut r = cc
        .this()
        .retrive_object_payload_mut(ctx, Some(JS_CLASS_MAP_ITERATOR + cc.magic() as JSClassID))?;

    let Some(it) = r.map_iterator_state_mut() else {
        return Err(-1);
    };

    let mut ret: Option<JSValue> = None;
    let mut end = false;

    if let Some(obj) = &it.obj {
        let r1 = obj.retrive_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))?;
        let ms = r1.map_state().unwrap();

        if it.index < ms.slots.len() {
            let slice = &ms.slots[it.index..];
            let mut idx = it.index;
            let mut pair: Option<(JSValue, JSValue)> = None;

            for kv in slice {
                idx += 1;

                match kv {
                    MapPair::Strong { key, value } => {
                        let val = if is_set { key.clone() } else { value.clone() };
                        pair.replace((key.clone(), val));
                        break;
                    }
                    MapPair::Weak { key, value } => {
                        if let Some(key) = key.upgrade() {
                            pair.replace((
                                key.clone().into(),
                                if is_set { key.into() } else { value.clone() },
                            ));
                            break;
                        }
                    }
                    _ => {}
                }
            }

            it.index = idx;

            if let Some((key, val)) = pair {
                let v = match it.kind {
                    JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => key,
                    JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => val,
                    JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                        ctx.create_array_with_values(vec![key, val])?.into()
                    }
                };
                ret.replace(v);
            }
        }

        if it.index >= ms.slots.len() {
            end = true;
        }
    }

    if end {
        it.obj.take(); // mark end of iterator
    }

    if let Some(ret) = ret {
        *state = IteratorState::NotDone;
        Ok(ret)
    } else {
        *state = IteratorState::Done;
        Ok(JSValue::UNDEFINED)
    }
}

pub static js_map_iterator_proto_funcs: [FieldDef; 2] = [
    JS_ITERATOR_NEXT_DEF!("next", 0, js_map_iterator_next, 0),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "Map Iterator",
        JS_PROP_CONFIGURABLE as i16
    ),
];

pub static js_set_iterator_proto_funcs: [FieldDef; 2] = [
    JS_ITERATOR_NEXT_DEF!("next", 0, js_map_iterator_next, 1),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "Set Iterator",
        JS_PROP_CONFIGURABLE as i16
    ),
];
