use std::ops::DerefMut;

use crate::iterator::ObjectIter;
use crate::object::JSObject;
use crate::{
    JS_CLASS_OBJECT,
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, consts::*},
    class::JS_DEFINE_CLASS_HAS_HERITAGE,
    context::JSContext,
    function::{JSFunctionKindEnum, JSVarRef, NativeFunctionType, ObjFunc},
    iterator::{
        JS_IteratorGetCompleteValue, JSIteratorKindEnum, build_for_in_iterator,
        js_array_iterator_next, js_create_array_iterator,
    },
    object::{
        JS_DefineObjectName, JS_DefineObjectNameComputed, JS_GPN_SET_ENUM, JS_GPN_STRING_MASK,
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
        JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_WRITABLE, JS_SetConstructorBit, ObjectDetail,
        js_get_length32,
    },
    stack_frame::StackFrame,
    string::JS_ConcatString3,
    symbol::JS_AtomSymbolHasDescription,
    value::{JSValue, JSValueConst},
};

pub fn js_operator_delete(ctx: &JSContext, sp: *mut JSValue) -> i32 {
    let (op1, op2) = unsafe {
        (
            std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED),
            std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED),
        )
    };

    let atom = op2.coerce_to_atom(ctx).unwrap_or(JS_ATOM_NULL);
    if atom == JS_ATOM_NULL {
        return -1;
    }

    let ret = op1.delete_property(ctx, atom, JS_PROP_THROW_STRICT);
    JS_FreeAtom(ctx, atom);
    if let Ok(b) = ret {
        unsafe {
            std::ptr::replace(sp.offset(-2), JSValue::Bool(b));
        }
        0
    } else {
        -1
    }
}

pub fn js_operator_instanceof(ctx: &JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let (obj, ctor) = unsafe {
        (
            std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED),
            std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED),
        )
    };

    let b = obj.instance_of(ctx, &ctor)?;
    unsafe {
        std::ptr::replace(sp.offset(-2), JSValue::Bool(b));
    }

    Ok(())
}

pub fn js_operator_in(ctx: &JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let prop = unsafe { std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED) };
    let obj = unsafe { std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED) };

    if !obj.is_object() {
        ctx.throw_type_error("invalid 'in' operand");
        return Err(-1);
    }

    let atom = prop.coerce_to_atom(ctx)?;
    let ret = obj.has_property(ctx, atom);
    JS_FreeAtom(ctx, atom);
    let b = ret?;
    unsafe {
        std::ptr::replace(sp.offset(-2), JSValue::Bool(b));
    }
    Ok(())
}

pub fn js_operator_private_in(ctx: &JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let obj = unsafe { std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED) }; // object
    let item = unsafe { std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED) }; // field name or method function

    if let JSValue::Object(o) = obj {
        let ret = if item.is_object() {
            // method: use the brand
            o.check_brand(ctx, &item)?
        } else {
            // field
            let atom = item.coerce_to_atom(ctx)?;
            let b = o.has_shape_property(atom);
            JS_FreeAtom(ctx, atom);
            b
        };
        unsafe {
            std::ptr::replace(sp.offset(-2), JSValue::Bool(ret));
        }
        Ok(())
    } else {
        ctx.throw_type_error("invalid 'in' operand");
        Err(-1)
    }
}

