use crate::{
    atom::consts::{JS_ATOM_cause, JS_ATOM_errors, JS_ATOM_message, JS_ATOM_prototype},
    context::{JSContext, JS_GetActiveFunction},
    error::{build_backtrace, JSErrorEnum, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL},
    function::JS_GetFunctionRealm,
    iterator::iterator_to_array,
    value::{
        object::{JS_NewObjectProtoClass, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
        JSValue, JSValueConst, JS_FreeValue, JS_ToString,
    },
    JS_CLASS_ERROR,
};

pub unsafe fn js_error_constructor(
    ctx: *mut JSContext,
    mut new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    if new_target.is_undefined() {
        let active_function = JS_GetActiveFunction(ctx);
        new_target = active_function;
    }

    let mut proto = new_target.get_property(ctx.as_mut().unwrap(), JS_ATOM_prototype);
    if proto.is_exception() {
        return proto;
    }

    if !proto.is_object() {
        JS_FreeValue(ctx, proto);
        let realm = JS_GetFunctionRealm(ctx, new_target);
        if realm.is_null() {
            return JSValue::EXCEPTION;
        }
        let proto1 = if magic < 0 {
            (*realm).class_proto[JS_CLASS_ERROR as usize]
        } else {
            (*realm).native_error_proto[magic as usize]
        };

        proto = proto1.dup();
    }

    let obj = JS_NewObjectProtoClass(ctx, proto, JS_CLASS_ERROR);
    JS_FreeValue(ctx, proto);
    if obj.is_exception() {
        return obj;
    }

    let mut arg_index = if magic == JSErrorEnum::Aggregate as i32 {
        1
    } else {
        0
    };
    let message = *argv.offset(arg_index as isize);
    arg_index += 1;

    if !message.is_undefined() {
        let msg = JS_ToString(ctx, message);
        if msg.is_exception() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        let _ = obj.define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_message,
            msg,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );
    }

    if arg_index < argc {
        let options = *argv.offset(arg_index as isize);

        if options.is_object() {
            let present = if let Ok(b) = options.has_property(ctx.as_mut().unwrap(), JS_ATOM_cause)
            {
                b
            } else {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            };

            if present {
                let cause = options.get_property(ctx.as_mut().unwrap(), JS_ATOM_cause);
                if cause.is_exception() {
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
                let _ = obj.define_property_value(
                    ctx.as_mut().unwrap(),
                    JS_ATOM_cause,
                    cause,
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                );
            }
        }
    }

    if magic == JSErrorEnum::Aggregate as i32 {
        let error_list = iterator_to_array(ctx, *argv.offset(0));
        if error_list.is_exception() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        let _ = obj.define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_errors,
            error_list,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );
    }

    // skip the Error() function in the backtrace
    build_backtrace(
        ctx,
        obj,
        std::ptr::null_mut(),
        Some(0),
        JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL,
    );

    obj
}
