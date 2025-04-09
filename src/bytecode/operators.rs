use std::ptr::NonNull;

use crate::{
    atom::{consts::*, JSAtom, JS_FreeAtom, JS_ValueToAtom, JS_ATOM_NULL},
    class::JS_DEFINE_CLASS_HAS_HERITAGE,
    context::JSContext,
    function::{
        js_class_has_bytecode, js_closure2, js_poll_interrupts, JSFunctionBytecode,
        JSFunctionKindEnum, JSVarRef,
    },
    gc::set_cycle_flag,
    iterator::{
        build_for_in_iterator, js_array_iterator_next, js_create_array_iterator,
        js_for_in_prepare_prototype_chain_enum, JSIteratorKindEnum, JS_GetIterator,
        JS_IteratorClose, JS_IteratorGetCompleteValue, JS_IteratorNext,
    },
    stack_frame::JSStackFrame,
    types::unlikely,
    value::{
        object::{
            js_get_length32, JS_CheckBrand, JS_DefineObjectName, JS_DefineObjectNameComputed,
            JS_GetOwnPropertyInternal, JS_GetPrototypeFree, JS_IsCFunction, JS_IsConstructor,
            JS_IsInstanceOf, JS_NewObjectProto, JS_NewObjectProtoClass, JS_SetConstructorBit,
            JS_GPN_SET_ENUM, JS_GPN_STRING_MASK, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E,
            JS_PROP_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_THROW,
            JS_PROP_THROW_STRICT, JS_PROP_WRITABLE,
        },
        JSTag, JSValue, JSValueConst, JS_AtomSymbolHasDescription, JS_ConcatString3, JS_FreeValue,
        JS_NewBool, JS_ToBoolFree, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_PTR, JS_VALUE_GET_TAG,
    },
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_FOR_IN_ITERATOR, JS_CLASS_OBJECT, JS_MKPTR,
};

pub unsafe fn js_operator_delete(ctx: &mut JSContext, sp: *mut JSValue) -> i32 {
    let op1 = *sp.offset(-2);
    let op2 = *sp.offset(-1);
    let atom = JS_ValueToAtom(ctx, op2);

    if atom == JS_ATOM_NULL {
        return -1;
    }

    let ret = op1.delete_property(ctx, atom, JS_PROP_THROW_STRICT);
    JS_FreeAtom(ctx, atom);

    if let Ok(ret) = ret {
        JS_FreeValue(ctx, op1);
        JS_FreeValue(ctx, op2);
        *sp.offset(-2) = JS_NewBool(ret);
        0
    } else {
        -1
    }
}

pub unsafe fn js_operator_instanceof(ctx: &mut JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let op1 = *sp.offset(-2);
    let op2 = *sp.offset(-1);
    let ret = JS_IsInstanceOf(ctx, op1, op2)?;

    JS_FreeValue(ctx, op1);
    JS_FreeValue(ctx, op2);
    *sp.offset(-2) = JS_NewBool(ret);
    Ok(())
}

pub unsafe fn js_operator_typeof(ctx: &mut JSContext, op1: JSValueConst) -> JSAtom {
    match JS_VALUE_GET_NORM_TAG(op1) {
        JSTag::Int | JSTag::Float64 => JS_ATOM_number,
        JSTag::Undefined => JS_ATOM_undefined,
        JSTag::Bool => JS_ATOM_boolean,
        JSTag::String => JS_ATOM_string,
        JSTag::Object => {
            let p = JS_VALUE_GET_OBJ(op1);
            if (*p).detailed.is_htmldda() {
                JS_ATOM_undefined
            } else if op1.is_function(ctx) {
                JS_ATOM_function
            } else {
                JS_ATOM_object
            }
        }
        JSTag::Null => JS_ATOM_object,
        JSTag::Symbol => JS_ATOM_symbol,
        JSTag::BigInt => JS_ATOM_bigint,
        _ => JS_ATOM_unknown,
    }
}

pub unsafe fn js_operator_in(ctx: &mut JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let op1 = *sp.offset(-2);
    let op2 = *sp.offset(-1);

    if JS_VALUE_GET_TAG(op2) != JSTag::Object as i32 {
        ctx.JS_ThrowTypeError(c"invalid 'in' operand");
        return Err(-1);
    }

    let atom = JS_ValueToAtom(ctx, op1);
    if unlikely(atom == JS_ATOM_NULL) {
        return Err(-1);
    }

    let ret = op2.has_property(ctx, atom)?;
    JS_FreeAtom(ctx, atom);
    JS_FreeValue(ctx, op1);
    JS_FreeValue(ctx, op2);

    *sp.offset(-2) = JS_NewBool(ret);
    Ok(())
}

