// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use colorized::{Colors, colorize_print};
use zuqe::{Array, CallContext, Callback, JS_PROP_C_W_E, JSResult, JSValue, ScopedContext};

#[cfg(feature = "persistent")]
use zuqe::builtins::typed_array::TypedArray;

/// `dumpValue(val)` — serialize `val` into a self-contained bytecode blob and
/// return it as a `Uint8Array`. The blob can be reloaded in any Runtime via
/// `restoreValue` (cross-runtime safe: primitives, objects/arrays, functions,
/// generators, modules). Throws when the value cannot be persisted.
#[cfg(feature = "persistent")]
fn js_dump_value(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let blob = match JSValue::dump(ctx, cc.arg(0)) {
        Ok(b) => b,
        Err(e) => return Err(ctx.throw_type_error(&format!("dumpValue: {e}"))),
    };
    let ta = TypedArray::create_from_vec_u8(ctx, blob.to_vec())
        .map_err(|_| ctx.throw_type_error("dumpValue: cannot allocate Uint8Array"))?;
    Ok(ta.into())
}

/// `restoreValue(bytes)` — rebuild the value previously produced by
/// `dumpValue` from a `Uint8Array` and return it. Throws when `bytes` is not a
/// `Uint8Array`, the blob is malformed/truncated, or a referenced value cannot
/// be reconstructed (e.g. a native function whose name is not exposed on this
/// runtime's global object).
#[cfg(feature = "persistent")]
fn js_restore_value(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let ta = match TypedArray::try_from(cc.arg(0)) {
        Ok(t) => t,
        Err(_) => return Err(ctx.throw_type_error("restoreValue: argument must be a Uint8Array")),
    };
    let buf = ta.bytes().to_vec();
    JSValue::restore(ctx, &buf).map_err(|e| ctx.throw_type_error(&format!("restoreValue: {e}")))
}

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
                // Node console.log 行为：顶层字符串参数裸打印，其余走 util.inspect
                match arg.as_string(false) {
                    Some(s) => s.encode_utf8(false).into_owned(),
                    None => crate::inspect::js_inspect_value(
                        ctx,
                        arg,
                        &crate::inspect::InspectOptions::default(),
                    ),
                }
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

    #[cfg(feature = "persistent")]
    {
        let dump_fn =
            ctx.create_native_function(Callback::Func(js_dump_value), "dumpValue", 1, 0)?;
        global.define_property_value_str(ctx, "dumpValue", dump_fn.into(), JS_PROP_C_W_E)?;

        let restore_fn =
            ctx.create_native_function(Callback::Func(js_restore_value), "restoreValue", 1, 0)?;
        global.define_property_value_str(ctx, "restoreValue", restore_fn.into(), JS_PROP_C_W_E)?;
    }

    {
        let mut args = Vec::<JSValue>::with_capacity(script_args.len() + 1);
        args.push(
            ctx.create_string_from_utf8(
                std::ffi::OsString::from("zjs")
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes(),
            )
            .map(JSValue::make_string)?,
        );
        for a in script_args {
            let s = ctx.create_string_from_utf8(a.as_bytes().to_vec())?;
            args.push(JSValue::make_string(s));
        }
        let arr = Array::from_values(ctx, args)?;
        global.set_property_str(ctx, "scriptArgs", arr.into())?;
    }

    Ok(())
}
