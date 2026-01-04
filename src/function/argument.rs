use crate::{
    JS_CLASS_OBJECT,
    atom::Atom,
    builtins::array::ArrayPayload,
    context::ContextImpl,
    error::JSResult,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_VARREF,
        JS_PROP_WRITABLE, JSProperty, ObjectPayload, Object,
    },
    runtime::JS_MAX_LOCAL_VARS,
    stack_frame::StackFrame,
    utils::unlikely,
    value::JSValue,
};

impl ContextImpl {
    pub(crate) fn build_rest(&self, first: usize, args: &[JSValue]) -> JSResult {
        let mut arr = self.create_array()?;
        if !args.is_empty() {
            for (i, v) in args.iter().enumerate().skip(first) {
                arr.define_property_value_u32(self, (i - first) as u32, *v, JS_PROP_C_W_E)?;
            }
        }
        Ok(arr.into())
    }
}

pub fn js_build_arguments(ctx: &ContextImpl, args: &[JSValue]) -> JSValue {
    let mut obj = if let Ok(o) = ctx.create_object_with_proto(
        ctx.class_proto(JS_CLASS_OBJECT),
        ObjectPayload::ARGUMENTS(ArrayPayload::default()),
    ) {
        o
    } else {
        return JSValue::EXCEPTION;
    };

    // add the length property (cannot fail)
    obj.add_own_property(
        ctx,
        Atom::CONST_length,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        JSProperty::Normal(JSValue::Int(args.len() as i32)),
    )
    .unwrap();

    if !args.is_empty() {
        // initialize fast array values
        if obj
            .with_fast_array_values_mut(|lst| -> Result<(), i32> {
                lst.try_reserve_exact(args.len()).map_err(|_| -1)?;
                lst.extend(args.iter().cloned());
                Ok(())
            })
            .unwrap()
            .is_err()
        {
            ctx.throw_out_of_memory();
            return JSValue::EXCEPTION;
        };
    }

    obj.define_property_value(
        ctx,
        Atom::CONST_Symbol_iterator,
        ctx.array_proto_values,
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )
    .unwrap();

    // LEGACY: add 'callee' property
    let throw_type_error = ctx.throw_type_error;
    obj.define_property(
        ctx,
        Atom::CONST_callee,
        JSValue::UNDEFINED,
        throw_type_error,
        throw_type_error,
        JS_PROP_HAS_GET | JS_PROP_HAS_SET,
    )
    .unwrap();

    obj.into()
}

/// legacy arguments object: add references to the function arguments
pub fn js_build_mapped_arguments(
    ctx: &ContextImpl,
    args: &[JSValue],
    sf: &mut StackFrame,
    arg_count: i32,
) -> Result<Object, i32> {
    let mut obj = ctx
        .create_object_with_proto(
            ctx.class_proto(JS_CLASS_OBJECT),
            ObjectPayload::MAPPED_ARGUMENTS,
        )
        .map_err(|_| -1)?;

    // add the `length` property
    obj.add_own_property(
        ctx,
        Atom::CONST_length,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        JSProperty::Normal(JSValue::Int(args.len() as i32)),
    )?;

    for i in 0..arg_count {
        let vref = sf.get_var_ref(ctx, i as _, true)?;
        obj.add_own_property(
            ctx,
            Atom::new_small_int(i as u32),
            JS_PROP_C_W_E | JS_PROP_VARREF,
            JSProperty::VarRef(vref),
        )?;
    }

    // the arguments not mapped to the arguments of the function can be normal properties
    for (i, a) in args.iter().enumerate().skip(arg_count as usize) {
        obj.define_property_value_u32(ctx, i as _, *a, JS_PROP_C_W_E)?;
    }

    obj.define_property_value(
        ctx,
        Atom::CONST_Symbol_iterator,
        ctx.array_proto_values,
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )?;

    // callee returns this function in non strict mode
    obj.define_property_value(
        ctx,
        Atom::CONST_callee,
        ctx.rt_ptr().current_stack_frame().unwrap().cur_func,
        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
    )?;

    Ok(obj)
}

// XXX: should use ValueArray
pub fn build_arg_list(ctx: &ContextImpl, array_arg: JSValue) -> Result<Box<[JSValue]>, i32> {
    if unlikely(!array_arg.is_object()) {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    } else {
        let len = array_arg.get_length_prop(ctx)? as u32;
        if len > JS_MAX_LOCAL_VARS as u32 {
            ctx.throw_internal_error("too many arguments");
            return Err(-1);
        }

        let mut tab = Vec::<JSValue>::new();
        tab.try_reserve_exact(len as usize).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        if unsafe { array_arg.as_object_unchecked() }
            .with_fast_array_values(|a| {
                if a.len() == len as usize {
                    // fast array: clone values
                    tab.extend(a.iter().cloned());
                }
            })
            .is_none()
        {
            // slow array
            for i in 0..len {
                tab.push(array_arg.get_property_u32(ctx, i)?);
            }
        }

        Ok(tab.into_boxed_slice())
    }
}