pub unsafe fn js_operator_private_in(ctx: &mut JSContext, sp: *mut JSValue) -> Result<(), i32> {
    let op1 = *sp.offset(-2); // object
    let op2 = *sp.offset(-1); // field name or method function

    if JS_VALUE_GET_TAG(op1) != JSTag::Object as i32 {
        ctx.JS_ThrowTypeError(c"invalid 'in' operand");
        return Err(-1);
    }

    let ret = if op2.is_object() {
        // method: use the brand
        JS_CheckBrand(ctx, op1, op2)?
    } else {
        // field
        let atom = JS_ValueToAtom(ctx, op2);
        if atom == JS_ATOM_NULL {
            return Err(-1);
        }
        let p = JS_VALUE_GET_OBJ(op1);
        let prs = (*p).find_own_property1(atom);
        JS_FreeAtom(ctx, atom);
        prs.is_some()
    };

    JS_FreeValue(ctx, op1);
    JS_FreeValue(ctx, op2);
    *sp.offset(-2) = JS_NewBool(ret);

    Ok(())
}

pub unsafe fn js_op_define_class(
    ctx: &mut JSContext,
    sp: *mut JSValue,
    class_name: JSAtom,
    class_flags: i32,
    cur_var_refs: &[NonNull<JSVarRef>],
    sf: *mut JSStackFrame,
    is_computed_name: bool,
) -> Result<(), i32> {
    let mut bfunc = *sp.offset(-1);
    let mut parent_class = *sp.offset(-2);

    let mut proto = JSValue::UNDEFINED;
    let mut ctor = JSValue::UNDEFINED;
    let mut parent_proto = JSValue::UNDEFINED;

    let is_fail = loop {
        if class_flags & JS_DEFINE_CLASS_HAS_HERITAGE != 0 {
            if parent_class.is_null() {
                parent_proto = JSValue::NULL;
                parent_class = ctx.function_proto.dup();
            } else {
                if !JS_IsConstructor(ctx, parent_class) {
                    ctx.JS_ThrowTypeError(c"parent class must be constructor ");
                    break true; // fail
                }

                parent_proto = parent_class.get_property(ctx, JS_ATOM_prototype);
                if parent_proto.is_exception() {
                    break true; // fail
                }

                if !parent_proto.is_null() && !parent_proto.is_object() {
                    ctx.JS_ThrowTypeError(c"parent prototype must be an object or null");
                    break true; // fail
                }
            }
        } else {
            /* parent_class is JSValue::UNDEFINED in this case */
            parent_proto = ctx.class_proto[JS_CLASS_OBJECT as usize].dup();
            parent_class = ctx.function_proto.dup();
        }

        proto = JS_NewObjectProto(ctx, parent_proto);
        if proto.is_exception() {
            break true; // fail
        }

        let b = JS_VALUE_GET_PTR(bfunc).cast::<JSFunctionBytecode>();
        debug_assert_eq!((*b).func_kind, JSFunctionKindEnum::JS_FUNC_NORMAL);
        ctor = JS_NewObjectProtoClass(ctx, parent_class, JS_CLASS_BYTECODE_FUNCTION);
        if ctor.is_exception() {
            break true; // fail
        }

        ctor = js_closure2(ctx, ctor, b, cur_var_refs, sf);
        bfunc = JSValue::UNDEFINED;
        if ctor.is_exception() {
            break true; // fail
        }

        js_method_set_home_object(ctx, ctor, proto);
        JS_SetConstructorBit(ctx, ctor, true);
        ctor.define_property_value(
            ctx,
            JS_ATOM_length,
            JSValue::from_i32((*b).defined_arg_count as _),
            JS_PROP_CONFIGURABLE,
        )
        .unwrap();

        if is_computed_name {
            if JS_DefineObjectNameComputed(ctx, ctor, *sp.offset(-3), JS_PROP_CONFIGURABLE) < 0 {
                break true; // fail
            }
        } else {
            if JS_DefineObjectName(ctx, ctor, class_name, JS_PROP_CONFIGURABLE) < 0 {
                break true; // fail
            }
        }

        /* the constructor property must be first. It can be overriden by
        computed property names */
        if proto
            .define_property_value(
                ctx,
                JS_ATOM_constructor,
                ctor.dup(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE | JS_PROP_THROW,
            )
            .is_err()
        {
            break true; // fail
        }

        // set the prototype property
        if ctor
            .define_property_value(ctx, JS_ATOM_prototype, proto.dup(), JS_PROP_THROW)
            .is_err()
        {
            break true; // fail
        }

        set_cycle_flag(ctx, ctor);
        set_cycle_flag(ctx, proto);

        JS_FreeValue(ctx, parent_proto);
        JS_FreeValue(ctx, parent_class);

        *sp.offset(-2) = ctor;
        *sp.offset(-1) = proto;

        break false; // success
    };

    if is_fail {
        JS_FreeValue(ctx, parent_class);
        JS_FreeValue(ctx, parent_proto);
        JS_FreeValue(ctx, bfunc);
        JS_FreeValue(ctx, proto);
        JS_FreeValue(ctx, ctor);
        *sp.offset(-2) = JSValue::UNDEFINED;
        *sp.offset(-1) = JSValue::UNDEFINED;
        Err(-1)
    } else {
        Ok(())
    }
}

pub unsafe fn js_has_unscopable(ctx: &mut JSContext, obj: JSValueConst, atom: JSAtom) -> i32 {
    let arr = obj.get_property(ctx, JS_ATOM_Symbol_unscopables);
    if arr.is_exception() {
        return -1;
    }

    let ret = if arr.is_object() {
        let val = arr.get_property(ctx, atom);
        JS_ToBoolFree(ctx, val).map_or(-1, |b| b as i32)
    } else {
        0
    };
    JS_FreeValue(ctx, arr);
    ret
}

pub unsafe fn js_get_function_name(ctx: &mut JSContext, name: JSAtom) -> JSValue {
    let mut name_str = name.to_value(ctx, true);
    if JS_AtomSymbolHasDescription(ctx, name) {
        name_str = JS_ConcatString3(ctx, c"[", name_str, c"]");
    }
    name_str
}

pub unsafe fn js_method_set_home_object(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    home_obj: JSValueConst,
) {
    if JS_VALUE_GET_TAG(func_obj) != JSTag::Object as i32 {
        return;
    }

    let p = JS_VALUE_GET_OBJ(func_obj);
    if !js_class_has_bytecode((*p).class_id()) {
        return;
    }

    let b = (*p).u.func.function_bytecode;
    if (*b).need_home_object {
        // free current home object
        let p1 = (*p).u.func.home_object;
        if !p1.is_null() {
            JS_FreeValue(ctx, JS_MKPTR!(JSTag::Object, p1.cast()));
        }

        // set new home object
        (*(*p).u.func).home_object = if JS_VALUE_GET_TAG(home_obj) == JSTag::Object as i32 {
            JS_VALUE_GET_OBJ(home_obj.dup())
        } else {
            std::ptr::null_mut()
        };
    }
}

// Modify the name of a method according to the atom and
// 'flags'. 'flags' is a bitmask of JS_PROP_HAS_GET and
// JS_PROP_HAS_SET. Also set the home object of the method.
// Return < 0 if exception.
pub unsafe fn js_method_set_properties(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    name: JSAtom,
    flags: u32,
    home_obj: JSValueConst,
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
pub unsafe fn js_append_enumerate(ctx: &mut JSContext, sp: *mut JSValue) -> i32 {
    if JS_VALUE_GET_TAG(*sp.offset(-2)) != JSTag::Int as i32 {
        ctx.JS_ThrowInternalError(c"invalid index for append");
        return -1;
    }

    let src = *sp.offset(-1); // the source iterator or array
    let dest = *sp.offset(-3); // the dest array

    #[cfg(debug_assertions)]
    let _src_rc0 = src.ref_count();

    // XXX: further optimisations:
    //       - use ctx->array_proto_values?
    //       - check if array_iterator_prototype next method is built-in and
    //         avoid constructing actual iterator object?
    //       - build this into js_for_of_start and use in all `for (x of o)` loops
    let iterator = src.get_property(ctx, JS_ATOM_Symbol_iterator);
    if iterator.is_exception() {
        return -1;
    }

    let is_array_iterator = JS_IsCFunction(
        ctx,
        iterator,
        std::mem::transmute(
            js_create_array_iterator
                as *const fn(
                    *mut JSContext,
                    JSValueConst,
                    i32,
                    *const JSValueConst,
                    i32,
                ) -> JSValue,
        ),
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as _,
    );
    JS_FreeValue(ctx, iterator);

    let enumobj = JS_GetIterator(ctx, src, false);
    if enumobj.is_exception() {
        return -1;
    }

    let method = enumobj.get_property(ctx, JS_ATOM_next);
    if method.is_exception() {
        JS_FreeValue(ctx, enumobj);
        return -1;
    }

    macro_rules! fail {
        () => {
            JS_IteratorClose(ctx, enumobj, true).expect("TODO");
            JS_FreeValue(ctx, enumobj);
            JS_FreeValue(ctx, method);
            return -1;
        };
    }

    // .length property of the array
    let len = if let Ok(l) = js_get_length32(ctx, src) {
        l as usize
    } else {
        fail!();
    };

    // check if `src` is a fast array
    let src_fast_array_values = if is_array_iterator
        && JS_IsCFunction(
            ctx,
            method,
            std::mem::transmute(
                js_array_iterator_next
                    as *const fn(
                        *mut JSContext,
                        JSValueConst,
                        i32,
                        *const JSValueConst,
                        i32,
                    ) -> JSValue,
            ),
            0,
        ) {
        src.fast_array_values()
            .and_then(|a| if a.len() == len { Some(a) } else { None })
    } else {
        None
    };

    // the index where append starts
    let mut pos = JS_VALUE_GET_INT(*sp.offset(-2));

    if let Some(lst) = src_fast_array_values {
        // fast path
        for v in lst {
            if dest
                .define_property_value_u32(ctx, pos as _, (*v).dup(), JS_PROP_C_W_E)
                .is_err()
            {
                fail!();
            }
            pos += 1;
        }
    } else {
        // general case: use iterator
        loop {
            let mut done = false;
            let value = JS_IteratorNext(ctx, enumobj, method, 0, std::ptr::null_mut(), &mut done);
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
    *sp.offset(-2) = JSValue::from_i32(pos);

    JS_FreeValue(ctx, enumobj);
    JS_FreeValue(ctx, method);

    #[cfg(debug_assertions)]
    debug_assert_eq!(src.ref_count().unwrap_or(-1), _src_rc0.unwrap_or(-1));

    0
}

/* obj -> enum_obj */
pub unsafe fn js_for_in_start(ctx: &mut JSContext, sp: *mut JSValue) -> i32 {
    *sp.offset(-1) = build_for_in_iterator(ctx, *sp.offset(-1));
    if (*sp.offset(-1)).is_exception() {
        -1
    } else {
        0
    }
}

/// 实现 JavaScript 中 for...in 循环的迭代器逻辑，用于遍历对象的可枚举属性
/// enum_obj -> enum_obj value done
pub unsafe fn js_for_in_next(ctx: &mut JSContext, sp: *mut JSValue) -> i32 {
    let enum_obj: JSValueConst = *sp.offset(-1);

    // fail safe
    if JS_VALUE_GET_TAG(enum_obj) != JSTag::Object as i32 {
        // C2RS: goto done translated to return 0 with done logic
        *sp.offset(0) = JSValue::UNDEFINED;
        *sp.offset(1) = JSValue::TRUE;
        return 0;
    }

    let p = JS_VALUE_GET_OBJ(enum_obj);

    // 检查迭代器对象是否有效。
    if (*p).class_id() != JS_CLASS_FOR_IN_ITERATOR {
        // C2RS: goto done translated to return 0 with done logic
        *sp.offset(0) = JSValue::UNDEFINED;
        *sp.offset(1) = JSValue::TRUE;
        return 0;
    }

    let it = (*p).u.for_in_iterator;
    let mut prop: JSAtom;

    loop {
        if (*it).idx >= (*it).atom_count {
            // 当前对象的属性遍历完成，需要查找原型链
            if (*it).obj.is_undefined_or_null() {
                *sp.offset(0) = JSValue::UNDEFINED;
                *sp.offset(1) = JSValue::TRUE;
                return 0;
            }

            if !(*it).in_prototype_chain {
                let ret = js_for_in_prepare_prototype_chain_enum(ctx, enum_obj);
                if ret < 0 {
                    return -1;
                }
                if ret != 0 {
                    // C2RS: goto done translated to return 0 with done logic
                    *sp.offset(0) = JSValue::UNDEFINED;
                    *sp.offset(1) = JSValue::TRUE;
                    return 0;
                }
                (*it).in_prototype_chain = true;
            }

            (*it).obj = JS_GetPrototypeFree(ctx, (*it).obj);
            if (*it).obj.is_exception() {
                return -1;
            }
            if (*it).obj.is_null() {
                *sp.offset(0) = JSValue::UNDEFINED;
                *sp.offset(1) = JSValue::TRUE;
                return 0;
            }

            // must check for timeout to avoid infinite loop
            if js_poll_interrupts(ctx) != 0 {
                return -1;
            }

            if let Ok(names) = (*JS_VALUE_GET_OBJ((*it).obj))
                .get_own_property_names_internal(ctx, JS_GPN_STRING_MASK | JS_GPN_SET_ENUM)
            {
                (*it).atom_count = names.len() as _;
                (*it).tab_atom = names;
                (*it).idx = 0;
            } else {
                return -1;
            }
        } else {
            // 遍历当前对象的属性

            if (*it).is_array {
                prop = JSAtom::from_u32((*it).idx);
                (*it).idx += 1;
            } else {
                prop = (*it).tab_atom[(*it).idx as usize].atom;
                let is_enumerable = (*it).tab_atom[(*it).idx as usize].is_enumerable;

                (*it).idx += 1;
                if (*it).in_prototype_chain {
                    // slow case: we are in the prototype chain
                    let ret = JS_GetOwnPropertyInternal(
                        ctx,
                        std::ptr::null_mut(),
                        JS_VALUE_GET_OBJ(enum_obj),
                        prop,
                    );
                    if ret < 0 {
                        return ret;
                    }
                    if ret != 0 {
                        continue; // already visited
                    }

                    // add to the visited property list
                    if enum_obj
                        .define_property_value(ctx, prop, JSValue::NULL, JS_PROP_ENUMERABLE)
                        .is_err()
                    {
                        return -1;
                    }
                }
                if !is_enumerable {
                    continue;
                }
            }

            // check if the property was deleted
            let ret = JS_GetOwnPropertyInternal(
                ctx,
                std::ptr::null_mut(),
                JS_VALUE_GET_OBJ((*it).obj),
                prop,
            );
            if ret < 0 {
                return ret;
            }
            if ret != 0 {
                break;
            }
        }
    }

    // return the property
    *sp.offset(0) = prop.to_value(ctx, false); // 属性名
    *sp.offset(1) = JSValue::FALSE; // 未完成标志

    0
}

// obj -> enum_rec (3 slots)
pub unsafe fn js_for_of_start(ctx: &mut JSContext, sp: *mut JSValue, is_async: bool) -> i32 {
    let op1 = *sp.offset(-1);
    let obj = JS_GetIterator(ctx, op1, is_async);
    if obj.is_exception() {
        return -1;
    }
    JS_FreeValue(ctx, op1);
    *sp.offset(-1) = obj;

    let method = obj.get_property(ctx, JS_ATOM_next);
    if method.is_exception() {
        return -1;
    }
    *sp.offset(0) = method;
    return 0;
}

// enum_rec [objs] -> enum_rec [objs] value done. There are 'offset'
// objs. If 'done' is true or in case of exception, 'enum_rec' is set
// to undefined. If 'done' is true, 'value' is always set to
// undefined.
pub unsafe fn js_for_of_next(ctx: &mut JSContext, sp: *mut JSValue, offset: i32) -> i32 {
    let mut value: JSValue = JSValue::UNDEFINED;
    let mut done = true;

    if !(*sp.offset(offset as isize)).is_undefined() {
        value = JS_IteratorNext(
            ctx,
            *sp.offset(offset as isize),
            *sp.offset((offset + 1) as isize),
            0,
            std::ptr::null_mut(),
            &mut done,
        );
        if value.is_exception() {
            // value is JSValue::UNDEFINED or JS_EXCEPTION
            // replace the iteration object with undefined
            JS_FreeValue(ctx, *sp.offset(offset as isize));
            *sp.offset(offset as isize) = JSValue::UNDEFINED;
            return -1;
        }

        if done {
            // value is JSValue::UNDEFINED or JS_EXCEPTION
            // replace the iteration object with undefined
            JS_FreeValue(ctx, *sp.offset(offset as isize));
            *sp.offset(offset as isize) = JSValue::UNDEFINED;

            JS_FreeValue(ctx, value);
            value = JSValue::UNDEFINED;
        }
    }

    *sp.offset(0) = value;
    *sp.offset(1) = JS_NewBool(done);

    0
}

pub unsafe fn js_iterator_get_value_done(ctx: &mut JSContext, sp: *mut JSValue) -> i32 {
    let obj = *sp.offset(-1);
    if !obj.is_object() {
        ctx.JS_ThrowTypeError(c"iterator must return an object");
        return -1;
    }

    let mut done = false;
    let value = JS_IteratorGetCompleteValue(ctx, obj, &mut done);
    if value.is_exception() {
        return -1;
    }

    JS_FreeValue(ctx, obj);
    (*sp.offset(-1)) = value;
    (*sp.offset(0)) = JS_NewBool(done);

    0
}
