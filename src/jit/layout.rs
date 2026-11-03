// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The layouts the generated code reads.
//!
//! The JIT's inline guards load fields straight out of the engine's objects,
//! so the offsets are a **contract** between the engine and the generated
//! code. They are `offset_of!`-derived: a layout change moves them with the
//! code instead of silently breaking the guards.
//!
//! Every contract here is pinned by a test (in the module that owns it) and
//! by the end-to-end tests in `jit::tests` and `tests/jit/`.

/// The object's layout: the flags word, the payload pointer, the shape
/// pointer and the property slots.
///
/// The masks mirror [`crate::object::ObjectFlags`]' own, so they cannot drift
/// from it either.
pub(crate) mod object {
    use crate::object::{ObjectImpl, PropView, ShapeImpl};

    /// The distance from a **GC node pointer** to the object's fields. A
    /// `JSValue` holds the *node* pointer (the object's payload follows the
    /// node header), so the offsets the generated code uses are relative to
    /// the node — not to the `ObjectImpl`.
    const NODE_TO_OBJECT: usize = gc_lite::gctype::payload_offset_of::<ObjectImpl>();

    /// The offset of the flags word (the class id + the object flags).
    pub(crate) const FLAGS_OFFSET: i32 =
        (NODE_TO_OBJECT + std::mem::offset_of!(ObjectImpl, obj_flags)) as i32;
    /// The offset of the type-erased payload pointer.
    pub(crate) const PAYLOAD_OFFSET: i32 =
        (NODE_TO_OBJECT + std::mem::offset_of!(ObjectImpl, payload)) as i32;
    /// The offset of the object's shape pointer (`None` is a null pointer).
    pub(crate) const SHAPE_OFFSET: i32 = (NODE_TO_OBJECT + ObjectImpl::SHAPE_FIELD_OFFSET) as i32;
    /// The class id lives in the low 16 bits of the flags word.
    pub(crate) const CLASS_ID_MASK: i64 = 0x0000_FFFF;
    /// The fast-array flag (bit 28 of the flags word).
    pub(crate) const FAST_ARRAY_MASK: i64 = 0x1000_0000;
    /// The extensible flag (bit 31 of the flags word). The engine's fast write
    /// into a plain array only runs on an extensible one, so the write guard
    /// checks it too.
    pub(crate) const EXTENSIBLE_MASK: i64 = 0x8000_0000;
    /// The offset of the property slots' data pointer.
    pub(crate) const PROP_DATA_OFFSET: i32 = (NODE_TO_OBJECT
        + std::mem::offset_of!(ObjectImpl, prop_view)
        + std::mem::offset_of!(PropView, data)) as i32;
    /// The offset of a shape's property count, from the shape's GC node pointer.
    ///
    /// The property guard's range check reads the slot count here: slots are
    /// index-aligned with the shape's props, and the guard reaches this only
    /// after the shape's identity (pointer + uid) already matched.
    pub(crate) const SHAPE_PROP_COUNT_OFFSET: i32 =
        (gc_lite::gctype::payload_offset_of::<ShapeImpl>()
            + std::mem::offset_of!(ShapeImpl, prop_count)) as i32;
    /// The offset of a shape's payload, from the shape's GC node pointer.
    ///
    /// The inline cache records a shape's *payload* pointer (what
    /// `GcRef::as_ptr` returns), while an object holds the shape's *node*
    /// pointer — so the guard adds this before comparing.
    pub(crate) const SHAPE_PAYLOAD_OFFSET: i32 =
        gc_lite::gctype::payload_offset_of::<ShapeImpl>() as i32;
    /// The offset of a shape's unique id, from the shape's GC node pointer.
    pub(crate) const SHAPE_UID_OFFSET: i32 = (gc_lite::gctype::payload_offset_of::<ShapeImpl>()
        + std::mem::offset_of!(ShapeImpl, uid)) as i32;
    /// The offset of a shape's prototype (a GC node pointer; null when the
    /// shape has no prototype).
    ///
    /// The typed array's `length` lives on the prototype, so the guard walks
    /// the chain from the shape to check that the prototype that owns
    /// `length` is still the one the inline cache recorded.
    pub(crate) const SHAPE_PROTO_OFFSET: i32 = (gc_lite::gctype::payload_offset_of::<ShapeImpl>()
        + std::mem::offset_of!(ShapeImpl, proto))
        as i32;

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::object::ObjectFlags;

