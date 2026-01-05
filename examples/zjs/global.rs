use colorized::{Colors, colorize_print, colorize_println};
use zuqe::{CallContext, Callback, Context, JS_PROP_C_W_E, JSResult, JSValue};

fn js_print(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let level = cc.magic();
    let inspect = level == 100;

    let color = || {
        match level {
            1 => Colors::CyanFg, // debug
            2 => Colors::GreenFg,
            3 => Colors::YellowFg,
            4 => Colors::RedBg,
            _ => Colors::WhiteFg,
        }
    };

    for i in 0..cc.length() {
        if i > 0 {
            colorize_print(" ", color());
        }

        let arg = cc.arg(i);

        colorize_print(
            if inspect {
                arg.inspect(&ctx)?.to_string()
            } else {
                format!("{}", arg.to_string(&ctx)?)
            },
            color(),
        );
    }

    colorize_println("", color());
    Ok(JSValue::UNDEFINED)
}

pub fn setup_globals(ctx: Context) -> Result<(), i32> {
    let mut global = ctx.global_obj();
    let ctx = &ctx;

    global.set_property_str(
        ctx,
        "createRealm",
        ctx.create_native_function(
            Callback::Func(|cc: CallContext| {
                let ctx1 = cc.context().rt_mut().create_context_ecma()?;
                let glo = JSValue::Object(ctx1.global_obj());
                Ok(glo)
            }),
            "createRealm",
            1,
            0,
        )?
        .into(),
    )?;

    {
        let printfn: JSValue = ctx
            .create_native_function(Callback::Func(js_print), "print", 0, 0)
            .unwrap()
            .into();

        global.set_property_str(ctx, "print", printfn)?;

        let mut console = ctx.create_base_object().unwrap();
        console.define_property_value_str(ctx, "log", printfn, JS_PROP_C_W_E)?;
        console.define_property_value_str(
            ctx,
            "inspect",
            ctx.create_native_function(Callback::Func(js_print), "inspect", 0, 100)?
                .into(),
            JS_PROP_C_W_E,
        )?;
        global.define_property_value_str(ctx, "console", console.into(), JS_PROP_C_W_E)?;
    }

    // detachArrayBuffer
    global
        .set_property_str(
            ctx,
            "detachArrayBuffer",
            ctx.create_native_function(
                Callback::Func(|cc: CallContext| -> JSResult {
                    if let Some(mut ab) = cc.arg(0).as_object_ArrayBuffer().copied() {
                        ab.detatch(&cc.context());
                    }
                    Ok(JSValue::UNDEFINED)
                }),
                "detachArrayBuffer",
                1,
                0,
            )?
            .into(),
        )
        .unwrap();

    {
        let mut args = Vec::<JSValue>::with_capacity(std::env::args().count());
        for a in std::env::args().skip(1) {
            let s = ctx.create_string_from_utf8(a.into_bytes())?;
            args.push(s.into());
        }
        let arr = ctx.create_array_with_values(args)?;
        global.set_property_str(ctx, "scriptArgs", arr.into())?;
    }

    Ok(())
}
