use crate::{
    JS_CLASS_OBJECT,
    atom::{Atom, JSAtom, consts::*},
    branch_hints::unlikely,
    class::JS_DEFINE_CLASS_HAS_HERITAGE,
    context::JSContext,
    function::{JSFunctionKindEnum, JSVarRef, NativeFunctionType},
    iterator::{
        ForInIterPayload, JS_IteratorGetCompleteValue, JSIteratorKindEnum, ObjectIter,
        js_array_iterator_next, js_create_array_iterator,
    },
    object::{
        JS_GPN_INDEX_MASK, JS_GPN_SET_ENUM, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE,
        JS_PROP_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW, JS_PROP_THROW_STRICT,
        JS_PROP_WRITABLE, JS_SetConstructorBit, JSObject, ObjectPayload, js_get_length32,
    },
    stack_frame::StackFrame,
    string::{JS_ConcatString3, JSString},
    value::JSValue,
};

pub fn js_operator_delete(ctx: &JSContext, sp: *mut JSValue) -> i32 {
    let (op1, op2) = unsafe {
        (
            std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED),
            std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED),
        )
    };

    let atom = op2.to_atom(ctx).unwrap_or(Atom::NULL);
    if atom.is_null() {
        return -1;
    }

    let ret = op1.delete_property(ctx, atom.as_ref(), JS_PROP_THROW_STRICT);
    atom.free(ctx.rt_mut());

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
    let (val, ctor) = unsafe {
        (
            std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED),
            std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED),
        )
    };

    let b = val.instance_of(ctx, &ctor)?;
    unsafe {
        std::ptr::replace(sp.offset(-2), JSValue::Bool(b));
    }

    Ok(())
}

pub fn js_operator_in(ctx: &JSContext, obj: &JSValue, prop: &JSValue) -> Result<bool, i32> {
    if unlikely(!obj.is_object()) {
        ctx.throw_type_error("invalid 'in' operand");
        Err(-1)
    } else {
        let atom = prop.to_atom(ctx)?;
        let ret = obj.has_property(ctx, atom.as_ref());
        atom.free(ctx.rt_mut());
        ret
    }
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
            let atom = item.to_atom(ctx)?;
            let b = o.has_shape_property(atom.as_ref());
            atom.free(ctx.rt_mut());
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
    debug_assert_eq!(bc.func_kind(), JSFunctionKindEnum::JS_FUNC_NORMAL);

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
    let ctor =
        JSValue::from(bc.make_closure(ctx, cur_var_refs, Some(sframe), Some(parent_class))?);

    js_method_set_home_object(&ctor, Some(&proto));
    JS_SetConstructorBit(ctx, &ctor, true);

    ctor.define_property_value(
        ctx,
        JS_ATOM_length,
        JSValue::Int(bc.attrs.defined_arg_count as i32),
        JS_PROP_CONFIGURABLE,
    )?;

    if is_computed_name {
        ctor.as_object().unwrap().define_name_computed(
            ctx,
            unsafe { &*sp.offset(-3) },
            JS_PROP_CONFIGURABLE,
        )?;
    } else {
        ctor.as_object()
            .unwrap()
            .define_name(ctx, class_name, JS_PROP_CONFIGURABLE)?;
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

/// test if `atom` property is true in value's \[Symbol.unscopables\] scope.
///
/// `atom` is not consumed.
pub(super) fn js_has_unscopable(ctx: &JSContext, obj: &JSValue, atom: JSAtom) -> Result<bool, i32> {
    if let Some(o) = obj
        .get_property(ctx, JS_ATOM_Symbol_unscopables)?
        .as_object()
    {
        o.get_property(ctx, atom)?.to_bool()
    } else {
        Ok(false)
    }
}

pub(crate) fn js_get_function_name(ctx: &JSContext, name: JSAtom) -> Result<JSString, i32> {
    let name_str = name.force_string(ctx)?;
    if name.has_description(ctx) {
        JS_ConcatString3(ctx, c"[", name_str.into(), c"]")
    } else {
        Ok(name_str)
    }
}

pub fn js_method_set_home_object(func_obj: &JSValue, home_obj: Option<&JSObject>) {
    if let JSValue::Object(o) = func_obj {
        if let Some(fo) = o.payload().bc_function() {
            if fo.bytecode().need_home_object() {
                fo.set_home_object(home_obj.cloned());
            }
        }
    }
}

/// Modify the name of a method according to the atom and
/// 'flags'. 'flags' is a bitmask of JS_PROP_HAS_GET and
/// JS_PROP_HAS_SET. Also set the home object of the method.
pub fn js_method_set_properties(
    ctx: &JSContext,
    func_obj: &JSValue,
    name: JSAtom,
    flags: u32,
    home_obj: &JSValue,
) -> Result<(), i32> {
    let mut name_str = js_get_function_name(ctx, name)?;

    if flags & JS_PROP_HAS_GET != 0 {
        name_str = JS_ConcatString3(ctx, c"get ", name_str.into(), c"")?;
    } else if flags & JS_PROP_HAS_SET != 0 {
        name_str = JS_ConcatString3(ctx, c"set ", name_str.into(), c"")?;
    }

    func_obj.define_property_value(ctx, JS_ATOM_name, name_str.into(), JS_PROP_CONFIGURABLE)?;
    js_method_set_home_object(func_obj, home_obj.as_object());
    Ok(())
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

    let is_sparse_array = if is_array_iterator
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
                Ok(false) // early exit for fast path (don't go slow path)
            } else {
                Ok(true) // try slow path
            }
        })
        .unwrap_or(Ok(true))?
    } else {
        true
    };

    if is_sparse_array {
        // slow path for sparse array, use iterator
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
        let obj = std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED);
        let it = ForInIterPayload::new(ctx, obj).map_or(JSValue::EXCEPTION, JSValue::Object);

        std::ptr::replace(sp.offset(-1), it);

        if (*sp.offset(-1)).is_exception() {
            Err(-1)
        } else {
            Ok(())
        }
    }
}

