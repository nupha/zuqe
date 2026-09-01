// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_DATAVIEW,
    builtins::array_buffer::ArrayBuffer,
    context::ScopedContext,
    object::{DataView, PayloadPtr},
    value::JSValue,
};

#[derive(Debug)]
pub struct DataViewPayload {
    /// underlaying ArrayBuffer object.
    pub(super) buffer: ArrayBuffer,
    /// start offset in ArrayBuffer
    pub(super) offset: u32,
    /// number of bytes from ArrayBuffer; unused when `length_tracked`.
    pub(super) byte_length: u32,
    /// `[[ByteLength]]` is `auto`: created without an explicit byteLength
    /// argument over a resizable buffer; length follows the buffer.
    pub(super) length_tracked: bool,
}

impl GcTrace for DataViewPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.buffer.trace(tr);
    }
}

impl DataViewPayload {
    /// create typed array payload with underlying array buffer `buffer`,
    /// offset in array buffer and type array element count.
    fn new(buffer: ArrayBuffer, offset: usize, len: usize, length_tracked: bool) -> Self {
        // the buffer may already be detached: argument coercion in the
        // constructor can run user code. DataView::create() throws right
        // after instance creation, and every accessor re-checks detachment
        // before touching the bytes.
        debug_assert!(
            buffer.payload().is_detached()
                || length_tracked
                || offset + len <= buffer.payload().byte_length()
        );

        Self {
            buffer,
            offset: offset as u32,
            byte_length: len as u32,
            length_tracked,
        }
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
    pub fn byte_length(&self) -> usize {
        self.byte_length as _
    }

    /// start offset in the underlying ArrayBuffer.
    #[inline(always)]
    pub const fn byte_offset(&self) -> usize {
        self.offset as _
    }

    /// IsViewOutOfBounds (§25.3.1.4): true when the buffer is detached or the
    /// view's start/end extends past the buffer's *current* byte length. For
    /// auto views the end always coincides with the buffer end.
    pub fn is_out_of_bounds(&self) -> bool {
        let ab = self.array_buffer();
        let abuf = ab.payload();
        if abuf.is_detached() {
            return true;
        }

        let buf_len = abuf.byte_length();
        let start = self.offset as usize;
        let end = if self.length_tracked {
            buf_len
        } else {
            start + self.byte_length as usize
        };
        start > buf_len || end > buf_len
    }

    /// GetViewByteLength (§25.3.1.3): the stored length for fixed-size views,
    /// `buffer.byteLength - offset` for auto (length-tracking) views.
    /// Caller must first rule out the out-of-bounds state.
    pub fn view_byte_length(&self) -> usize {
        if self.length_tracked {
            self.array_buffer().payload().byte_length() - self.offset as usize
        } else {
            self.byte_length as usize
        }
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.view_byte_length();
        self.array_buffer()
            .payload()
            .with_bytes(|buf| f(&buf[base..base + len]))
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.view_byte_length();
        self.array_buffer()
            .payload_mut()
            .with_bytes_mut(|buf| f(&mut buf[base..base + len]))
    }
}

impl DataView {
    /// create a DataView with ArrayBuffer, byte offset and byte length.
    /// `length_tracked` marks an `auto` ([[ByteLength]]) view whose length
    /// follows the buffer. `offset`/`length` must have been validated by the
    /// caller. The buffer may already be detached (the byteLength argument
    /// coercion can run user code) — per ES §25.3.2.1 this is only caught
    /// here, after instance creation.
    pub(crate) fn create(
        ctx: &mut ScopedContext,
        ctor: JSValue,
        array_buffer: ArrayBuffer,
        offset: usize,
        byte_length: usize,
        length_tracked: bool,
    ) -> Result<Self, i32> {
        let o = ctx.ordinary_create_from_constructor_payload(
            ctor,
            PayloadPtr::new(
                JS_CLASS_DATAVIEW,
                DataViewPayload::new(array_buffer, offset, byte_length, length_tracked),
            ),
        )?;

        // §25.3.2.1 steps 11-14: reading `newTarget.prototype` can run user
        // code that detaches or resizes the buffer; re-check detach and
        // bounds against fresh state. The half-initialized instance is
        // discarded when these throw, exactly like in the spec algorithm.
        array_buffer.payload().ensure_attached(ctx)?;
        let buf_len = array_buffer.payload().byte_length();
        if offset > buf_len {
            return Err(ctx.throw_range_error("invalid byteOffset"));
        }
        if !length_tracked && offset + byte_length > buf_len {
            return Err(ctx.throw_range_error("invalid byteLength"));
        }

        Ok(Self(o.0))
    }

    #[inline(always)]
    pub fn payload(&self) -> &DataViewPayload {
        unsafe { self.0.as_ref() }.payload_data_view().unwrap()
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
            self.payload().view_byte_length()
        }
    }
}