pub fn js_op_define_class(
    ctx: &JSContext,
    sp: *mut JSValue,
    class_name: JSAtom,
    class_flags: i32,
    cur_var_refs: &[JSVarRef],
    sframe: &mut StackFrame,
    is_computed_name: bool,
) -> Result<(), i32> {
    // SP[-1] is bytecode
    let bc_val = unsafe { std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED) };
    let bc = match bc_val {
        JSValue::Bytecode(b) => b,
        JSValue::Object(o) => {
            todo!("[O.o] could this be an function object?");
            // o.0.detail
            //     .borrow()
            //     .func()
            //     .and_then(|f| f.bytecode.clone())
            //     .unwrap();
        }
        _ => unreachable!(),
    };
    debug_assert_eq!(bc.0.borrow().func_kind, JSFunctionKindEnum::JS_FUNC_NORMAL);

    // SP[-2]: parent class
    let mut parent_class = unsafe { std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED) };
    let parent_proto = if class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0 {
        if parent_class.is_null() {
            parent_class = ctx.0.borrow().function_proto.clone();
            JSValue::NULL
        } else {
            if !parent_class.is_constructor() {
                ctx.throw_type_error("parent class must be constructor ");
                return Err(-1);
            }

            let p = parent_class.get_property(ctx, JS_ATOM_prototype)?;

            if !matches!(p, JSValue::NULL | JSValue::Object(_)) {
                ctx.throw_type_error("parent prototype must be an object or null");
                return Err(-1);
            }
            p
        }
    } else {
        // parent_class is undefined
        parent_class = ctx.0.borrow().function_proto.clone();
        ctx.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone()
    };

    let proto = ctx.create_base_object_from_proto(parent_proto)?;

    // make constructor function
    let func = ctx
        .create_object_from_proto(
            parent_class,
            ObjectDetail::BYTECODE_FUNCTION(ObjFunc::default()),
        )
        .map_err(|_| -1)?;

    let ctor = JSValue::Object(bc.make_closure_to(ctx, func, cur_var_refs, Some(sframe))?);

    js_method_set_home_object(&ctor, Some(&proto));
    JS_SetConstructorBit(ctx, &ctor, true);

    ctor.define_property_value(
        ctx,
        JS_ATOM_length,
        JSValue::Int(bc.0.borrow().defined_arg_count as i32),
        JS_PROP_CONFIGURABLE,
    )
    .unwrap();

    if is_computed_name {
        if unsafe { JS_DefineObjectNameComputed(ctx, &ctor, &*sp.offset(-3), JS_PROP_CONFIGURABLE) }
            < 0
        {
            return Err(-1);
        }
    } else {
        if unsafe { JS_DefineObjectName(ctx, &ctor, class_name, JS_PROP_CONFIGURABLE) } < 0 {
            return Err(-1);
        }
    }

    // the constructor property must be first. It can be overridden by
    // computed property names
    proto.define_property_value(
        ctx,
        JS_ATOM_constructor,
        ctor.clone(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE | JS_PROP_THROW,
    )?;

    // set the prototype property
    ctor.define_property_value(ctx, JS_ATOM_prototype, proto.clone().into(), JS_PROP_THROW)?;

    // TODO: mark cyclic reference
    // set_cycle_flag(ctx, &ctor);
    // set_cycle_flag(ctx, &proto);

    unsafe {
        std::ptr::replace(sp.offset(-2), ctor);
        std::ptr::replace(sp.offset(-1), proto.into());
    }

    Ok(())
}

pub fn js_has_unscopable(ctx: &JSContext, obj: &JSValueConst, atom: JSAtom) -> i32 {
    let arr = if let Ok(x) = obj.get_property(ctx, JS_ATOM_Symbol_unscopables) {
        x
    } else {
        return -1;
    };

    if arr.is_object() {
        if let Ok(val) = arr.get_property(ctx, atom) {
            val.coerce_to_bool(ctx).map_or(-1, |b| b as i32)
        } else {
            -1
        }
    } else {
        0
    }
}

pub fn js_get_function_name(ctx: &JSContext, name: JSAtom) -> JSValue {
    let mut name_str = name.to_value(ctx, true);
    if JS_AtomSymbolHasDescription(ctx, name) {
        name_str = unsafe { JS_ConcatString3(ctx, c"[", name_str, c"]") };
    }
    name_str
}

pub fn js_method_set_home_object(func_obj: &JSValue, home_obj: Option<&JSObject>) {
    if let JSValue::Object(o) = func_obj {
        if let Some(fo) = o.0.payload.borrow_mut().func_mut() {
            let need_home_object = fo
                .bytecode
                .as_ref()
                .map_or(false, |b| b.0.borrow().need_home_object);

            if need_home_object {
                fo.home_object = home_obj.cloned();
            }
        }
    }
}

// Modify the name of a method according to the atom and
// 'flags'. 'flags' is a bitmask of JS_PROP_HAS_GET and
// JS_PROP_HAS_SET. Also set the home object of the method.
// Return < 0 if exception.
pub fn js_method_set_properties(
    ctx: &JSContext,
    func_obj: &JSValue,
    name: JSAtom,
    flags: u32,
    home_obj: &JSValue,
) -> i32 {
    let mut name_str = js_get_function_name(ctx, name);
    if flags & JS_PROP_HAS_GET != 0 {
        name_str = unsafe { JS_ConcatString3(ctx, c"get ", name_str, c"") };
    } else if flags & JS_PROP_HAS_SET != 0 {
        name_str = unsafe { JS_ConcatString3(ctx, c"set ", name_str, c"") };
    }

    if name_str.is_exception() {
        return -1;
    }
    if func_obj
        .define_property_value(ctx, JS_ATOM_name, name_str, JS_PROP_CONFIGURABLE)
        .is_err()
    {
        return -1;
    }

    js_method_set_home_object(func_obj, home_obj.object());
    0
}

