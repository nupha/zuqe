use crate::{context::JSContext, object::GCObject, runtime::JSRuntime};

use super::JSValue;

#[deprecated(note = "unused")]
pub fn JS_FreeValue(ctx: &JSContext, v: &JSValue) {
    JS_FreeValueRT(ctx.rt_mut(), v)
}

#[deprecated(note = "unused")]
pub fn JS_FreeValueRT(rt: *mut JSRuntime, v: &JSValue) {
    // if let Some(p) = v.payload() {
    //     if p.strong_count() == 1 {
    //         unsafe {
    //             __JS_FreeValueRT(rt, p);
    //         }
    //     }
    // }

    // if JS_VALUE_HAS_REF_COUNT(v) {
    //     let mut gcobj = match v.tag() {
    //         JS_TAG_OBJECT => v.object_unchecked().gc_node_ptr(),
    //         JS_TAG_FUNCTION_BYTECODE => {
    //             (*JS_VALUE_GET_PTR(v).cast::<JSFunctionBytecode>()).gc_node_ptr()
    //         }
    //         JS_TAG_STRING | JS_TAG_SYMBOL => {
    //             let s = Rc::from_raw(v.string());
    //             if Rc::strong_count(&s) == 1 {
    //                 // rc is about to drop, drop atom description too
    //                 if let Some(at) = s.atom_info() {
    //                     JS_FreeAtomStruct(rt, at.atom());
    //                 }
    //             }
    //             return;
    //         }
    //         _ => return,
    //     };

    //     debug_assert!(gcobj.as_ref().ref_count() > 0);
    //     if gcobj.as_mut().dec_ref() == 0 {
    //         __JS_FreeValueRT(rt, v);
    //     }
    // }
}

/* called with the ref_count of 'v' reaches zero. */
#[deprecated(note = "unused")]
pub unsafe fn __JS_FreeValueRT(rt: *mut JSRuntime, payload: &GCObject) {
    //debug_assert_eq!(payload.strong_count(), 1);
    //payload.finalize(rt.as_mut().unwrap());

    // let tag = v.tag();

    // #[cfg(feature = "dump_free")]
    // {
    //     print!("Freeing ({tag:2}) ");
    //     if tag == JS_TAG_OBJECT {
    //         v.object_unchecked().dump(rt.as_ref().unwrap());
    //     } else {
    //         print!("{}", v.repr(rt.as_ref().unwrap()));
    //         println!();
    //     }
    // }

    // match tag {
    //     JS_TAG_OBJECT => {
    //         if (*rt).gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
    //             let node = v.object_unchecked().gc_node_ptr();
    //             debug_assert_eq!(node.as_ref().ref_count(), 0);

    //             Node::unlink(node);
    //             (*rt).gc_zero_ref_count_list.push_raw(node);

    //             if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
    //                 (*rt).gc_free_zeroed();
    //             }
    //         }
    //     }
    //     JS_TAG_FUNCTION_BYTECODE => {
    //         if (*rt).gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
    //             let node = (*JS_VALUE_GET_PTR(v).cast::<JSFunctionBytecode>()).gc_node_ptr();
    //             debug_assert_eq!(node.as_ref().ref_count(), 0);

    //             Node::unlink(node);
    //             (*rt).gc_zero_ref_count_list.push_raw(node);

    //             if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
    //                 (*rt).gc_free_zeroed();
    //             }
    //         }
    //     }
    //     JS_TAG_STRING | JS_TAG_SYMBOL => {
    //         unreachable!("never freed here");
    //     }
    //     JS_TAG_MODULE => {
    //         unreachable!("never freed here");
    //     }
    //     JS_TAG_BIG_INT | JS_TAG_BIG_FLOAT | JS_TAG_BIG_DECIMAL => {
    //         unimplemented!()
    //     }
    //     _ => {
    //         panic!("__JS_FreeValue: unknown tag={}", tag);
    //     }
    // }
}
