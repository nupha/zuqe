// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_DATAVIEW,
    builtins::array_buffer::ArrayBuffer,
    class::ClassID,
    context::{Context, ContextImpl},
    error::JS_ThrowTypeErrorDetachedArrayBuffer,
    function::Callback,
    object::{Object, ObjectImpl, ObjectPayload},
    object_cast,
    runtime::RuntimeImpl,
    value::JSValue,
};

use super::class_dataview::{js_dataview_constructor, js_dataview_proto_funcs};

#[derive(Debug)]
pub struct DataViewPayload {
    /// underlaying ArrayBuffer object.
    pub(super) buffer: ArrayBuffer,
    /// start offset in ArrayBuffer
    pub(super) offset: u32,
    /// number of bytes from ArrayBuffer
    pub(super) byte_length: u32,
}

impl GcTrace for DataViewPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.buffer.trace(tr);
    }
}

impl DataViewPayload {
    /// create typed array payload with underlying array buffer `buffer`,
    /// offset in array buffer and type array element count.
    fn new(buffer: ArrayBuffer, offset: usize, len: usize) -> Box<Self> {
        let abuf = buffer.payload();
        let byte_length = abuf.byte_length();

        debug_assert!(!abuf.is_detached());
        debug_assert!(offset <= byte_length);
        debug_assert!(offset + len <= byte_length);

        Box::new(Self {
            buffer,
            offset: offset as u32,
            byte_length: len as u32,
        })
    }

    /// get underlaying ArrayBuffer object.
    #[inline(always)]
    pub const fn array_buffer(&self) -> ArrayBuffer {
        self.buffer
    }

    /// check if `self` TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        self.array_buffer().payload().is_detached()
    }

    #[inline(always)]
    pub const fn byte_length(&self) -> usize {
        self.byte_length as _
    }

    /// run closure with underlaying bytes of typed array.
    pub(super) fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload()
            .with_bytes(|buf| f(&buf[base..base + len]))
    }

    /// run closure with underlaying bytes of typed array.
    pub(super) fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload_mut()
            .with_bytes_mut(|buf| f(&mut buf[base..base + len]))
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct DataView(GcRef<ObjectImpl>);

object_cast!(DataView, JS_CLASS_DATAVIEW);

impl DataView {
    pub fn class_initialize(mut ctx: Context, class_id: ClassID) -> Result<Option<Object>, i32> {
        debug_assert_eq!(class_id, JS_CLASS_DATAVIEW);

        let mut dv_obj = ctx.create_base_object()?;
        dv_obj.install_fields(&ctx, js_dataview_proto_funcs)?;

        let dv_ctor: JSValue = ctx
            .create_native_function(
                Callback::Constructor(js_dataview_constructor),
                "DataView",
                1,
                0,
            )?
            .into();
        ctx.add_global_constructor("DataView", dv_ctor, dv_obj.into());

        Ok(Some(dv_obj))
    }

    pub const fn class_finalize(_rt: &mut RuntimeImpl, _isa: &ObjectPayload, _id: usize) {}

    /// create a DataView with AarrayBuffer, offset and length.
    pub(crate) fn create(
        ctx: &ContextImpl,
        ctor: JSValue,
        array_buffer: ArrayBuffer,
        offset: usize,
        length: Option<usize>,
    ) -> Result<Self, i32> {
        debug_assert!(!array_buffer.payload().is_detached());

        let byte_length = array_buffer.payload().byte_length();
        if offset > byte_length {
            ctx.throw_range_error("invalid byteOffset");
            return Err(-1);
        }

        let mut len = byte_length - offset;
        if let Some(l) = length {
            if l > len {
                ctx.throw_range_error("invalid byteLength");
                return Err(-1);
            } else {
                len = l;
            }
        }

        let o = ctx
            .create_object_from_ctor(
                ctor,
                ObjectPayload::DATAVIEW(DataViewPayload::new(array_buffer, offset, len)),
            )
            .map_err(|_| -1)?;

        // could have been detached in create_object_from_ctor(), reading prototype method.
        if array_buffer.payload().is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        Ok(Self(o.0))
    }

    #[inline]
    pub fn payload(&self) -> &DataViewPayload {
        self.0.payload().data_view().unwrap()
    }

    /// check if the TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        self.payload().is_detached()
    }

    #[inline]
    pub fn array_buffer(&self) -> ArrayBuffer {
        self.payload().array_buffer()
    }

    pub fn byte_length(&self) -> usize {
        if self.is_detached() {
            0
        } else {
            self.payload().byte_length()
        }
    }

    /// Get underlying array buffer byte offset.
    /// When underlaying array buffer is detached, returns Err.
    pub(super) fn array_buffer_byte_offset(&self, ctx: &ContextImpl) -> Result<usize, i32> {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            Err(-1)
        } else {
            Ok(self.payload().offset as usize)
        }
    }
}
