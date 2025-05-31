use std::ops::Deref;

use crate::{
    function::JSVarRef,
    gc::{GCObject, GCObjectType},
    runtime::JSRuntime,
    typed_array::typed_array_size_log2,
    value::{
        object::{JS_PROP_AUTOINIT, JS_PROP_GETSET, JS_PROP_VARREF},
        JSTag,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{js_autoinit_get_id, js_autoinit_get_realm, JSObject, JSShape, JS_PROP_TMASK};

impl JSShape {
    /// print shape debug info to console
    pub unsafe fn dump(&self, rt: &JSRuntime, i: i32) {
        // XXX: should output readable class prototype

        print!(
            "{:5} {:3}{} {:2} {:14p} {:14p} {:5} {:5}",
            i,
            self.ref_count(),
            if self.is_hashed { '*' } else { ' ' }, // hashed?
            if self.header.link.next.is_null() && self.header.link.prev.is_null() {
                " " // not in gc list
            } else {
                "+" // in gc list
            },
            self,
            self.proto,
            self.props.len(),
            self.props.len(),
        );

        for key in self.props.keys() {
            print!(" {}", key.to_str(rt));
        }

        println!();
    }
}

pub unsafe fn JS_DumpShapes(rt: *mut JSRuntime) {
    println!("JSShapes: {{");
    println!(
        "{:5} {:4} {:2} {:14} {:14} {:5} {:5} PROPS",
        "SLOT", "REFS", "GC", "ADDR", "PROTO", "SIZE", "COUNT"
    );

    // dump hashed shapes
    for (key, lst) in (*rt).shape_registry.iter() {
        for sh in lst {
            sh.dump(rt.as_ref().unwrap(), *key as _);
            debug_assert!(sh.is_hashed);
        }
    }

    // dump non-hashed shapes
    for node in (*rt).gc_obj_list.iter() {
        if let GCObjectType::Object(p) = node.gc_type() {
            let sh = p.as_ref().shape().unwrap();
            if !sh.is_hashed {
                sh.dump(rt.as_ref().unwrap(), -1);
            }
        }
    }

    println!("}}");
}

#[deprecated(note = "use JSShape::dump() instead")]
pub unsafe fn JS_DumpShape(rt: *const JSRuntime, i: i32, sh: *const JSShape) {
    (*sh).dump(rt.as_ref().unwrap(), i);
}

impl JSObject {
    /// print object debug info to console
    pub fn dump(&self, rt: &JSRuntime) {
        // XXX: should encode atoms with special characters
        print!("{:14p} {:4} ", self, self.ref_count());

        // NOTE: the shape can be NULL while freeing an object
        if let Some(sh) = self.shape() {
            print!(
                "{:3}{} {:14p} {:14p} ",
                sh.ref_count(),
                if sh.is_hashed { "*" } else { " " },
                sh,
                sh.proto
            );
        } else {
            print!("{:>3}  {:>14} {:>14} ", "-", "-", "-");
        }

        // print class name
        print!(
            "{:10} ",
            rt.class_array[self.class_id() as usize]
                .class_name
                .to_str(rt),
        );

        if self.detailed.is_exotic() && self.detailed.fast_array() {
            print!("[ ");

            match self.class_id() {
                JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                    let lst = unsafe { self.fast_array_values_unchecked() };
                    for (i, v) in lst.iter().enumerate() {
                        if i != 0 {
                            print!(", ");
                        }
                        v.repr(rt);
                    }
                }
                class_id @ (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY) => {
                    // 1, 2, 4, 8
                    let bytes_per_element = 1 << typed_array_size_log2(class_id);

                    self.isa().typed_array().unwrap().with_bytes(|lst| {
                        for (i, b) in lst.iter().enumerate() {
                            if i != 0 && i % bytes_per_element == 0 {
                                print!(", ");
                            }
                            print!("{:02X}", *b);
                        }
                    });
                }
                class_id => unimplemented!("{}", class_id),
            }

            print!(" ] ");
        }

        // print properties
        if let Some(sh) = self.shape() {
            print!("{{ ");

            let mut is_first: bool = true;
            for (idx, (atom, prs)) in sh.props.iter().enumerate() {
                if !is_first {
                    print!(", ");
                };

                // key
                print!("{}:", atom.to_str(rt));

                // value
                if let Some(pr) = self.prop_values.get(idx) {
                    match prs.flags() & JS_PROP_TMASK {
                        JS_PROP_GETSET => unsafe {
                            print!("[getset {:p} {:p}]", pr.u.getset.getter, pr.u.getset.setter);
                        },
                        JS_PROP_VARREF => unsafe {
                            print!("[varref {:p}]", pr.u.var_ref.deref());
                        },
                        JS_PROP_AUTOINIT => unsafe {
                            print!(
                                "[autoinit {:p} {} {:p}]",
                                js_autoinit_get_realm(pr),
                                js_autoinit_get_id(pr),
                                pr.u.init.opaque
                            );
                        },
                        _ => unsafe {
                            print!("{}", pr.u.value.repr(rt));
                        },
                    }
                } else {
                    // prop value slots is cleared
                    print!("<?>");
                }

                is_first = false;
            }
            print!(" }}");
        }

        if let Some(func) = self.isa().func() {
            let b = func.function_bytecode;
            if unsafe { (*b).closure_var_count } != 0 {
                print!(" Closure:");
                for vr in func.var_refs.iter() {
                    print!(" {}", unsafe {
                        (*vr.cast::<JSVarRef>().as_ref().pvalue).repr(rt)
                    });
                }

                if !func.home_object.is_null() {
                    print!(
                        " HomeObject: {}",
                        JS_MKPTR!(JSTag::Object, func.home_object.cast()).repr(rt)
                    );
                }
            }
        }

        println!();
    }
}
