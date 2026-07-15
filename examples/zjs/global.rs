// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ops::Deref;

use colorized::{Colors, colorize_print};
use zuqe::{Array, CallContext, Callback, JS_PROP_C_W_E, JSResult, JSValue, ScopedContext};

fn js_print(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let level = cc.magic();
    let inspect = true;

    let color = || {
        match level {
            1 => Colors::CyanFg,   // debug
            2 => Colors::GreenFg,  // info
            3 => Colors::YellowFg, // warn
            4 => Colors::RedFg,    // error
            _ => Colors::Reset,
        }
    };

    for i in 0..cc.length() {
        if i > 0 {
            colorize_print(" ", color());
        }

        let arg = cc.arg(i);
        colorize_print(
            if inspect {
                arg.inspect(ctx)?.to_string()
            } else {
                format!("{}", arg.to_string(ctx)?)
            },
            color(),
        );
    }

    println!();
    Ok(JSValue::UNDEFINED)
}

pub fn setup_globals(ctx: &mut ScopedContext, script_args: &[String]) -> Result<(), i32> {
    let mut global = ctx.global_obj();

    {
        let printfn = ctx.create_native_function(Callback::Func(js_print), "print", 0, 0)?;
        global.define_property_value_str(ctx, "print", printfn.into(), JS_PROP_C_W_E)?;

        let mut console = ctx.create_base_object()?;
        console.define_property_value_str(ctx, "log", printfn.into(), JS_PROP_C_W_E)?;

        let debug_func = ctx.create_native_function(Callback::Func(js_print), "debug", 0, 1)?;
        console.define_property_value_str(ctx, "debug", debug_func.into(), JS_PROP_C_W_E)?;

        let info_func = ctx.create_native_function(Callback::Func(js_print), "info", 0, 2)?;
        console.define_property_value_str(ctx, "info", info_func.into(), JS_PROP_C_W_E)?;

        let warn_func = ctx.create_native_function(Callback::Func(js_print), "warn", 0, 3)?;
        console.define_property_value_str(ctx, "warn", warn_func.into(), JS_PROP_C_W_E)?;

        let error_func = ctx.create_native_function(Callback::Func(js_print), "error", 0, 4)?;
        console.define_property_value_str(ctx, "error", error_func.into(), JS_PROP_C_W_E)?;

        global.define_property_value_str(ctx, "console", console.into(), JS_PROP_C_W_E)?;
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
        let arr = Array::from_values(ctx, args)?;
        global.set_property_str(ctx, "scriptArgs", arr.into())?;
    }

    Ok(())
}
