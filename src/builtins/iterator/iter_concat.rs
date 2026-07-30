// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::Cell;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    class::ClassID,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::Object,
    value::JSValue,
};

use super::{call_iterator_next, iterator_return, make_iterator_next_result};

/// Payload for `Iterator.concat` — lazily concatenates iterables.
#[derive(Debug)]
pub(crate) struct IteratorConcatData {
    pub(crate) values: Vec<(JSValue, JSValue)>,
    pub(crate) index: Cell<usize>,
    pub(crate) iter: Cell<Option<Object>>,
    pub(crate) next: Cell<JSValue>,
    pub(crate) running: Cell<bool>,
}

impl GcTrace for IteratorConcatData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        for (obj, method) in &self.values {
            obj.trace(tr);
            method.trace(tr);
        }
        if let Some(o) = self.iter.get() {
            o.trace(tr);
            self.next.get().trace(tr);
        }
    }
}

pub(crate) fn js_iterator_concat_class_init(
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<(), i32> {
    let iter_proto = ctx.iterator_proto()?;
    let mut proto = ctx.create_base_object_with_proto(Some(iter_proto))?;
    proto.install_fields(ctx, js_iterator_concat_proto_funcs)?;
    ctx.set_class_proto(class_id, proto);
    Ok(())
}

pub(crate) fn js_iterator_concat_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this
        .as_object()
        .ok_or_else(|| ctx.throw_type_error_not_an_object())?;

    let cd = this_obj
        .payload()
        .iterator_concat()
        .ok_or_else(|| ctx.throw_type_error("not an Iterator Concat"))?;

    if cd.running.get() {
        return Err(ctx.throw_type_error("Iterator Concat is already executing"));
    }

    cd.running.set(true);

    let values = &cd.values;
    let mut idx = cd.index.get();

    loop {
        if idx >= values.len() {
            cd.running.set(false);
            break make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
        }

        let (ref obj, ref method) = values[idx];

        // Create a new iterator from the current object if none active
        let cur_iter = cd.iter.get();
        if cur_iter.is_none() {
            let iter_val = method.call(ctx, *obj, &[])?;
            let iter_obj = iter_val.ensure_object(ctx)?;
            let next_method = iter_obj.get_property(ctx, crate::atom::Atom::CONST_next)?;
            cd.iter.set(Some(iter_obj));
            cd.next.set(next_method);
        }

        let cur_iter_obj = cd.iter.get().unwrap();
        let next_method = cd.next.get();

        let (value, done) = call_iterator_next(ctx, cur_iter_obj, next_method)?;

        if !done {
            cd.running.set(false);
            break make_iterator_next_result(ctx, value, false);
        }

        // Exhausted this iterator, advance to next
        cd.index.set(idx + 1);
        idx = cd.index.get();
        cd.iter.set(None);
        cd.next.set(JSValue::UNDEFINED);
    }
}

pub(crate) fn js_iterator_concat_return(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this
        .as_object()
        .ok_or_else(|| ctx.throw_type_error_not_an_object())?;

    let cd = this_obj
        .payload()
        .iterator_concat()
        .ok_or_else(|| ctx.throw_type_error("not an Iterator Concat"))?;

    if cd.running.get() {
        return Err(ctx.throw_type_error("Iterator Concat is already executing"));
    }

    cd.running.set(true);

    let result = (|| -> JSResult {
        // Close current iterator (may re-enter via recursive return())
        if let Some(cur_iter) = cd.iter.get() {
            iterator_return(ctx, cur_iter)?;
            cd.iter.set(None);
            cd.next.set(JSValue::UNDEFINED);
        }

        // Skip remaining iterables
        cd.index.set(cd.values.len());
        make_iterator_next_result(ctx, cc.arg(0), true)
    })();

    cd.running.set(false);
    result
}

pub(crate) const js_iterator_concat_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("next", 0, js_iterator_concat_next, 0),
    FieldDef::FUNC("return", 0, js_iterator_concat_return, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Iterator Concat",
        crate::object::JS_PROP_CONFIGURABLE,
    ),
];
