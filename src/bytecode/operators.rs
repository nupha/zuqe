use crate::{
    JS_CLASS_OBJECT,
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_ValueToAtom, JSAtom, consts::*},
    class::JS_DEFINE_CLASS_HAS_HERITAGE,
    context::JSContext,
    function::{JSFunctionKindEnum, JSObjectFunc, JSVarRef, NativeFunctionType, js_closure2},
    gc::set_cycle_flag,
    iterator::{
        JS_GetIterator, JS_IteratorClose, JS_IteratorGetCompleteValue, JS_IteratorNext,
        JSIteratorKindEnum, build_for_in_iterator, js_array_iterator_next,
        js_create_array_iterator, js_for_in_prepare_prototype_chain_enum,
    },
    object::{
        JS_DefineObjectName, JS_DefineObjectNameComputed, JS_GPN_SET_ENUM, JS_GPN_STRING_MASK,
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET,
        JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_WRITABLE, JS_SetConstructorBit, JSObjectIsa,
        js_get_length32,
    },
    stack_frame::JSStackFrame,
    value::{JS_AtomSymbolHasDescription, JS_ConcatString3, JSValue, JSValueConst},
};

pub unsafe fn js_operator_delete(ctx: &JSContext, sp: *mut JSValue) -> i32 {
    let (op1, op2) = unsafe {
        (
            std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED),
            std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED),
        )
    };

    let atom = JS_ValueToAtom(ctx, &op2);
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
    let (op1, op2) = unsafe {
        (
            std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED),
            std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED),
        )
    };

    let b = op2.instance_of(ctx, &op1)?; // op2=obj, op1=type
    unsafe {
        std::ptr::replace(sp.offset(-2), JSValue::Bool(b));
    }

    Ok(())
}

pub unsafe fn js_operator_in(ctx: &JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let op1 = unsafe { std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED) };
    let op2 = unsafe { std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED) };

    if !op2.is_object() {
        ctx.JS_ThrowTypeError("invalid 'in' operand");
        return Err(-1);
    }

    let atom = JS_ValueToAtom(ctx, &op1);
    if atom == JS_ATOM_NULL {
        return Err(-1);
    }

    let ret = op2.has_property(ctx, atom)?;
    JS_FreeAtom(ctx, atom);

    unsafe {
        std::ptr::replace(sp.offset(-2), JSValue::Bool(ret));
    }
    Ok(())
}

pub unsafe fn js_operator_private_in(ctx: &JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let op1 = unsafe { std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED) }; // object
    let op2 = unsafe { std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED) }; // field name or method function

    if !matches!(op1, JSValue::Object(_)) {
        ctx.JS_ThrowTypeError("invalid 'in' operand");
        return Err(-1);
    }

    let ret = if op2.is_object() {
        // method: use the brand
        op1.check_brand(ctx, &op2)?
    } else {
        // field
        let atom = JS_ValueToAtom(ctx, &op2);
        if atom == JS_ATOM_NULL {
            return Err(-1);
        }

        let b = op1.object_unchecked().has_shape_property(atom);
        JS_FreeAtom(ctx, atom);
        b
    };

    unsafe {
        std::ptr::replace(sp.offset(-2), JSValue::Bool(ret));
    }
    Ok(())
}

