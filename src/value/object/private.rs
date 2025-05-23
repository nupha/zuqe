use std::ffi::CStr;

use crate::{
    atom::{consts::*, js_symbol_to_atom, JSAtomTypeHint, JS_FreeAtom},
    context::JSContext,
    error::JS_ThrowTypeErrorPrivateNotFound,
    function::js_class_has_bytecode,
};

use super::{
    super::{
        set_value, JSValue, JSValueConst, JS_FreeValue, JS_NewSymbolFromAtom, JS_TAG_OBJECT,
        JS_TAG_SYMBOL, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_PROP_C_W_E,
};

// return a boolean telling if the brand of the home object of 'func'
// is present on 'obj'
pub unsafe fn JS_CheckBrand(
    ctx: *mut JSContext,
    obj: JSValueConst,
    func: JSValueConst,
) -> Result<bool, i32> {
    // get the home object of 'func'
    if JS_VALUE_GET_TAG(func) != JS_TAG_OBJECT {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let p1 = JS_VALUE_GET_OBJ(func);
    if !js_class_has_bytecode((*p1).class_id()) {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let home_obj = (*p1).isa().func().unwrap().home_object;
    if home_obj.is_null() {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    if let Some((_, pr)) = (*home_obj).find_own_property(JS_ATOM_Private_brand) {
        let brand = pr.u.value;

        // safety check
        if JS_VALUE_GET_TAG(brand) != JS_TAG_SYMBOL {
            (*ctx).JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }

        // get the brand array of 'obj'
        if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
            (*ctx).JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }

        Ok((*JS_VALUE_GET_OBJ(obj))
            .find_own_property1(js_symbol_to_atom(ctx, brand))
            .is_some())
    } else {
        (*ctx).JS_ThrowTypeError(c"expecting <brand> private field");
        Err(-1)
    }
}

// add a private brand field to 'home_obj' if not already present and
// if obj is != null add a private brand to it
pub unsafe fn JS_AddBrand(ctx: *mut JSContext, obj: JSValueConst, home_obj: JSValueConst) -> i32 {
    if JS_VALUE_GET_TAG(home_obj) != JS_TAG_OBJECT {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        return -1;
    }

    let home_p = JS_VALUE_GET_OBJ(home_obj);
    let brand = if let Some((_, pr)) = (*home_p).find_own_property(JS_ATOM_Private_brand) {
        pr.u.value.dup()
    } else {
        // if the brand is not present, add it
        let br = JS_NewSymbolFromAtom(ctx, JS_ATOM_brand, JSAtomTypeHint::Private);
        if br.is_exception() {
            return -1;
        } else if let Ok(pr) =
            (*home_p).add_property(ctx.as_mut().unwrap(), JS_ATOM_Private_brand, JS_PROP_C_W_E)
        {
            (*pr).u.value = br.dup();
            br
        } else {
            JS_FreeValue(ctx, br);
            return -1;
        }
    };

    let brand_atom = js_symbol_to_atom(ctx, brand);

    if obj.is_object() {
        let p1 = JS_VALUE_GET_OBJ(obj);

        if (*p1).find_own_property1(brand_atom).is_some() {
            JS_FreeAtom(ctx, brand_atom);
            (*ctx).JS_ThrowTypeError(c"private method is already present");
            -1
        } else {
            if let Ok(pr) = (*p1).add_property(ctx.as_mut().unwrap(), brand_atom, JS_PROP_C_W_E) {
                JS_FreeAtom(ctx, brand_atom);
                (*pr).u.value = JSValue::UNDEFINED;
                0
            } else {
                JS_FreeAtom(ctx, brand_atom);
                -1
            }
        }
    } else {
        JS_FreeAtom(ctx, brand_atom);
        0
    }
}

// Private fields can be added even on non extensible objects or Proxies
pub unsafe fn JS_DefinePrivateField(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSValueConst,
    val: JSValue,
) -> i32 {
    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        JS_FreeValue(ctx, val);
        return -1;
    }

    // safety check
    if JS_VALUE_GET_TAG(name) != JS_TAG_SYMBOL {
        (*ctx).JS_ThrowTypeErrorNotASymbol();
        JS_FreeValue(ctx, val);
        return -1;
    }

    let prop = js_symbol_to_atom(ctx, name);
    let p = JS_VALUE_GET_OBJ(obj);

    if (*p).find_own_property1(prop).is_some() {
        (*ctx).JS_ThrowTypeError(CStr::from_ptr(
            format!(
                "private class field '{}' already exists\0",
                prop.to_str((*ctx).rt())
            )
            .as_ptr()
            .cast(),
        ));
        JS_FreeValue(ctx, val);
        -1
    } else if let Ok(pr) = (*p).add_property(ctx.as_mut().unwrap(), prop, JS_PROP_C_W_E) {
        (*pr).u.value = val;
        0
    } else {
        JS_FreeValue(ctx, val);
        -1
    }
}

pub unsafe fn JS_GetPrivateField(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSValueConst,
) -> JSValue {
    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
    }
    // safety check
    if JS_VALUE_GET_TAG(name) != JS_TAG_SYMBOL {
        return ctx.as_mut().unwrap().JS_ThrowTypeErrorNotASymbol();
    }

    let prop = js_symbol_to_atom(ctx, name as JSValue);
    let p = JS_VALUE_GET_OBJ(obj);

    if let Some((_, pr)) = (*p).find_own_property(prop) {
        pr.u.value.dup()
    } else {
        JS_ThrowTypeErrorPrivateNotFound(ctx, prop);
        JSValue::EXCEPTION
    }
}

pub unsafe fn JS_SetPrivateField(
    ctx: *mut JSContext,
    obj: JSValueConst,
    name: JSValueConst,
    val: JSValue,
) -> i32 {
    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        JS_FreeValue(ctx, val);
        return -1;
    }

    // ensure name must be a Symbol
    if JS_VALUE_GET_TAG(name) != JS_TAG_SYMBOL {
        (*ctx).JS_ThrowTypeErrorNotASymbol();
        JS_FreeValue(ctx, val);
        return -1;
    }

    let prop = js_symbol_to_atom(ctx, name as JSValue);
    let p = JS_VALUE_GET_OBJ(obj);

    if let Some((_, pr)) = (*p).find_own_property_mut(prop) {
        set_value(ctx, &mut pr.u.value, val);
        0
    } else {
        JS_ThrowTypeErrorPrivateNotFound(ctx, prop);
        JS_FreeValue(ctx, val);
        -1
    }
}
