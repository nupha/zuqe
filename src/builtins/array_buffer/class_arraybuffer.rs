use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_ARRAY_BUFFER, JS_CLASS_DATAVIEW,
    JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY, JS_PROP_STRING_DEF,
    branch_hints::unlikely,
    builtins::js_get_this,
    class::ClassID,
    context::ContextRef,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, FieldDef, JS_CallConstructor},
    object::{JS_PROP_CONFIGURABLE, JS_SpeciesConstructor},
    value::JSValue,
};

use super::ArrayBuffer;

//
// Prototype
//

fn js_array_buffer_get_byteLength(ctx: ContextRef, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;

    cc.this()
        .retrive_object_payload(&ctx, class_id)
        .map(|isa| JSValue::from(isa.array_buffer().unwrap().byte_length() as i64))
}

fn js_array_buffer_detached(ctx: ContextRef, cc: CallContext) -> JSResult {
    cc.this()
        .as_object()
        .filter(|o| o.class_id() == JS_CLASS_ARRAY_BUFFER)
        .and_then(|o| {
            o.payload()
                .array_buffer()
                .map(|p| JSValue::Bool(p.is_detached()))
        })
        .ok_or_else(|| {
            ctx.throw_type_error_invalid_class(JS_CLASS_ARRAY_BUFFER);
            -1
        })
}

fn js_array_buffer_slice(ctx: ContextRef, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;

    let abuf = cc
        .this()
        .retrive_object_payload(&ctx, class_id)
        .map(|isa| isa.array_buffer().unwrap())?;

    if abuf.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(&ctx);
        return Err(-1);
    }

    let len = abuf.byte_length() as i64;
    let start: i64 = cc.arg(0).to_i64_clamp(&ctx, 0, len, len)?;
    let end = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i64_clamp(&ctx, 0, len, len)?
    } else {
        len
    };

    let new_len = std::cmp::max(end - start, 0);
    let ctor = JS_SpeciesConstructor(&ctx, *cc.this(), JSValue::UNDEFINED)?;

    let mut new_obj = if ctor.is_undefined() {
        ArrayBuffer::with_capacity_ex(
            &ctx,
            JSValue::UNDEFINED,
            cc.magic() as ClassID == JS_CLASS_SHARED_ARRAY_BUFFER,
            new_len as usize,
            std::ptr::null_mut(),
        )
        .map(|x| x.into())
    } else {
        JS_CallConstructor(&ctx, ctor, &[JSValue::from(new_len)]).err()
    }?;

    let new_abuf = new_obj
        .retrive_object_payload(&ctx, class_id)
        .map(|isa| isa.array_buffer().unwrap())?;

    if unlikely(new_obj.is_same(*cc.this())) {
        ctx.throw_type_error("cannot use identical ArrayBuffer");
        Err(-1)
    } else if unlikely(new_abuf.is_detached()) {
        JS_ThrowTypeErrorDetachedArrayBuffer(&ctx);
        Err(-1)
    } else if unlikely(new_abuf.byte_length() < new_len as usize) {
        ctx.throw_type_error("new ArrayBuffer is too small");
        Err(-1)
    } else {
        // must test again because of side effects
        if abuf.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(&ctx);
            return Err(-1);
        }

        let new_abuf_mut = new_obj
            .retrive_object_payload_mut(&ctx, class_id)
            .map(|isa| isa.array_buffer_mut().unwrap())?;

        abuf.with_bytes(|src| {
            new_abuf_mut.with_bytes_mut(|dst| unsafe {
                std::ptr::copy_nonoverlapping(
                    src.as_ptr().offset(start as isize),
                    dst.as_mut_ptr(),
                    new_len as usize,
                );
            })
        });

        Ok(new_obj)
    }
}

pub static js_array_buffer_proto_funcs: [FieldDef; 4] = [
    JS_CGETSET_NATIVE_DEF!(
        "byteLength",
        js_array_buffer_get_byteLength,
        NONE,
        JS_CLASS_ARRAY_BUFFER as i16
    ),
    JS_CGETSET_NATIVE_DEF!("detached", js_array_buffer_detached, NONE, 0),
    JS_CFUNC_NATIVE_DEF!(
        "slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_ARRAY_BUFFER as i16
    ),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "ArrayBuffer", JS_PROP_CONFIGURABLE),
];

pub static js_shared_array_buffer_proto_funcs: [FieldDef; 3] = [
    JS_CGETSET_NATIVE_DEF!(
        "byteLength",
        js_array_buffer_get_byteLength,
        NONE,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16
    ),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "SharedArrayBuffer",
        JS_PROP_CONFIGURABLE as i16
    ),
];

//
// static methods
//

pub fn js_array_buffer_isView(_: ContextRef, cc: CallContext) -> JSResult {
    let res = if let JSValue::Object(p) = cc.arg(0)
        && (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_DATAVIEW).contains(&p.class_id())
    {
        true
    } else {
        false
    };
    Ok(JSValue::Bool(res))
}

pub static js_array_buffer_funcs: [FieldDef; 2] = [
    JS_CFUNC_NATIVE_DEF!("isView", 1, js_array_buffer_isView, 0),
    JS_CGETSET_NATIVE_DEF!("[Symbol.species]", js_get_this, NONE, 0),
];

pub static js_shared_array_buffer_funcs: [FieldDef; 1] = [JS_CGETSET_NATIVE_DEF!(
    "[Symbol.species]",
    js_get_this,
    NONE,
    0
)];
