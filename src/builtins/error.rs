use crate::{
    JS_CLASS_ERROR,
    atom::Atom,
    builtins::iterator::iterator_to_array,
    context::{ContextImpl, ContextPtr},
    error::{ErrorKind, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL, JSResult},
    function::{CallContext, JS_GetFunctionRealm},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, ObjectPayload, ObjectRef},
    value::JSValue,
};

pub fn js_error_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let mut new_target = *cc.this();
    if new_target.is_undefined() {
        new_target = ctx.current_function();
    }

    // use proper proto, ensure proto is an object
    let mut proto = new_target.get_property(ctx, Atom::CONST_prototype)?;
    if !proto.is_object() {
        let realm =
            JS_GetFunctionRealm(unsafe { ContextPtr::from_ref_unchecked(ctx) }, new_target)?;
        let magic = cc.magic();
        proto = if magic < 0 {
            realm.class_proto(JS_CLASS_ERROR).unwrap().into()
        } else {
            realm.native_error_proto[magic as usize]
        };
    }

    debug_assert!(proto.is_object());

    let mut obj = ctx
        .create_object_with_proto(
            Some(ObjectRef::try_from(proto).unwrap()),
            ObjectPayload::ERROR,
        )
        .map_err(|_| -1)?;

    let mut arg_index = if cc.magic() == ErrorKind::Aggregate as i32 {
        1
    } else {
        0
    };
    let message = cc.arg(arg_index);
    arg_index += 1;

    if !message.is_undefined() {
        let msg = message.to_string(ctx)?;
        obj.define_property_value(
            ctx,
            Atom::CONST_message,
            msg.into(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }

    if arg_index < cc.length() {
        let options = cc.arg(arg_index);

        if options.is_object() && options.has_property(ctx, Atom::CONST_cause)? {
            let cause = options.get_property(ctx, Atom::CONST_cause)?;
            obj.define_property_value(
                ctx,
                Atom::CONST_cause,
                cause,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;
        }
    }

    if cc.magic() == ErrorKind::Aggregate as i32 {
        let error_list = iterator_to_array(ctx, cc.arg(0))?;
        obj.define_property_value(
            ctx,
            Atom::CONST_errors,
            error_list,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }

    // skip the Error() function in the backtrace
    ctx.build_backtrace_to(obj.into(), None, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL);

    Ok(obj.into())
}

/// `errors` value is not consumed.
pub fn js_aggregate_error_constructor(ctx: &ContextImpl, errors: JSValue) -> JSValue {
    if let Ok(mut obj) = ctx.create_object_with_proto(
        ctx.native_error_proto[ErrorKind::Aggregate as usize]
            .as_object()
            .copied(),
        ObjectPayload::ERROR,
    ) {
        obj.define_property_value(
            ctx,
            Atom::CONST_errors,
            errors,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )
        .unwrap();
        obj.into()
    } else {
        JSValue::EXCEPTION
    }
}
