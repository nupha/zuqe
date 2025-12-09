use std::cell::Cell;

use once_cell::sync::Lazy;
use zhuque::{
    context::Context,
    error::{JS_SetUncatchableError, JSResult},
    eval::{JS_EVAL_FLAG_ASYNC, JS_EVAL_FLAG_BACKTRACE_BARRIER, JS_EVAL_TYPE_GLOBAL},
    function::{CallContext, FieldDef},
    value::JSValue,
};

use crate::{
    class_file::{JSFile, js_file_puts},
    mod_os::OS_PENDING_SIGNALS,
};

fn js_std_open(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let filename = cc.arg(0).to_string(&ctx)?;
    let mode = if cc.length() > 1 {
        cc.arg(1)
            .to_string(&ctx)?
            .content()
            .to_utf8(false)
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

    let mut err_obj = *cc.arg(2);
    let mut errno = 0;

    let fh = match opts.open(filename.content().to_utf8(false).as_ref()) {
        Ok(fh) => Some(fh),
        Err(e) => {
            println!("[ERR] {e}");
            errno = e.raw_os_error().unwrap();
            None
        }
    };

    if !err_obj.is_undefined() {
        err_obj.set_property_str(&ctx, "errno", JSValue::Int(errno))?;
    }

    if let Some(fh) = fh {
        JSFile::new_instance(ctx, fh).map(JSValue::Object)
    } else {
        Ok(JSValue::NULL)
    }
}

fn js_evalScript(mut cc: CallContext) -> JSResult {
    let mut backtrace_barrier = false;
    let mut is_async = false;

    if cc.length() >= 2 {
        let options_obj = *cc.arg(1);

        is_async = options_obj.get_property_str(cc.ctx(), "async")?.to_bool()?;
        backtrace_barrier = options_obj
            .get_property_str(cc.ctx(), "backtrace_barrier")?
            .to_bool()?;
    }

    let str = cc.arg(0).to_string(cc.ctx())?;

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
        // JS_SetInterruptHandler(
        //     JS_GetRuntime(ctx),
        //     Some(interrupt_handler),
        //     std::ptr::null_mut(),
        // );
    }

    let mut flags = JS_EVAL_TYPE_GLOBAL;
    if backtrace_barrier {
        flags |= JS_EVAL_FLAG_BACKTRACE_BARRIER;
    }
    if is_async {
        flags |= JS_EVAL_FLAG_ASYNC;
    }

    let ret = cc.ctx_mut().eval(
        str.content().encode_utf8_bytes().as_ref(),
        "<evalScript>",
        flags,
    );
    let lev = EVAL_SCRIPT_RECURSE.with(|s| {
        let n = s.get();
        s.set(n - 1);
        n - 1
    });

    if lev == 0 {
        /* remove the interrupt handler */
        // JS_SetInterruptHandler(ctx.rt_mut(), None, std::ptr::null_mut());

        OS_PENDING_SIGNALS.with(|s| {
            let n = s.get() & !(1_u64 << libc::SIGINT);
            s.set(n);
        });

        /* convert the uncatchable "interrupted" error into a normal error
        so that it can be caught by the REPL */
        if ret.is_err() {
            JS_SetUncatchableError(cc.ctx().rt_ref().exception(), false);
        }
    }

    ret
}

fn std_exit(_: CallContext) -> JSResult {
    println!("*** exit called");
    Ok(JSValue::UNDEFINED)
}

static MOD_STD_ENTRIES: Lazy<Box<[FieldDef]>> = Lazy::new(|| {
    let lst = vec![
        FieldDef::CALL("open", 2, js_std_open, 0),
        FieldDef::CALL("puts", 1, js_file_puts, 0), // stdout
        FieldDef::CALL("evalScript", 1, js_evalScript, 0),
        FieldDef::CALL("exit", 1, std_exit, 0),
        FieldDef::BUILD(
            "in",
            |cx| {
                let obj = JSFile::new_instance_stdin(cx)?;
                Ok(JSValue::Object(obj))
            },
            0,
        ),
        FieldDef::BUILD(
            "out",
            |cx| {
                let obj = JSFile::new_instance_stdout(cx)?;
                Ok(JSValue::Object(obj))
            },
            0,
        ),
        FieldDef::BUILD(
            "err",
            |cx| {
                let obj = JSFile::new_instance_stderr(cx)?;
                Ok(JSValue::Object(obj))
            },
            0,
        ),
    ];
    lst.into()
});

pub fn init_mod_std(mut ctx: Context) -> Result<(), i32> {
    ctx.register_module("std", &MOD_STD_ENTRIES)?;
    Ok(())
}
