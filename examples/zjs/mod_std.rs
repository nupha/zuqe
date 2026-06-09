// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, process::ExitCode};

use zuqe::{
    CallContext, EvalFlags, EvalType, Exception, FieldDef, JS_PROP_ENUMERABLE,
    JS_SetUncatchableError, JSResult, JSValue, ScopedContext,
};

use crate::{
    class_file::{JSFile, js_file_puts},
    mod_os::OS_PENDING_SIGNALS,
};

fn js_std_open(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let filename = cc.arg(0).to_string(ctx)?;
    let mode = if cc.length() > 1 {
        cc.arg(1)
            .to_string(ctx)?
            .content()
            .encode_utf8(false)
            .to_string()
    } else {
        "r".to_string()
    };

    let mut opts = std::fs::OpenOptions::new();

    match mode.as_str() {
        "r" | "rb" | "rrr" | "" => {
            opts.read(true);
        }
        "w" | "wb" => {
            opts.write(true).truncate(true).create(true);
        }
        "a" | "ab" => {
            opts.write(true).append(true).create(true);
        }
        "r+" | "r+b" | "rb+" => {
            opts.read(true).write(true).create(true);
        }
        "w+" | "w+b" | "wb+" => {
            opts.read(true).write(true).truncate(true).create(true);
        }
        "a+" | "a+b" | "ab+" => {
            opts.read(true).write(true).append(true).create(true);
        }
        "rw" => {
            opts.read(true).write(true).create(true);
        }
        _ => {
            ctx.throw_type_error("invalid file mode");
            return Err(-1);
        }
    }

    let mut err_obj = cc.arg(2);
    let mut errno = 0;

    let fh = match opts.open(filename.content().encode_utf8(false).as_ref()) {
        Ok(fh) => Some(fh),
        Err(e) => {
            println!("[ERR] {e}");
            errno = e.raw_os_error().unwrap();
            None
        }
    };

    if !err_obj.is_undefined() {
        err_obj.set_property_str(ctx, "errno", JSValue::Int(errno))?;
    }

    if let Some(fh) = fh {
        JSFile::create_file_instance(ctx, fh).map(JSValue::Object)
    } else {
        Ok(JSValue::NULL)
    }
}

fn js_evalScript(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut backtrace_barrier = false;
    let mut is_async = false;

    if cc.length() >= 2 {
        let options_obj = cc.arg(1);

        is_async = options_obj.get_property_str(ctx, "async")?.to_bool()?;
        backtrace_barrier = options_obj
            .get_property_str(ctx, "backtrace_barrier")?
            .to_bool()?;
    }

    let str = cc.arg(0).to_string(ctx)?;

    thread_local! {
        static EVAL_SCRIPT_RECURSE: Cell<u32> = const { Cell::new(0) };
    }

    let lev = EVAL_SCRIPT_RECURSE.with(|s| {
        let n = s.get();
        s.set(n + 1);
        n + 1
    });

    if lev == 1 {
        /* install the interrupt handler */
        ctx.rt_mut().set_interrupt_handler(|_| {
            let s = OS_PENDING_SIGNALS.get();
            (s >> libc::SIGINT) & 1 != 0 // break if has SIGINT
        });
    }

    let mut flags = EvalFlags::empty();
    if backtrace_barrier {
        flags |= EvalFlags::BACKTRACE_BARRIER;
    }
    if is_async {
        flags |= EvalFlags::COMPILE_TO_ASYNC;
    }

    let ret = ctx.with_new_scope(|mut cx| {
        let res = cx.eval(
            str.content().encode_utf8_bytes(true).as_ref(),
            "<evalScript>",
            EvalType::Global,
            flags,
        );
        cx.return_with(res)
    });

    let lev = EVAL_SCRIPT_RECURSE.with(|s| {
        let n = s.get();
        s.set(n - 1);
        n - 1
    });
    if lev == 0 {
        /* remove the interrupt handler */
        ctx.rt_mut().unset_interrupt_handler();

        OS_PENDING_SIGNALS.with(|s| {
            let n = s.get() & !(1_u64 << libc::SIGINT);
            s.set(n);
        });

        if ret.is_err() {
            // convert the uncatchable "interrupted" error into a normal error
            // so that it can be caught by the REPL
            if let Exception::Error(v) = ctx.rt().exception() {
                JS_SetUncatchableError(v, false);
            }
        }
    }

    ret
}

fn js_gc(ctx: &mut ScopedContext, _: CallContext) -> JSResult {
    ctx.rt_mut().gc_request();
    Ok(JSValue::UNDEFINED)
}

fn std_exit(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let n = cc.arg(0).to_number(ctx)?.to_i32();
    ctx.rt_mut().interrupt(ExitCode::from(n as u8));
    Ok(JSValue::UNDEFINED)
}

const MOD_STD_ENTRIES: &[FieldDef] = &[
    FieldDef::FUNC("open", 2, js_std_open, 0),
    FieldDef::FUNC("puts", 1, js_file_puts, 0), // stdout
    FieldDef::FUNC("evalScript", 1, js_evalScript, 0),
    FieldDef::FUNC("gc", 0, js_gc, 0),
    FieldDef::FUNC("exit", 1, std_exit, 0),
    FieldDef::LAZY_VALUE(
        "in",
        |cx| {
            let obj = JSFile::create_stdin_instance(cx)?;
            Ok(JSValue::Object(obj))
        },
        JS_PROP_ENUMERABLE,
    ),
    FieldDef::LAZY_VALUE(
        "out",
        |cx| {
            let obj = JSFile::create_stdout_instance(cx)?;
            Ok(JSValue::Object(obj))
        },
        JS_PROP_ENUMERABLE,
    ),
    FieldDef::LAZY_VALUE(
        "err",
        |cx| {
            let obj = JSFile::create_stderr_instance(cx)?;
            Ok(JSValue::Object(obj))
        },
        JS_PROP_ENUMERABLE,
    ),
];

pub fn init_mod_std(ctx: &mut ScopedContext) -> Result<(), i32> {
    ctx.create_module("std", MOD_STD_ENTRIES)?;
    Ok(())
}