        /// The JIT's masks must be the engine's: the generated code reads the
        /// class id and the fast-array flag out of the flags word with them.
        /// Drift would make the guard read the wrong bits — silently.
        #[test]
        fn the_jit_masks_match_the_engine() {
            assert_eq!(CLASS_ID_MASK, i64::from(ObjectFlags::MASK_CLASS_ID));
            assert_eq!(FAST_ARRAY_MASK, i64::from(ObjectFlags::MASK_FAST_ARRAY));
            assert_eq!(EXTENSIBLE_MASK, i64::from(ObjectFlags::MASK_EXTENSIBLE));
        }

        /// The shape layout the property guard reads: the id from the shape's
        /// GC node pointer, the payload pointer the inline cache records, and
        /// the prototype the typed-array `length` guard walks.
        #[test]
        fn the_shape_layout_is_stable() {
            use hash_link::HashList;

            use crate::object::Shape;

            let mut rt = crate::Runtime::create().unwrap();
            let mut ctx = rt
                .create_context(crate::ScopedContext::ECMA_CONTEXT_SETUP)
                .unwrap();
            ctx.with_new_scope(|scope| {
                // A shape with a prototype, so the chain link is a real object.
                let proto = scope.create_base_object().unwrap();
                let shape =
                    Shape::create_internal(scope, Some(proto.clone()), HashList::new()).unwrap();

                // The node pointer: what an object holds in its `shape` field.
                let node = shape.gc_ref().node_ptr().as_ptr() as usize;
                // The payload pointer: what the inline cache records.
                let payload = unsafe { shape.gc_ref().as_ptr().as_ptr() as usize };
                assert_eq!(
                    payload,
                    node + SHAPE_PAYLOAD_OFFSET as usize,
                    "the shape's payload is not where the JIT expects it"
                );

                // The id, read the way the guard reads it.
                let uid =
                    unsafe { *((node as *const u8).add(SHAPE_UID_OFFSET as usize) as *const u32) };
                assert_eq!(
                    uid, shape.uid,
                    "the shape's id is not where the JIT reads it"
                );

                // The prototype, read the way the typed array's `length` guard
                // walks the chain: the shape's prototype is an object (a GC
                // node pointer), and *its* shape is the next link.
                let proto = unsafe {
                    *((node as *const u8).add(SHAPE_PROTO_OFFSET as usize) as *const u64)
                };
                assert_eq!(
                    proto,
                    shape
                        .proto()
                        .map(|p| p.gc_node_ptr().as_ptr() as u64)
                        .unwrap_or(0),
                    "the shape's prototype is not where the JIT reads it"
                );
            });
        }
    }
}

/// The bytecode function's payload layout: where the `Bytecode` handle lives.
///
/// A nested inlined call site guards on the live callee's *bytecode* identity
/// (the same identity `h_inline_guard` compares), so the generated code walks
/// object → payload → bytecode without a helper.
pub(crate) mod function {
    use crate::function::BFuncPayload;