/// 将可迭代对象`SP[-1]`中的所有元素，追加到目标数组`SP[-3]`从`SP[-2]`起始的位置。
/// 类似于 JavaScript 中的 `[...iterable]` 或者 `array.push(...iterable)` 操作。
///
/// 操作完成后, `SP[-3]`数组中加入了来自`SP[-1]`的全部元素，`SP[-1]`没有变化，`SP[-2]`更新为结束位置index。
pub unsafe fn js_append_enumerate(ctx: &JSContext, sp: *mut JSValue) -> Result<(), i32> {
    // the index where append starts
    let mut pos = if let JSValue::Int(i) = &*sp.offset(-2) {
        *i
    } else {
        ctx.throw_internal_error("invalid index for append");
        return Err(-1);
    };

    let src = &*sp.offset(-1); // the source iterator or array
    let dest = &*sp.offset(-3); // the dest array

    // .length property of the array
    let len = js_get_length32(ctx, &src)? as usize;

    // XXX: further optimisations:
    //       - use ctx->array_proto_values?
    //       - check if array_iterator_prototype next method is built-in and
    //         avoid constructing actual iterator object?
    //       - build this into js_for_of_start and use in all `for (x of o)` loops
    let is_array_iterator = src
        .get_property(ctx, JS_ATOM_Symbol_iterator)?
        .match_native_callback(
            NativeFunctionType::Func(js_create_array_iterator),
            JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as _,
        );

    let it = src.iter(ctx)?;

    let try_slow_array = if is_array_iterator
        && it
            .next_method
            .match_native_callback(NativeFunctionType::IteratorNext(js_array_iterator_next), 0)
    {
        // fast path: fast array
        src.with_fast_array_values(|lst| -> Result<bool, i32> {
            if lst.len() == len {
                // `src` is a fast array
                for v in lst {
                    dest.define_property_value_u32(ctx, pos as _, v.clone(), JS_PROP_C_W_E)?;
                    pos += 1;
                }
                Ok(false) // fast path applied, don't try slow path
            } else {
                Ok(true) // try slow path
            }
        })
        .unwrap_or(Ok(true))?
    } else {
        true
    };

    if try_slow_array {
        // general case: slow array, use iterator
        for value in it {
            dest.define_property_value_u32(ctx, pos as _, value?, JS_PROP_C_W_E)?;
            pos += 1;
        }
    }

    // update end index to SP[-2]
    unsafe {
        std::ptr::replace(sp.offset(-2), JSValue::Int(pos));
    }

    Ok(())
}

/// obj -> enum_obj
pub fn js_for_in_start(ctx: &JSContext, sp: *mut JSValue) -> Result<(), i32> {
    unsafe {
        std::ptr::replace(
            sp.offset(-1),
            build_for_in_iterator(ctx, (*sp.offset(-1)).clone()),
        );

        if (*sp.offset(-1)).is_exception() {
            Err(-1)
        } else {
            Ok(())
        }
    }
}

