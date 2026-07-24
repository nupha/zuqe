// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    class::ClassID,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::Object,
    value::JSValue,
};

use super::{call_iterator_next, make_iterator_next_result};

#[derive(Debug)]
pub(crate) struct IteratorWrapData {
    pub(crate) obj: Object,
    pub(crate) next: JSValue,
}

impl GcTrace for IteratorWrapData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.obj.trace(tr);
        self.next.trace(tr);
    }
}

pub(crate) fn js_iterator_wrap_class_init(
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<(), i32> {
    let iter_proto = ctx.iterator_proto()?;
    let mut proto = ctx.create_base_object_with_proto(Some(iter_proto))?;
    proto.install_fields(ctx, js_iterator_wrap_proto_funcs)?;
    ctx.set_class_proto(class_id, proto);
    Ok(())
}

fn js_iterator_wrap_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let wrap = this_obj.payload().iterator_wrap().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Wrap");
        -1
    })?;

    let (value, done) = call_iterator_next(ctx, wrap.obj, wrap.next)?;
    make_iterator_next_result(ctx, value, done)
}

fn js_iterator_wrap_return(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let wrap = this_obj.payload().iterator_wrap().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Wrap");
        -1
    })?;

    let return_method = wrap
        .obj
        .get_property(ctx, crate::atom::Atom::CONST_return)?;
    if !return_method.is_undefined_or_null() {
        let result = return_method.call(ctx, wrap.obj.into(), &[cc.arg(0)])?;
        return make_iterator_next_result(ctx, result, true);
    }

    make_iterator_next_result(ctx, cc.arg(0), true)
}

pub(crate) const js_iterator_wrap_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("next", 0, js_iterator_wrap_next, 0),
    FieldDef::FUNC("return", 0, js_iterator_wrap_return, 0),
];
