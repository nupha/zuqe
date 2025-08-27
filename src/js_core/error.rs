use crate::{
    JS_CLASS_ERROR,
    atom::consts::{JS_ATOM_cause, JS_ATOM_errors, JS_ATOM_message, JS_ATOM_prototype},
    context::{JS_GetActiveFunction, JSContext},
    error::{ErrorKind, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL, JSResult},
    function::{CallContext, JS_GetFunctionRealm},
    iterator::iterator_to_array,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, ObjectPayload},
    value::JSValue,
};

pub fn js_error_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut new_target = cc.this().clone();

    if new_target.is_undefined() {
        new_target = unsafe { JS_GetActiveFunction(ctx).clone() };
    }

    let mut proto = new_target.get_property(ctx, JS_ATOM_prototype)?;

    if !proto.is_object() {
        let realm = JS_GetFunctionRealm(ctx, &new_target)?;
        proto = realm.0.borrow().class_proto[if cc.magic() < 0 {
            JS_CLASS_ERROR as usize
        } else {
            cc.magic() as usize
        }]
        .clone();
    }

    let obj = ctx
        .create_object_from_proto(proto, ObjectPayload::ERROR)
        .map_err(|_| -1)?;

    let mut arg_index = if cc.magic() == ErrorKind::Aggregate as i32 {
        1
    } else {
        0
    };
    let message = cc.arg(arg_index as usize);
    arg_index += 1;

    if !message.is_undefined() {
        let msg = message.to_string(ctx)?;
        obj.define_property_value(
            ctx,
            JS_ATOM_message,
            msg.into(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }

    if arg_index < cc.length() {
        let options = cc.arg(arg_index as usize);
        if options.is_object() {
            if options.has_property(ctx, JS_ATOM_cause)? {
                let cause = options.get_property(ctx, JS_ATOM_cause)?;
                obj.define_property_value(
                    ctx,
                    JS_ATOM_cause,
                    cause,
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;
            }
        }
    }

    if cc.magic() == ErrorKind::Aggregate as i32 {
        let error_list = unsafe { iterator_to_array(ctx, cc.arg(0))? };
        obj.define_property_value(
            ctx,
            JS_ATOM_errors,
            error_list,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }

    // skip the Error() function in the backtrace
    ctx.build_backtrace_to(
        &obj.clone().into(),
        None,
        JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL,
    );

    Ok(obj.into())
}

/// `errors` value is not consumed.
pub fn js_aggregate_error_constructor(ctx: &JSContext, errors: JSValue) -> JSValue {
    if let Ok(obj) = ctx.create_object_from_proto(
        ctx.0.borrow().native_error_proto[ErrorKind::Aggregate as usize].clone(),
        ObjectPayload::ERROR,
    ) {
        obj.define_property_value(
            ctx,
            JS_ATOM_errors,
            errors,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )
        .unwrap();
        obj.into()
    } else {
        JSValue::EXCEPTION
    }
}
