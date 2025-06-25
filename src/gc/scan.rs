use std::ptr::NonNull;

use crate::{runtime::JSRuntime, utils::Node, value::object::GCObject};

use super::JSGCPhaseEnum;

impl JSRuntime {
    /// Preserves objects with positive reference counts and their child objects.
    /// Identifies objects with zero reference count in the GC object list
    /// and moves them to a temporary list for removal.
    pub fn gc_scan(&mut self) {
        print!(
            "[O.o][gc_scan] gc_obj_list={}, tmp_obj_list={} ",
            self.gc_obj_list.len(),
            self.tmp_obj_list.len(),
        );

        // let mut rt = unsafe { NonNull::new_unchecked(self as *mut JSRuntime) };

        // for w in self.gc_obj_list.iter_mut() {
        //     if let Some(gco) = w.upgrade() {
        //         // reset the mark for the next GC call
        //         gco.set_gc_mark(0);

        //         unsafe {
        //             gco.gc_mark_children(self, |rt: *mut JSRuntime, p: &GCObject| {
        //                 if p.inc_ref() == 1 {
        //                     // ref_count was 0: `p` is now in tmp_obj_list, restore it to gc_obj_list

        //                     let node1 = (*p).gc_node_ptr();
        //                     Node::unlink(node1);
        //                     (*rt).gc_obj_list.push_raw(node1);

        //                     // reset the mark for the next GC call
        //                     (*p).set_gc_mark(0);
        //                 }
        //             });
        //         }
        //     }
        // }

        // // restore the refcount of the objects to be deleted.
        // for node in self.tmp_obj_list.iter_mut() {
        //     unsafe {
        //         node.gc_mark_children(rt.as_mut(), |_, p: *mut dyn GCObject| {
        //             (*p).inc_ref();
        //         });
        //     }
        // }

        println!("[O.o] TODO gc scan");

        println!(
            " -> gc_obj_list={}, tmp_obj_list={} [/O.o]",
            self.gc_obj_list.len(),
            self.tmp_obj_list.len(),
        );
    }

    pub fn gc_free_cycles(&mut self) {
        println!("[O.o] TODO gc free cycles");

        #[cfg(feature = "dump_gc_free")]
        let mut header_done = false;

        // self.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES;

        // while let Some(mut w) = self.tmp_obj_list.pop() {
        //     if let Some(node) = w.upgrade() {
        //         // Only need to free the GC object associated with JS values
        //         // or async functions. The rest will be automatically removed
        //         // because they must be referenced by them.
        //         match node {
        //             GCObject::Object(_)
        //             | GCObject::FunctionBytecode(_)
        //             | GCObject::AsyncFunction(_)
        //             | GCObject::Context(_) => {
        //                 #[cfg(feature = "dump_gc_free")]
        //                 {
        //                     if !header_done {
        //                         println!("Freeing cycles:");
        //                         JS_DumpObjectHeader();
        //                         header_done = true;
        //                     }
        //                     p.as_ref().gc_dump(self);
        //                 }

        //                 node.finalize(self);

        //                 if node.ref_count() != 0 {
        //                     // this gcobject is about to be freed cascading.
        //                     // temporarily add it to zero list,
        //                     // it won't live long till zeroed list purge step.
        //                     self.gc_zero_ref_count_list.push(node);
        //                 } else {
        //                     drop(node);
        //                 }
        //             }
        //             GCObject::Shape(_) => {
        //                 // these objects are weak ref: to be dropped cascading
        //                 self.gc_zero_ref_count_list.push(node);
        //             }
        //             GCObject::VarRef(_) => {
        //                 // these objects are weak ref: to be dropped cascading
        //                 self.gc_zero_ref_count_list.push(node);
        //             }
        //         }
        //     }
        // }

        // self.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;

        // // O.o print gcobject in variant chains
        // {
        //     println!("[O.o]----------- free cycles purge");
        //     self.gc_dump_objects(1);
        //     self.gc_dump_objects(2);
        //     self.gc_dump_objects(0);
        // }

        // // purge gz_zero_ref_count_list to drop gcobjects
        // while let Some(node) = self.gc_zero_ref_count_list.pop() {
        //     debug_assert_eq!(node.ref_count(), 0);

        //     match node.gc_type() {
        //         GCObjectType::Object(_)
        //         | GCObjectType::FunctionBytecode(_)
        //         | GCObjectType::AsyncFunction(_)
        //         | GCObjectType::Context(_) => {}

        //         GCObjectType::Shape(_) | GCObjectType::VarRef(_) => {
        //             println!("[O.o] ------------------");
        //             node.gc_dump(self);
        //             unreachable!(
        //                 "[O.o] above object can't live long in gc zero list up to pruge zero gc list stage"
        //             );
        //         }
        //     }

        //     drop(node);
        // }
    }
}
