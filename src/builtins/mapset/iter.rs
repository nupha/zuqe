// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcRef, GcTracable, GcTraceCtx};

use crate::{
    JS_CLASS_MAP, JS_CLASS_MAP_ITERATOR,
    builtins::iterator::JSIteratorKindEnum,
    class::ClassID,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    object::{JS_PROP_CONFIGURABLE, Object, ObjectImpl},
    object_cast,
    value::JSValue,
};

use super::MapPair;

#[derive(Debug)]
pub struct JSMapIteratorData {
    /// None if iterator is done.
    pub obj: Option<Object>,

    /// result kind
    pub kind: JSIteratorKindEnum,

    /// current index of mapset slot
    pub(crate) index: usize,
}

unsafe impl GcTracable for JSMapIteratorData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Some(x) = self.obj {
            x.trace(tr)
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct MapIterator(GcRef<ObjectImpl>);

object_cast!(MapIterator, JS_CLASS_MAP_ITERATOR);

fn js_map_iterator_next(cc: CallContext, state: &mut IteratorState) -> JSResult {
    let ctx = cc.ctx();
    let magic = cc.magic();
    let is_set = magic & 1 != 0;
    let mut this = *cc.this();

    *state = IteratorState::NotDone;

    let isa = this.retrive_object_payload_mut(ctx, JS_CLASS_MAP_ITERATOR + magic as ClassID)?;
    let Some(it) = isa.mapset_iterator_mut() else {
        return Err(-1);
    };

    let mut ret: Option<JSValue> = None;
    let mut end = false;

    if let Some(mut obj) = it.obj {
        let r1 = obj.retrive_payload_mut(ctx, JS_CLASS_MAP + magic as ClassID)?;
        let ms = r1.mapset_mut().unwrap();

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
                            let key = Object(key);
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

pub const js_map_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::ITERATOR_NEXT("next", 0, js_map_iterator_next, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "Map Iterator", JS_PROP_CONFIGURABLE),
];

pub const js_set_iterator_proto_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::ITERATOR_NEXT("next", 0, js_map_iterator_next, 1),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "Set Iterator", JS_PROP_CONFIGURABLE),
];