    /// The offset of the `Bytecode` handle inside the payload.
    pub(crate) const BYTECODE_OFFSET: i32 = BFuncPayload::BYTECODE_OFFSET as i32;

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The bytecode handle sits where the nested inlined-call guard reads
        /// it: object → payload → bytecode, without a helper.
        #[test]
        fn the_function_payload_layout_is_stable() {
            let mut rt = crate::Runtime::create().unwrap();
            let mut ctx = rt
                .create_context(crate::ScopedContext::ECMA_CONTEXT_SETUP)
                .unwrap();
            let value = ctx
                .eval(
                    b"(function f(){ return 1; })",
                    &crate::EvalName::Name("jit-layout-fn.js".into()),
                    crate::EvalType::Global,
                    crate::EvalFlags::empty(),
                )
                .unwrap();
            let func = crate::function::BCWrapper::try_from(value).expect("a function object");
            let bytecode = func.payload().bytecode();
            let node = value.as_gc_node().expect("a GC node").as_ptr() as usize;
            let obj = unsafe {
                *((node as *const u8).add(crate::jit::layout::object::PAYLOAD_OFFSET as usize)
                    as *const u64)
            };
            let read = unsafe { *((obj as *const u8).add(BYTECODE_OFFSET as usize) as *const u64) };
            assert_eq!(
                read,
                bytecode.gc_node_ptr().as_ptr() as u64,
                "the bytecode handle is not where the nested guard reads it"
            );
        }
    }
}

/// The global-variable site cache's layout: the owning shape, the shadowing
/// `global_var_obj` shape, the slot and the flags. The generated guard reads
/// one entry per access.
pub(crate) mod global_var {
    use crate::jit::abi::GlobalVarCache;

    /// The size of one site's cache entry.
    pub(crate) const ENTRY_SIZE: i64 = std::mem::size_of::<GlobalVarCache>() as i64;
    /// The offset of the owning object's shape (payload pointer).
    pub(crate) const OWNER_SHAPE_OFFSET: i32 =
        std::mem::offset_of!(GlobalVarCache, owner_shape) as i32;
    /// The offset of `global_var_obj`'s shape (payload pointer).
    pub(crate) const LEXICAL_SHAPE_OFFSET: i32 =
        std::mem::offset_of!(GlobalVarCache, lexical_shape) as i32;
    /// The offset of the owning shape's id.
    pub(crate) const OWNER_UID_OFFSET: i32 = std::mem::offset_of!(GlobalVarCache, owner_uid) as i32;
    /// The offset of `global_var_obj`'s shape id.
    pub(crate) const LEXICAL_UID_OFFSET: i32 =
        std::mem::offset_of!(GlobalVarCache, lexical_uid) as i32;
    /// The offset of the property slot.
    pub(crate) const SLOT_OFFSET: i32 = std::mem::offset_of!(GlobalVarCache, slot) as i32;
    /// The offset of the kind byte (`0` empty, `1` lexical, `2` global object).
    pub(crate) const KIND_OFFSET: i32 = std::mem::offset_of!(GlobalVarCache, kind) as i32;
    /// The offset of the cached property flags.
    pub(crate) const FLAGS_OFFSET: i32 = std::mem::offset_of!(GlobalVarCache, flags) as i32;

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::jit::abi::{GLOBAL_CACHE_GLOBAL, GLOBAL_CACHE_LEXICAL};

        /// The guard loads every field independently: all offsets must fall
        /// inside the entry and must not overlap.
        #[test]
        fn the_global_cache_layout_is_stable() {
            assert_eq!(ENTRY_SIZE, 32);
            assert_eq!(KIND_OFFSET, 28);
            assert_eq!(FLAGS_OFFSET, 29);
            assert!(GLOBAL_CACHE_LEXICAL != 0 && GLOBAL_CACHE_GLOBAL != 0);
            assert_ne!(GLOBAL_CACHE_LEXICAL, GLOBAL_CACHE_GLOBAL);
            let mut offsets = [
                OWNER_SHAPE_OFFSET,
                LEXICAL_SHAPE_OFFSET,
                OWNER_UID_OFFSET,
                LEXICAL_UID_OFFSET,
                SLOT_OFFSET,
                KIND_OFFSET,
                FLAGS_OFFSET,
            ];
            for offset in offsets {
                assert!(
                    offset >= 0 && i64::from(offset) < ENTRY_SIZE,
                    "the guard would read outside the entry"
                );
            }
            offsets.sort_unstable();
            for pair in offsets.windows(2) {
                assert!(pair[0] < pair[1], "two fields share an offset");
            }
        }
    }
}

/// The typed array's layout: the view's buffer, its start offset, its lengths
/// and its out-of-bounds flag.
pub(crate) mod typed_array {
    use crate::builtins::typed_array::TypedArrayPayload;

