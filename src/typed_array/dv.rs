use crate::{
    JS_CLASS_DATAVIEW,
    array_buffer::ArrayBuffer,
    class::ClassID,
    context::JSContext,
    error::JS_ThrowTypeErrorDetachedArrayBuffer,
    function::NativeFunctionType,
    object::{JSObject, ObjectPayload},
    object_cast,
    runtime::JSRuntime,
    value::JSValue,
};

use super::class_dataview::{js_dataview_constructor, js_dataview_proto_funcs};

#[derive(Debug)]
pub struct DataViewPayload {
    /// underlaying array buffer object.
    pub(super) buffer: ArrayBuffer,
    /// start offset in array buffer  
    pub(super) offset: u32,
    /// number of bytes from array buffer
    pub(super) byte_length: u32,
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

    /// get underlaying array buffer object.
    #[inline(always)]
    pub const fn array_buffer(&self) -> &ArrayBuffer {
        &self.buffer
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

    // pub fn set_obj(&self, obj: &JSObject) {
    //     unsafe {
    //         *self.obj.get() = obj.downgrade();
    //     }
    // }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload()
            .with_bytes(|buf| f(&buf[base..base + len]))
    }

    /// run closure with underlaying bytes of typed array.
    pub fn with_bytes_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let base = self.offset as usize;
        let len = self.byte_length as usize;
        self.array_buffer()
            .payload()
            .with_bytes_mut(|buf| f(&mut buf[base..base + len]))
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct DataView(pub(crate) JSObject);

object_cast!(DataView, JS_CLASS_DATAVIEW);

impl DataView {
    pub fn class_initialize(ctx: &JSContext, class_id: ClassID) -> Result<Option<JSObject>, i32> {
        debug_assert_eq!(class_id, JS_CLASS_DATAVIEW);

        let dv_obj = ctx.create_base_object()?;
        dv_obj.install_field_defs(ctx, &js_dataview_proto_funcs)?;

        let dv_ctor: JSValue = ctx
            .create_native_function(
                NativeFunctionType::Constructor(js_dataview_constructor),
                "DataView",
                1,
                0,
            )?
            .into();
        ctx.add_global_constructor("DataView", &dv_ctor, &dv_obj.clone().into());

        Ok(Some(dv_obj))
    }

    pub fn class_finalize(_: &mut JSRuntime, isa: &ObjectPayload, id: usize) {
        if let Some(dv) = isa.data_view() {
            // during the GC the finalizers are called in an arbitrary
            // order so the ArrayBuffer finalize may have been called
            dv.array_buffer().payload().remove_typed_array_weakref(id);
        }
    }

    /// create a DataView with AarrayBuffer, offset and length.
    pub(crate) fn create(
        ctx: &JSContext,
        ctor: &JSValue,
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
                ObjectPayload::DATAVIEW(DataViewPayload::new(array_buffer.clone(), offset, len)),
            )
            .map_err(|_| -1)?;

        // could have been detached in create_object_from_ctor(), reading prototype method.
        if array_buffer.payload().is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        // let o = ctx
        //     .create_object_from_ctor_with(ctor, || {
        //         // Note: test array buffer detach again,
        //         // because get prototype may potentially detach the buffer.
        //         if unlikely(array_buffer.payload().is_detached()) {
        //             JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        //             Err(-1)
        //         } else {
        //             let isa = Box::new(DataViewPayload {
        //                 obj: UnsafeCell::new(JSObjectWeakref::default()),
        //                 buffer: array_buffer,
        //                 offset: offset as u32,
        //                 byte_length: len as _,
        //                 count: Cell::new(0),
        //             });
        //             Ok(ObjectPayload::DATAVIEW(isa))
        //         }
        //     })
        //     .map_err(|_| -1)?;

        let dv = Self(o);
        Ok(dv)
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
    pub fn array_buffer(&self) -> &ArrayBuffer {
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
    pub(super) fn array_buffer_byte_offset(&self, ctx: &JSContext) -> Result<usize, i32> {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            Err(-1)
        } else {
            Ok(self.payload().offset as usize)
        }
    }
}

// impl JSContext {
//     /// create a DataView with underlaying AarrayBuffer.
//     #[deprecated(note = "use DataView::create() instead")]
//     pub fn create_dataview(
//         &self,
//         ctor: &JSValue,
//         array_buffer: ArrayBuffer,
//         offset: usize,
//         length: Option<usize>,
//     ) -> Result<DataView, i32> {
//         DataView::create(self, ctor, array_buffer, offset, length)
//     }
// }