/// 实现 JavaScript 中 for...in 循环的迭代器逻辑，用于遍历对象的可枚举属性
/// enum_obj -> enum_obj value done
pub fn js_for_in_next(ctx: &JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let enum_obj = unsafe { &*sp.offset(-1) };
    debug_assert!(enum_obj.is_object());

    let ob = unsafe { enum_obj.as_object_unchecked() };

    if let ObjectPayload::FOR_IN_ITERATOR(it) = ob.payload() {
        let mut prop: JSAtom;

        loop {
            let mut it = it.borrow_mut();

            if it.idx >= it.prop_count {
                if it.obj.is_undefined_or_null() {
                    // object has no parent prototype
                    unsafe {
                        std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                        std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                    }
                    return Ok(());
                }

                if !it.in_prototype_chain {
                    let done = it.prepare_prototype_chain_enum(ctx)?;
                    for name in it.prop_enums.iter() {
                        enum_obj.define_property_value(
                            ctx,
                            name.atom.as_ref(),
                            JSValue::NULL,
                            JS_PROP_ENUMERABLE,
                        )?;
                    }
                    if done {
                        unsafe {
                            std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                            std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                        }
                        return Ok(());
                    } else {
                        it.in_prototype_chain = true;
                    }
                }

                // walk up prototype chain.
                it.obj = it.obj.prototype(ctx)?;

                // end of prototype chain.
                if it.obj.is_null() {
                    unsafe {
                        std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                        std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                    }
                    return Ok(());
                }

                // must check for timeout to avoid infinite loop
                ctx.poll_interrupts()?;

                let names = unsafe {
                    it.obj
                        .as_object_unchecked()
                        .get_own_property_names(ctx, JS_GPN_INDEX_MASK | JS_GPN_SET_ENUM)?
                };
                it.prop_count = names.len() as _;
                it.prop_enums = names;
                it.idx = 0;
            } else {
                // enumerate on props_enum of current it.obj.
                if it.is_array {
                    prop = JSAtom::new_small_int(it.idx);
                    it.idx += 1;
                } else {
                    prop = it.prop_enums[it.idx as usize].atom.as_ref();
                    let is_enumerable = it.prop_enums[it.idx as usize].is_enumerable;

                    it.idx += 1;
                    if it.in_prototype_chain {
                        // slow case: we are in the prototype chain
                        match unsafe { enum_obj.as_object_unchecked().get_own_property(ctx, prop)? }
                        {
                            Some(_) => {
                                continue; // already visited
                            }
                            None => {
                                // add to the visited property list
                                enum_obj.define_property_value(
                                    ctx,
                                    prop,
                                    JSValue::NULL,
                                    JS_PROP_ENUMERABLE,
                                )?;
                            }
                        }
                    }
                    if !is_enumerable {
                        continue;
                    }
                }

                if it.obj.get_own_property(ctx, prop)?.is_some() {
                    break;
                }
            }
        }

        unsafe {
            std::ptr::replace(
                sp.offset(0),
                prop.string_or_symbol(ctx).unwrap_or(JSValue::EXCEPTION),
            ); // value
            std::ptr::replace(sp.offset(1), JSValue::Bool(false)); // done?
        }
    } else {
        unsafe {
            std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
            std::ptr::replace(sp.offset(1), JSValue::Bool(true));
        }
    }

    Ok(())
}

/// Get enumerate object and next method of sp[-1] object.
/// Note: new values are updated to sp[-1], sp[0], old value in sp[-1] is consumed.
/// if fail, sp[-1] is not consumed.
#[deprecated(
    note = "use JSValue::get_iterator_must() and JSValue::get_property(JS_ATOM_next) combination"
)]
pub fn js_for_of_start(ctx: &JSContext, sp: *mut JSValue, is_async: bool) -> Result<(), i32> {
    let iter_obj = unsafe { &*sp.offset(-1) }.get_iterator_must(ctx, is_async)?;
    let next = iter_obj.get_property(ctx, JS_ATOM_next)?;
    unsafe {
        std::ptr::replace(sp.offset(-1), iter_obj.into());
        std::ptr::replace(sp.offset(0), next);
    }
    Ok(())
}

#[deprecated]
pub unsafe fn js_for_of_next(ctx: &JSContext, sp: *mut JSValue, base: i32) -> Result<(), i32> {
    let mut value: JSValue = JSValue::UNDEFINED;
    let mut done = true;

    if let JSValue::Object(iter_obj) = &*sp.offset(base as isize) {
        if let Ok((v, d)) =
            ObjectIter::fetch_next(ctx, iter_obj, &*sp.offset((base + 1) as isize), &[])
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

    std::ptr::replace(sp.add(0), value);
    std::ptr::replace(sp.add(1), JSValue::Bool(done));
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
