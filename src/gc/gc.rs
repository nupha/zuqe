use std::ptr::NonNull;

use crate::{
    context::JSContext, function::JSVarRef, runtime::JSRuntime, utils::Node, value::JSValueConst,
};

use super::{GCObject, GCObjectType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSGCPhaseEnum {
    JS_GC_PHASE_NONE,
    /// Phase where objects with zero reference count are freed
    JS_GC_PHASE_DECREF,
    /// Phase where cyclic references are detected and freed
    JS_GC_PHASE_REMOVE_CYCLES,
}

pub fn JS_DumpObjectHeader() {
    println!(
        "{:>14} {:4} {:4} {:>14} {:>14} {:10} PROPS",
        "ADDRESS", "REFS", "SHRF", "SHAPE@", "PROTO", "CLASS",
    );
}

fn gcobject_to_node(g: Box<dyn GCObject>) -> Box<Node<Box<dyn GCObject>>> {
    Node::new(g, |data, link| {
        let ptr = link.as_ptr();
        match &mut data.gc_type() {
            GCObjectType::Object(mut o) => unsafe {
                o.as_mut().header.list_node = ptr;
            },
            GCObjectType::FunctionBytecode(mut o) => unsafe {
                o.as_mut().header.list_node = ptr;
            },
            GCObjectType::Shape(mut o) => unsafe {
                o.as_mut().header.list_node = ptr;
            },
            GCObjectType::VarRef(o) => unsafe {
                o.cast::<JSVarRef>().as_mut().header.list_node = ptr;
            },
            GCObjectType::AsyncFunction(mut o) => unsafe {
                o.as_mut().header.list_node = ptr;
            },
            GCObjectType::Context(mut o) => unsafe {
                o.as_mut().header.list_node = ptr;
            },
        }
    })
}

impl JSRuntime {
    /// Adds a GC object to the garbage collector's tracking list
    /// Initializes the object's mark and type, then adds it to the GC object list
    pub fn gc_add_object(&mut self, mut ob: Box<dyn GCObject>) {
        ob.set_gc_mark(0);
        self.gc_obj_list.push(gcobject_to_node(ob));
    }

    /// Frees objects in zeroed gc list
    pub unsafe fn gc_free_zeroed(&mut self) {
        // Set GC phase to decrement reference count
        self.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_DECREF;

        while let Some(mut node) = self.gc_zero_ref_count_list.pop() {
            debug_assert_eq!(node.ref_count(), 0);
            node.finalize(self);
            // node is dropped here
        }

        // Reset GC phase
        self.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;
    }

    /// Decreases reference counts for all child objects in the GC object list.
    /// Moves objects with zero reference count to a temporary list for cleanup.
    pub fn gc_decref(&mut self) {
        debug_assert!(self.tmp_obj_list.is_empty());

        let mut rt = unsafe { NonNull::new_unchecked(self) };

        for node in self.gc_obj_list.iter_mut() {
            debug_assert_eq!(node.gc_mark(), 0);

            node.gc_mark_children(unsafe { rt.as_mut() }, |rt, p| unsafe {
                debug_assert!(!p.is_null());

                #[cfg(debug_assertions)]
                if (*p).gc_mark() == 0 {
                    debug_assert!(
                        (*p).ref_count() > 0,
                        "object in gc list must have ref count greater than 0, if it's not in marking"
                    );
                }
 

                // must do def_ref() first
                if   (*p).dec_ref() == 0 && (*p).gc_mark() == 1  {
                    let n = (*p).gc_node_ptr();
                    Node::unlink(n);
                    (*rt).tmp_obj_list.push_raw(n);
                }else{
                    // node is dropped
                }
            });

            node.set_gc_mark(1);

            if node.ref_count() == 0 {
                unsafe {
                    let n = NonNull::from(node);
                    Node::unlink(n);
                    self.tmp_obj_list.push_raw(n);
                }
            }
        }
    }

    /// Executes garbage collection process once
    pub fn gc(&mut self) {
        // decrement the reference of the children of each object. mark = 1 after this pass.
        self.gc_decref();
        // keep the GC objects with a non zero refcount and their childs
        self.gc_scan();
        // free the GC objects in a cycle
        self.gc_free_cycles();
    }

    pub fn gc_dump_objects(&mut self, which: u8) {
        let list = match which {
            0 => {
                println!("Zeroed GCObjects: {{");
                &self.gc_zero_ref_count_list
            }
            1 => {
                println!("GCObjects: {{");
                &self.gc_obj_list
            }
            2 => {
                println!("Temp GCObjects: {{");
                &self.tmp_obj_list
            }
            _ => unreachable!(),
        };

        JS_DumpObjectHeader();

        for node in list.iter() {
            node.gc_dump(self);
        }

        println!("}}");
    }
}

pub fn js_trigger_gc(rt: &mut JSRuntime, size: usize) {
    let force_gc: bool;

    #[cfg(feature = "FORCE_GC_AT_MALLOC")]
    {
        force_gc = true;
    }
    #[cfg(not(feature = "FORCE_GC_AT_MALLOC"))]
    {
        force_gc = ((*rt).malloc_state.malloc_size + size) > (*rt).malloc_gc_threshold;
    }

    if force_gc {
        #[cfg(feature = "DUMP_GC")]
        {
            println!("GC: size={}", (*rt).malloc_state.malloc_size);
        }

        rt.gc();
        rt.malloc_gc_threshold = rt.malloc_state.malloc_size + (rt.malloc_state.malloc_size >> 1);
    }
}

/// Frees a GC object based on its type
/// Handles different types of GC objects with their specific cleanup logic.
///
/// returns:
/// - True if the pointer was freed
/// - False if the object is not freed, its status changed to Zeroed
// pub(super) unsafe fn gc_free(rt: *mut JSRuntime, mut gp: Box<dyn GCObject>) -> bool {
//     gp.before_drop_clean_up(rt.as_mut().unwrap());

//     if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES && gp.as_ref().ref_count() != 0 {
//         // we are in free cycles process,
//         // where Temp status object does not always have ref_count==0, it could be 1 sometimes.
//         // in this case, we put this object in Zeroed state to let it be freed in later
//         // gc_free_cycles() call.
//         gp.as_mut().gc_set_status(GCStatus::Zeroed);
//         false
//     } else {
//         {
//             print!(
//                 "[O.o] gc state={:?} ref_count={}",
//                 gp.as_ref().gc_status(),
//                 gp.as_ref().ref_count()
//             );
//             gp.as_ref().gc_dump(rt.as_ref().unwrap());
//         }

//         debug_assert_eq!(gp.as_ref().ref_count(), 0);
//         gp.as_mut().before_drop_clean_up(rt.as_mut().unwrap());

//         drop(Box::from_raw(gp.as_ptr()));
//         true
//     }
// }

/* Marks an object as potentially part of a function prototype cycle */
pub unsafe fn set_cycle_flag(ctx: *mut JSContext, obj: JSValueConst) {
    // to nothing
}