/// 实现 JavaScript 中 for...in 循环的迭代器逻辑，用于遍历对象的可枚举属性
/// enum_obj -> enum_obj value done
pub fn js_for_in_next(ctx: &JSContext, sp: *mut JSValue) -> i32 {
    let enum_obj = unsafe { &*sp.offset(-1) };
    debug_assert!(enum_obj.is_object());

    let ob = unsafe { enum_obj.object_unchecked() };

    if let ObjectDetail::FOR_IN_ITERATOR(it) = ob.0.payload.borrow_mut().deref_mut() {
        let mut prop: JSAtom;

        loop {
            if it.idx >= it.prop_count {
                if it.obj.is_undefined_or_null() {
                    // object has no parent prototype
                    unsafe {
                        std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                        std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                    }
                    return 0;
                }

                if !it.in_prototype_chain {
                    if let Ok(done) = it.prepare_prototype_chain_enum(ctx) {
                        for name in it.prop_enums.iter() {
                            if enum_obj
                                .define_property_value(
                                    ctx,
                                    name.atom,
                                    JSValue::NULL,
                                    JS_PROP_ENUMERABLE,
                                )
                                .is_err()
                            {
                                return -1;
                            }
                        }

                        if done {
                            unsafe {
                                std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                                std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                            }
                            return 0;
                        } else {
                            it.in_prototype_chain = true;
                        }
                    } else {
                        return -1;
                    }
                }

                // walk up prototype chain.
                it.obj = if let Ok(up) = it.obj.prototype(ctx) {
                    up
                } else {
                    return -1;
                };

                // end of prototype chain.
                if it.obj.is_null() {
                    unsafe {
                        std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                        std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                    }
                    return 0;
                }

                // must check for timeout to avoid infinite loop
                if ctx.poll_interrupts().is_err() {
                    return -1;
                }

                if let Ok(names) = unsafe {
                    it.obj
                        .object_unchecked()
                        .query_own_property_names(ctx, JS_GPN_STRING_MASK | JS_GPN_SET_ENUM)
                } {
                    it.prop_count = names.len() as _;
                    it.prop_enums = names;
                    it.idx = 0;
                } else {
                    return -1;
                }
            } else {
                // enumerate on props_enum of current it.obj.
                if it.is_array {
                    prop = JSAtom::new_u32(it.idx);
                    it.idx += 1;
                } else {
                    prop = it.prop_enums[it.idx as usize].atom;
                    let is_enumerable = it.prop_enums[it.idx as usize].is_enumerable;

                    it.idx += 1;
                    if it.in_prototype_chain {
                        // slow case: we are in the prototype chain
                        match unsafe { enum_obj.object_unchecked().get_own_property(ctx, prop) } {
                            Ok(Some(_)) => {
                                continue; // already visited
                            }
                            Ok(None) => {
                                // add to the visited property list
                                if enum_obj
                                    .define_property_value(
                                        ctx,
                                        prop,
                                        JSValue::NULL,
                                        JS_PROP_ENUMERABLE,
                                    )
                                    .is_err()
                                {
                                    return -1;
                                }
                            }
                            Err(_) => {
                                return -1;
                            }
                        }
                    }
                    if !is_enumerable {
                        continue;
                    }
                }

                match it.obj.get_own_property(ctx, prop) {
                    Ok(Some(_)) => {
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        return -1;
                    }
                }
            }
        }

        // return the property
        unsafe {
            std::ptr::replace(sp.offset(0), prop.to_value(ctx, false)); // 属性名
            std::ptr::replace(sp.offset(1), JSValue::Bool(false)); // 未完成标志
        }
    } else {
        unsafe {
            std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
            std::ptr::replace(sp.offset(1), JSValue::Bool(true));
        }
    }

    0
}

/// Get enumerate object and next method of sp[-1] object.
/// Note: new values are updated to sp[-1], sp[0], old value in sp[-1] is consumed.
/// if fail, sp[-1] is not consumed.
/// obj -> enum_rec (3 slots)
pub fn js_for_of_start(ctx: &JSContext, sp: *mut JSValue, is_async: bool) -> Result<(), i32> {
    let iter_obj = unsafe { &*sp.offset(-1) }.iterator_obj(ctx, is_async)?;
    let next = iter_obj.get_property(ctx, JS_ATOM_next)?;
    unsafe {
        std::ptr::replace(sp.offset(-1), iter_obj.into());
        std::ptr::replace(sp.offset(0), next);
    }

    Ok(())
}

pub unsafe fn js_for_of_next(ctx: &JSContext, sp: *mut JSValue, base: i32) -> Result<(), i32> {
    let mut value: JSValue = JSValue::UNDEFINED;
    let mut done = true;

    if let JSValue::Object(iter_obj) = &*sp.offset(base as isize) {
        if let Ok((v, d)) =
            ObjectIter::do_next_with_method(ctx, iter_obj, &*sp.offset((base + 1) as isize), &[])
        {
            done = d;
            value = if done {
                // replace the iteration object with undefined
                std::ptr::replace(sp.offset(base as isize), JSValue::UNDEFINED);
                JSValue::UNDEFINED
            } else {
                v
            };
        } else {
            // replace the iteration object with undefined
            std::ptr::replace(sp.offset(base as isize), JSValue::UNDEFINED);
            return Err(-1);
        }
    }

    std::ptr::replace(sp.offset(0), value);
    std::ptr::replace(sp.offset(1), JSValue::Bool(done));
    Ok(())
}

pub unsafe fn js_iterator_get_value_done(ctx: &JSContext, sp: *mut JSValue) -> i32 {
    let obj = &*sp.offset(-1);
    if !obj.is_object() {
        ctx.throw_type_error("iterator must return an object");
        return -1;
    }

    let mut done = false;
    let value = JS_IteratorGetCompleteValue(ctx, obj, &mut done);
    if value.is_exception() {
        return -1;
    }

    unsafe {
        std::ptr::replace(sp.offset(-1), value);
        std::ptr::replace(sp.offset(0), JSValue::Bool(done));
    }

    0
}
