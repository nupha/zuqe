// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    builtins::array_buffer::ArrayBuffer,
    context::ScopedContext,
    object::{DataView, ObjectPayload},
    value::JSValue,
};

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
        // the buffer may already be detached: argument coercion in the
        // constructor can run user code. DataView::create() throws right
        // after instance creation, and every accessor re-checks detachment
        // before touching the bytes.
        debug_assert!(
            buffer.payload().is_detached() || offset + len <= buffer.payload().byte_length()
        );

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

    /// Throw a TypeError if the underlying buffer has been detached.
    #[inline]
    pub fn ensure_attached(&self, ctx: &mut ScopedContext) -> Result<(), i32> {
        self.array_buffer().payload().ensure_attached(ctx)
    }

    #[inline(always)]
    pub const fn byte_length(&self) -> usize {
        self.byte_length as _
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload()
            .with_bytes(|buf| f(&buf[base..base + len]))
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload_mut()
            .with_bytes_mut(|buf| f(&mut buf[base..base + len]))
    }
}

impl DataView {
    /// create a DataView with ArrayBuffer, byte offset and byte length.
    /// `offset`/`length` must have been validated by the caller. The buffer
    /// may already be detached (the byteLength argument coercion can run
    /// user code) — per ES §25.3.2.1 this is only caught here, after
    /// instance creation.
    pub(crate) fn create(
        ctx: &mut ScopedContext,
        ctor: JSValue,
        array_buffer: ArrayBuffer,
        offset: usize,
        length: usize,
    ) -> Result<Self, i32> {
        let o = ctx
            .create_instance_with_ctor(
                ctor,
                ObjectPayload::DATAVIEW(DataViewPayload::new(array_buffer, offset, length)),
            )
            .map_err(|_| -1)?;

        // the buffer could have been detached by the byteLength argument
        // coercion, or in create_instance_with_ctor() reading the prototype
        array_buffer.payload().ensure_attached(ctx)?;

        Ok(Self(o.0))
    }

    #[inline(always)]
    pub fn payload(&self) -> &DataViewPayload {
        unsafe { self.0.as_ref().payload() }.data_view().unwrap()
    }

    /// check if the TypedArray is detached.
    #[inline(always)]
    pub fn is_detached(&self) -> bool {
        self.payload().is_detached()
    }

    /// Throw a TypeError if the underlying buffer has been detached.
    #[inline]
    pub fn ensure_attached(&self, ctx: &mut ScopedContext) -> Result<(), i32> {
        self.payload().ensure_attached(ctx)
    }

    #[inline(always)]
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
    pub(super) fn array_buffer_byte_offset(&self, ctx: &mut ScopedContext) -> Result<usize, i32> {
        self.ensure_attached(ctx)?;
        Ok(self.payload().offset as usize)
    }
}
