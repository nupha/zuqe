use crate::{
    atom::consts::{JS_ATOM_cause, JS_ATOM_errors, JS_ATOM_message, JS_ATOM_prototype},
    context::{JSContext, JS_GetActiveFunction},
    error::{build_backtrace, JSErrorEnum, JSResult, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL},
    function::{CallArgs, JS_GetFunctionRealm},
    iterator::iterator_to_array,
    value::{
        object::{JS_NewObjectProtoClass, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
        JSValue, JS_FreeValue, JS_ToString,
    },
    JS_CLASS_ERROR,
};

pub fn js_error_constructor(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let mut new_target = args.this;

    if new_target.is_undefined() {
        new_target = unsafe { JS_GetActiveFunction(ctx) };
    }

    let mut proto = new_target
        .get_property(ctx, JS_ATOM_prototype)
        .check_exception()?;

    if !proto.is_object() {
        unsafe {
            JS_FreeValue(ctx, proto);
        }

        let realm = unsafe { JS_GetFunctionRealm(ctx, new_target) };
        if realm.is_null() {
            return Err(-1);
        }
        let realm = unsafe { realm.as_mut().unwrap() };

        let proto1 = if args.magic < 0 {
            realm.class_proto[JS_CLASS_ERROR as usize]
        } else {
            realm.native_error_proto[args.magic as usize]
        };

        proto = proto1.dup();
    }

    let obj = unsafe { JS_NewObjectProtoClass(ctx, proto, JS_CLASS_ERROR) };
    unsafe {
        JS_FreeValue(ctx, proto);
    }
    let obj = obj.check_exception()?;

    let mut arg_index = if args.magic == JSErrorEnum::Aggregate as i32 {
        1
    } else {
        0
    };
    let message = args.get_raw(arg_index as usize);
    arg_index += 1;

    if !message.is_undefined() {
        let msg = unsafe { JS_ToString(ctx, message) };
        if msg.is_exception() {
            unsafe {
                JS_FreeValue(ctx, obj);
            }
            return Err(-1);
        }

        let _ = obj.define_property_value(
            ctx,
            JS_ATOM_message,
            msg,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );
    }

    if arg_index < args.length() {
        let options = args.get_raw(arg_index as usize);
        if options.is_object() {
            let present = if let Ok(b) = options.has_property(ctx, JS_ATOM_cause) {
                b
            } else {
                unsafe {
                    JS_FreeValue(ctx, obj);
                }
                return Err(-1);
            };

            if present {
                let cause = options.get_property(ctx, JS_ATOM_cause);
                if cause.is_exception() {
                    unsafe {
                        JS_FreeValue(ctx, obj);
                    }
                    return Err(-1);
                }

                let _ = obj.define_property_value(
                    ctx,
                    JS_ATOM_cause,
                    cause,
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                );
            }
        }
    }

    if args.magic == JSErrorEnum::Aggregate as i32 {
        let error_list = unsafe { iterator_to_array(ctx, args.get_raw(0)) };
        if error_list.is_exception() {
            unsafe {
                JS_FreeValue(ctx, obj);
            }
            return Err(-1);
        }

        let _ = obj.define_property_value(
            ctx,
            JS_ATOM_errors,
            error_list,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        );
    }

    // skip the Error() function in the backtrace
    unsafe {
        build_backtrace(
            ctx,
            obj,
            std::ptr::null_mut(),
            Some(0),
            JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL,
        )
    };

    Ok(obj)
}

/// `errors` value is not consumed.
pub fn js_aggregate_error_constructor(ctx: &mut JSContext, errors: JSValue) -> JSValue {
    let obj = unsafe {
        JS_NewObjectProtoClass(
            ctx,
            ctx.native_error_proto[JSErrorEnum::Aggregate as usize],
            JS_CLASS_ERROR,
        )
    };

    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    obj.define_property_value(
        ctx,
        JS_ATOM_errors,
        errors.dup(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    );

    obj
}
