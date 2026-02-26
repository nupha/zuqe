// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ops::Deref;

use colorized::{Colors, colorize_print, colorize_println};
use zuqe::{CallContext, Callback, Context, JS_PROP_C_W_E, JSResult, JSValue};

fn js_print(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let level = cc.magic();
    let inspect = true;

    let color = || {
        match level {
            1 => Colors::CyanFg,   // debug
            2 => Colors::GreenFg,  // info
            3 => Colors::YellowFg, // warn
            4 => Colors::RedFg,    // error
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
                format!("{}", arg.to_string(&ctx)?.deref())
            },
            color(),
        );
    }

    colorize_println("", color());
    Ok(JSValue::UNDEFINED)
}

pub fn setup_globals(ctx: Context, script_args: &[String]) -> Result<(), i32> {
    let mut global = ctx.global_obj();
    let ctx = &ctx;

    {
        let printfn: JSValue = ctx
            .create_native_function(Callback::Func(js_print), "print", 0, 0)
            .unwrap()
            .into();

        let mut console = ctx.create_base_object().unwrap();
        console.define_property_value_str(ctx, "log", printfn, JS_PROP_C_W_E)?;
        console.define_property_value_str(
            ctx,
            "debug",
            ctx.create_native_function(Callback::Func(js_print), "inspect", 0, 1)?
                .into(),
            JS_PROP_C_W_E,
        )?;
        console.define_property_value_str(
            ctx,
            "info",
            ctx.create_native_function(Callback::Func(js_print), "inspect", 0, 2)?
                .into(),
            JS_PROP_C_W_E,
        )?;
        console.define_property_value_str(
            ctx,
            "warn",
            ctx.create_native_function(Callback::Func(js_print), "inspect", 0, 3)?
                .into(),
            JS_PROP_C_W_E,
        )?;
        console.define_property_value_str(
            ctx,
            "error",
            ctx.create_native_function(Callback::Func(js_print), "inspect", 0, 4)?
                .into(),
            JS_PROP_C_W_E,
        )?;
        global.define_property_value_str(ctx, "console", console.into(), JS_PROP_C_W_E)?;

        global.set_property_str(ctx, "print", printfn)?;
    }

    {
        let mut args = Vec::<JSValue>::with_capacity(script_args.len() + 1);
        args.push(
            ctx.create_string_from_utf8(
                std::ffi::OsString::from("zjs")
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes(),
            )?
            .into(),
        );
        for a in script_args {
            let s = ctx.create_string_from_utf8(a.as_bytes().to_vec())?;
            args.push(s.into());
        }
        let arr = ctx.create_array_with_values(args)?;
        global.set_property_str(ctx, "scriptArgs", arr.into())?;
    }

    // for debug
    // global.set_property_str(
    //     ctx,
    //     "createRealm",
    //     ctx.create_native_function(
    //         Callback::Func(|cc: CallContext| {
    //             let ctx1 = cc.context().rt_mut().create_context_ecma()?;
    //             let glo = JSValue::Object(ctx1.global_obj());
    //             Ok(glo)
    //         }),
    //         "createRealm",
    //         1,
    //         0,
    //     )?
    //     .into(),
    // )?;
    // // detachArrayBuffer
    // global
    //     .set_property_str(
    //         ctx,
    //         "detachArrayBuffer",
    //         ctx.create_native_function(
    //             Callback::Func(|cc: CallContext| -> JSResult {
    //                 if let Some(mut ab) = cc.arg(0).as_object_ArrayBuffer().copied() {
    //                     ab.detatch(&cc.context());
    //                 }
    //                 Ok(JSValue::UNDEFINED)
    //             }),
    //             "detachArrayBuffer",
    //             1,
    //             0,
    //         )?
    //         .into(),
    //     )
    //     .unwrap();

    Ok(())
}