pub unsafe fn js_op_define_class(
    ctx: &JSContext,
    sp: *mut JSValue,
    class_name: JSAtom,
    class_flags: i32,
    cur_var_refs: &[JSVarRef],
    sf: *mut JSStackFrame,
    is_computed_name: bool,
) -> Result<(), i32> {
    let mut bfunc = unsafe { std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED) };
    let mut parent_class = unsafe { std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED) };
    let parent_proto: JSValue;

    if class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0 {
        if parent_class.is_null() {
            parent_proto = JSValue::NULL;
            parent_class = ctx.0.borrow().function_proto.clone();
        } else {
            if !parent_class.is_constructor() {
                ctx.JS_ThrowTypeError("parent class must be constructor ");
                return Err(-1);
            }

            parent_proto = parent_class
                .get_property(ctx, JS_ATOM_prototype)
                .check_exception()?;

            if !parent_proto.is_null() && !parent_proto.is_object() {
                ctx.JS_ThrowTypeError("parent prototype must be an object or null");
                return Err(-1);
            }
        }
    } else {
        // parent_class is JSValue::UNDEFINED in this case
        parent_proto = ctx.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone();
        parent_class = ctx.0.borrow().function_proto.clone();
    }

    let proto = ctx.create_base_object_with_proto(&parent_proto)?;

    let bc = unsafe {
        bfunc
            .object_unchecked()
            .0
            .isa
            .borrow()
            .func()
            .and_then(|f| f.bytecode.clone())
            .unwrap()
    };
    debug_assert_eq!(bc.0.borrow().func_kind, JSFunctionKindEnum::JS_FUNC_NORMAL);

    let mut ctor = ctx
        .create_instance_from_proto(
            &parent_class,
            JSObjectIsa::BYTECODE_FUNCTION(JSObjectFunc::default()),
        )
        .map_err(|_| -1)?;

    ctor = js_closure2(ctx, ctor, bc.clone(), cur_var_refs, sf).check_exception()?;
    bfunc = JSValue::UNDEFINED;

    js_method_set_home_object(ctx, &ctor, &proto);
    JS_SetConstructorBit(ctx, &ctor, true);

    ctor.define_property_value(
        ctx,
        JS_ATOM_length,
        JSValue::Int(bc.0.borrow().defined_arg_count as i32),
        JS_PROP_CONFIGURABLE,
    )
    .unwrap();

    if is_computed_name {
        if JS_DefineObjectNameComputed(ctx, &ctor, &*sp.offset(-3), JS_PROP_CONFIGURABLE) < 0 {
            return Err(-1);
        }
    } else {
        if JS_DefineObjectName(ctx, &ctor, class_name, JS_PROP_CONFIGURABLE) < 0 {
            return Err(-1);
        }
    }

    // the constructor property must be first. It can be overriden by
    // computed property names
    proto.define_property_value(
        ctx,
        JS_ATOM_constructor,
        ctor.clone(),
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE | JS_PROP_THROW,
    )?;

    // set the prototype property
    ctor.define_property_value(ctx, JS_ATOM_prototype, proto.clone(), JS_PROP_THROW)?;

    set_cycle_flag(ctx, &ctor);
    set_cycle_flag(ctx, &proto);

    unsafe {
        std::ptr::replace(sp.offset(-2), ctor);
        std::ptr::replace(sp.offset(-1), proto);
    }

    Ok(())
}

pub unsafe fn js_has_unscopable(ctx: &JSContext, obj: &JSValueConst, atom: JSAtom) -> i32 {
    let arr = obj.get_property(ctx, JS_ATOM_Symbol_unscopables);
    if arr.is_exception() {
        return -1;
    }

    let ret = if arr.is_object() {
        let val = arr.get_property(ctx, atom);
        val.coerce_to_bool(ctx).map_or(-1, |b| b as i32)
    } else {
        0
    };
    ret
}

pub fn js_get_function_name(ctx: &JSContext, name: JSAtom) -> JSValue {
    let mut name_str = name.to_value(ctx, true);
    if JS_AtomSymbolHasDescription(ctx, name) {
        name_str = unsafe { JS_ConcatString3(ctx, c"[", name_str, c"]") };
    }
    name_str
}

/// func_obj is not consumed.
/// home_obj is not consumed.
pub fn js_method_set_home_object(
    ctx: &JSContext,
    func_obj: &JSValueConst,
    home_obj: &JSValueConst,
) {
    if let JSValue::Object(o) = func_obj {
        if let Some(fo) = o.0.isa.borrow_mut().func_mut() {
            let need_home_object = fo
                .bytecode
                .as_ref()
                .map_or(false, |b| b.0.borrow().need_home_object);

            if need_home_object {
                fo.home_object = home_obj.object().cloned();
            }
        }
    }
}

