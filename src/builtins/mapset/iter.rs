// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Array, JS_CLASS_MAP, JS_CLASS_MAP_ITERATOR, ScopedContext,
    builtins::iterator::JSIteratorKindEnum,
    class::ClassID,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    object::{JS_PROP_CONFIGURABLE, Object},
    value::JSValue,
};

use super::{JSMapState, MapPair};

#[derive(Debug)]
pub struct JSMapIteratorData {
    /// None if iterator is done.
    pub obj: Option<Object>,

    /// result kind
    pub kind: JSIteratorKindEnum,

    /// current index of mapset slot
    pub(crate) index: usize,
}

impl GcTrace for JSMapIteratorData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Some(x) = self.obj {
            x.trace(tr)
        }
    }
}

fn js_map_iterator_next(
    ctx: &mut ScopedContext,
    cc: CallContext,
    state: &mut IteratorState,
) -> JSResult {
    let magic = cc.magic();

    let is_set = magic & 1 != 0;
    let mut this = cc.this();

    *state = IteratorState::NotDone;

    let it: &mut JSMapIteratorData =
        this.class_payload_mut(ctx, JS_CLASS_MAP_ITERATOR + magic as ClassID)?;

    let mut ret: Option<JSValue> = None;
    let mut end = false;

    if let Some(obj) = it.obj {
        let mut objv: JSValue = obj.into();
        let ms: &mut JSMapState = objv.class_payload_mut(ctx, JS_CLASS_MAP + magic as ClassID)?;

        if it.index < ms.slots.len() {
            let slice = &ms.slots[it.index..];
            let mut idx = it.index;
            let mut pair: Option<(JSValue, JSValue)> = None;

            for kv in slice {
                idx += 1;

                match kv {
                    MapPair::Strong { key, value } => {
                        let val = if is_set { *key } else { *value };
                        pair.replace((*key, val));
                        break;
                    }
                    MapPair::Weak { key, value } => {
                        if let Some(key) = key.upgrade(ctx.rt_ptr().gc_heap()) {
                            let key = Object(key.into_raw());
                            pair.replace((key.into(), if is_set { key.into() } else { *value }));
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
                        Array::from_values(ctx, vec![key, val])?.into()
                    }
                };
                ret.replace(v);
            }
        }

        // Only mark iterator as exhausted if no entry was found in the
        // current scan. If we found an entry, keep the iterator alive so
        // newly added entries can be visited on subsequent .next() calls
        // (ECMA-262 24.1.5.2.1 step 11: "numEntries must be redetermined
        // every time this method is evaluated").
        if ret.is_none() && it.index >= ms.slots.len() {
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

pub const js_map_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 0, js_map_iterator_next, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Map Iterator", JS_PROP_CONFIGURABLE),
];

pub const js_set_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 0, js_map_iterator_next, 1),
    FieldDef::STR("[Symbol.toStringTag]", "Set Iterator", JS_PROP_CONFIGURABLE),
];