    /// The offset of the underlying buffer (a GC node pointer).
    pub(crate) const BUFFER_OFFSET: i32 =
        std::mem::offset_of!(TypedArrayPayload, array_buffer) as i32;
    /// The offset of the view's start offset, in bytes.
    pub(crate) const OFFSET_OFFSET: i32 = std::mem::offset_of!(TypedArrayPayload, offset) as i32;
    /// The offset of the view's element count.
    pub(crate) const COUNT_OFFSET: i32 = std::mem::offset_of!(TypedArrayPayload, count) as i32;
    /// The offset of the "out of bounds" flag.
    pub(crate) const OUT_OF_BOUNDS_OFFSET: i32 =
        std::mem::offset_of!(TypedArrayPayload, out_of_bounds) as i32;

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The offsets the typed-array guard reads: buffer, start offset,
        /// element count and out-of-bounds flag. They must stay inside the
        /// payload and must not overlap — the guard loads each one
        /// independently. Their meaning is pinned by the behavioural tests in
        /// `tests/jit/jit_array.rs`.
        #[test]
        fn the_jit_layout_contract_is_stable() {
            let size = std::mem::size_of::<TypedArrayPayload>();
            let mut offsets = [
                BUFFER_OFFSET,
                OFFSET_OFFSET,
                COUNT_OFFSET,
                OUT_OF_BOUNDS_OFFSET,
            ];
            for offset in offsets {
                assert!(
                    offset >= 0 && (offset as usize) < size,
                    "the guard would read outside the payload"
                );
            }
            offsets.sort_unstable();
            for pair in offsets.windows(2) {
                assert!(pair[0] < pair[1], "two fields share an offset");
            }
        }
    }
}

/// The array's layout: where its elements live inside the payload.
pub(crate) mod array {
    use crate::array::{ELEMENTS_DATA_OFFSET, ELEMENTS_LEN_OFFSET, ELEMENTS_VIEW_OFFSET};

    /// The offset of the elements' data pointer, from the payload's start.
    pub(crate) const DATA_OFFSET: i32 = ELEMENTS_VIEW_OFFSET + ELEMENTS_DATA_OFFSET;
    /// The offset of the elements' length, from the payload's start.
    pub(crate) const LEN_OFFSET: i32 = ELEMENTS_VIEW_OFFSET + ELEMENTS_LEN_OFFSET;
}

/// The array buffer's layout: the bytes' view and the detached flag.
pub(crate) mod array_buffer {
    use crate::builtins::array_buffer::{ArrayBufferPayload, BufferView};

    /// The offset of the bytes' data pointer inside the view.
    pub(crate) const VIEW_DATA_OFFSET: i32 = std::mem::offset_of!(BufferView, data) as i32;
    /// The offset of the "directly readable" flag inside the view.
    pub(crate) const VIEW_DIRECT_OFFSET: i32 = std::mem::offset_of!(BufferView, direct) as i32;
    /// The offset of the bytes' view inside the payload.
    pub(crate) const VIEW_OFFSET: i32 = std::mem::offset_of!(ArrayBufferPayload, view) as i32;
    /// The offset of the "detached" flag inside the payload.
    pub(crate) const DETACHED_OFFSET: i32 =
        std::mem::offset_of!(ArrayBufferPayload, detached) as i32;

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The offsets the array-buffer guard reads: the bytes' data pointer,
        /// the "directly readable" flag, and the payload's detached flag (a
        /// detached buffer's bytes are gone).
        #[test]
        fn the_view_layout_is_stable() {
            assert_eq!(VIEW_DATA_OFFSET, 0);
            assert_eq!(VIEW_DIRECT_OFFSET, 12);
            // The detached flag must not share an offset with the view: the
            // guard loads them independently.
            let mut offsets = [VIEW_OFFSET, DETACHED_OFFSET];
            offsets.sort_unstable();
            assert!(
                offsets[0] < offsets[1],
                "the detached flag overlaps the view"
            );
        }
    }
}