// Modify the name of a method according to the atom and
// 'flags'. 'flags' is a bitmask of JS_PROP_HAS_GET and
// JS_PROP_HAS_SET. Also set the home object of the method.
// Return < 0 if exception.
pub unsafe fn js_method_set_properties(
    ctx: &JSContext,
    func_obj: &JSValue,
    name: JSAtom,
    flags: u32,
    home_obj: &JSValue,
) -> i32 {
    let mut name_str = js_get_function_name(ctx, name);
    if flags & JS_PROP_HAS_GET != 0 {
        name_str = JS_ConcatString3(ctx, c"get ", name_str, c"");
    } else if flags & JS_PROP_HAS_SET != 0 {
        name_str = JS_ConcatString3(ctx, c"set ", name_str, c"");
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

    js_method_set_home_object(ctx, func_obj, home_obj);
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
        ctx.JS_ThrowInternalError("invalid index for append");
        return Err(-1);
    };

    let src = &*sp.offset(-1); // the source iterator or array
    let dest = &*sp.offset(-3); // the dest array

    // XXX: further optimisations:
    //       - use ctx->array_proto_values?
    //       - check if array_iterator_prototype next method is built-in and
    //         avoid constructing actual iterator object?
    //       - build this into js_for_of_start and use in all `for (x of o)` loops
    let iterator = src
        .get_property(ctx, JS_ATOM_Symbol_iterator)
        .check_exception()?;

    let is_array_iterator = iterator.match_native_callback(
        NativeFunctionType::Func(js_create_array_iterator),
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as _,
    );

    let enumobj = JS_GetIterator(ctx, src, false).check_exception()?;
    let method = enumobj.get_property(ctx, JS_ATOM_next).check_exception()?;

    macro_rules! fail {
        () => {
            JS_IteratorClose(ctx, &enumobj, true).expect("TODO");
            return Err(-1);
        };
    }

    // .length property of the array
    let len = if let Ok(l) = js_get_length32(ctx, &src) {
        l as usize
    } else {
        fail!();
    };

    let slow_array = if is_array_iterator
        && method.match_native_callback(NativeFunctionType::IteratorNext(js_array_iterator_next), 0)
    {
        match src.with_fast_array_values(|lst| {
            if lst.len() == len {
                // `src` is a fast array
                for v in lst {
                    dest.define_property_value_u32(ctx, pos as _, v.clone(), JS_PROP_C_W_E)?;
                    pos += 1;
                }
                Ok(true)
            } else {
                Ok(false)
            }
        }) {
            Some(Err(e)) => {
                // fast array has error
                JS_IteratorClose(ctx, &enumobj, true).unwrap();
                return Err(e);
            }
            Some(Ok(true)) => {
                false // is fast array, done
            }
            Some(Ok(false)) | None => {
                true // use slow array
            }
        }
    } else {
        true
    };

    if slow_array {
        // general case: slow array, use iterator
        loop {
            let mut done = false;
            let value = JS_IteratorNext(ctx, &enumobj, &method, &[], &mut done);
            if value.is_exception() {
                fail!();
            }

            if done {
                break; // value is JSValue::UNDEFINED
            }

            if dest
                .define_property_value_u32(ctx, pos as _, value, JS_PROP_C_W_E)
                .is_err()
            {
                fail!();
            }

            pos += 1;
        }
    }

    // Note: could raise an error if too many elements

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
pub unsafe fn js_for_in_next(ctx: &JSContext, sp: *mut JSValue) -> i32 {
    let enum_obj = &*sp.offset(-1);

    if let Some(JSObjectIsa::FOR_IN_ITERATOR(it)) = enum_obj
        .object()
        .map(|p| p.0.isa.borrow_mut())
        .as_deref_mut()
    {
        let mut prop: JSAtom;

        loop {
            if it.idx >= it.atom_count {
                // 当前对象的属性遍历完成，需要查找原型链
                if it.obj.is_undefined_or_null() {
                    std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                    std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                    return 0;
                }

                if !it.in_prototype_chain {
                    let ret = js_for_in_prepare_prototype_chain_enum(ctx, enum_obj);
                    if ret < 0 {
                        return -1;
                    }
                    if ret != 0 {
                        std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                        std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                        return 0;
                    }
                    it.in_prototype_chain = true;
                }

                it.obj = if let Ok(o) = it.obj.prototype(ctx) {
                    o
                } else {
                    return -1;
                };
                if it.obj.is_null() {
                    std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
                    std::ptr::replace(sp.offset(1), JSValue::Bool(true));
                    return 0;
                }

                // must check for timeout to avoid infinite loop
                if ctx.poll_interrupts().is_err() {
                    return -1;
                }

                if let Ok(names) = it
                    .obj
                    .object_unchecked()
                    .get_own_property_names_internal(ctx, JS_GPN_STRING_MASK | JS_GPN_SET_ENUM)
                {
                    it.atom_count = names.len() as _;
                    it.tab_atom = names;
                    it.idx = 0;
                } else {
                    return -1;
                }
            } else {
                // 遍历当前对象的属性
                if it.is_array {
                    prop = JSAtom::new_u32((*it).idx);
                    it.idx += 1;
                } else {
                    prop = it.tab_atom[(*it).idx as usize].atom;
                    let is_enumerable = (*it).tab_atom[(*it).idx as usize].is_enumerable;

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
        std::ptr::replace(sp.offset(0), prop.to_value(ctx, false)); // 属性名
        std::ptr::replace(sp.offset(1), JSValue::Bool(false)); // 未完成标志
    } else {
        std::ptr::replace(sp.offset(0), JSValue::UNDEFINED);
        std::ptr::replace(sp.offset(1), JSValue::Bool(true));
    }

    0
}

/// Get enumerate object and next method of sp[-1] object.
/// Note: new values are updated to sp[-1], sp[0], old value of sp[-1] is consumed.
/// obj -> enum_rec (3 slots)
pub unsafe fn js_for_of_start(ctx: &JSContext, sp: *mut JSValue, is_async: bool) -> i32 {
    let op1 = &*sp.offset(-1);
    let obj = JS_GetIterator(ctx, op1, is_async);
    if obj.is_exception() {
        return -1;
    }

    unsafe {
        std::ptr::replace(sp.offset(-1), obj.clone());
    }

    let method = obj.get_property(ctx, JS_ATOM_next);
    if method.is_exception() {
        return -1;
    }

    unsafe {
        std::ptr::replace(sp.offset(0), method);
    }
    return 0;
}

/// enum_rec [objs] -> enum_rec [objs] value done. There are 'offset'
/// objs. If 'done' is true or in case of exception, 'enum_rec' is set
/// to undefined. If 'done' is true, 'value' is always set to undefined.
pub unsafe fn js_for_of_next(ctx: &JSContext, sp: *mut JSValue, offset: i32) -> Result<(), i32> {
    let mut value: JSValue = JSValue::UNDEFINED;
    let mut done = true;

    if !(*sp.offset(offset as isize)).is_undefined() {
        value = JS_IteratorNext(
            ctx,
            &*sp.offset(offset as isize),
            &*sp.offset((offset + 1) as isize),
            &[],
            &mut done,
        );
        if value.is_exception() {
            // value is JSValue::UNDEFINED or JS_EXCEPTION
            // replace the iteration object with undefined
            std::ptr::replace(sp.offset(offset as isize), JSValue::UNDEFINED);
            return Err(-1);
        }

        if done {
            // value is JSValue::UNDEFINED or JS_EXCEPTION
            // replace the iteration object with undefined
            std::ptr::replace(sp.offset(offset as isize), JSValue::UNDEFINED);
            value = JSValue::UNDEFINED;
        }
    }

    std::ptr::replace(sp.offset(0), value);
    std::ptr::replace(sp.offset(1), JSValue::Bool(done));

    Ok(())
}

pub unsafe fn js_iterator_get_value_done(ctx: &JSContext, sp: *mut JSValue) -> i32 {
    let obj = &*sp.offset(-1);
    if !obj.is_object() {
        ctx.JS_ThrowTypeError("iterator must return an object");
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
