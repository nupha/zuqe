use std::cell::{Cell, UnsafeCell};

use crate::{
    JS_CLASS_DATAVIEW,
    array_buffer::ArrayBuffer,
    class::ClassID,
    context::JSContext,
    error::JS_ThrowTypeErrorDetachedArrayBuffer,
    function::NativeFunctionType,
    object::{JSObject, JSObjectWeakref, ObjectPayload},
    object_cast,
    runtime::JSRuntime,
    value::JSValue,
};

use super::{
    TypedArrayPayload,
    class_dataview::{js_dataview_constructor, js_dataview_proto_funcs},
};

pub type DataViewPayload = TypedArrayPayload;

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
                ObjectPayload::DATAVIEW(Box::new(DataViewPayload {
                    obj: UnsafeCell::new(JSObjectWeakref::default()),
                    buffer: array_buffer.clone(),
                    offset: offset as u32,
                    byte_length: len as _,
                    count: Cell::new(0), // no use
                })),
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

        // set back ref to DataView object self
        let dv = Self(o);
        dv.payload().set_obj(&dv.0);

        Ok(dv)
    }

    pub fn payload(&self) -> &DataViewPayload {
        self.0.payload().data_view().unwrap()
    }

    /// check if the TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        self.payload().is_detached()
    }

    pub fn array_buffer(&self) -> &ArrayBuffer {
        self.payload().array_buffer()
    }

    /// Get elements count of typed array.
    /// If underlaying array buffer is detached, returns 0.
    #[inline]
    pub fn length(&self) -> usize {
        #[cfg(debug_assertions)]
        if self.is_detached() {
            debug_assert_eq!(self.payload().count(), 0);
            0
        } else {
            self.payload().count()
        }

        #[cfg(not(debug_assertions))]
        self.payload().count()
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
    pub fn array_buffer_byte_offset(&self, ctx: &JSContext) -> Result<usize, i32> {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            Err(-1)
        } else {
            Ok(self.payload().offset as usize)
        }
    }
}

impl JSContext {
    /// create a DataView with underlaying AarrayBuffer.
    #[deprecated(note = "use DataView::create() instead")]
    pub fn create_dataview(
        &self,
        ctor: &JSValue,
        array_buffer: ArrayBuffer,
        offset: usize,
        length: Option<usize>,
    ) -> Result<DataView, i32> {
        DataView::create(self, ctor, array_buffer, offset, length)
    }
}
