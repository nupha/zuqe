use zhuque::{
    JS_CFUNC_NATIVE_DEF, JS_PROP_INT32_DEF,
    context::ContextRef,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::JS_PROP_C_W_E,
    value::JSValue,
};

use crate::class_file::class_file_new_instance;

fn js_std_open(ctx: ContextRef, cc: CallContext) -> JSResult {
    let filename = cc.arg(0).to_string(&ctx)?;
    let mode = cc.arg(1).to_string(&ctx)?;
    let mode_str = mode.content().to_utf8(false);

    let mut opts = std::fs::OpenOptions::new();

    match mode_str.as_ref() {
        "r" | "" => {
            opts.read(true);
        }
        "w" => {
            opts.write(true);
        }
        "a" => {
            opts.append(true);
        }
        "r+" => {
            opts.read(true).write(true);
        }
        "w+" => {
            opts.read(true).write(true).truncate(true);
        }
        "a+" => {
            opts.read(true).write(true).append(true);
        }
        "rb" => {
            opts.read(true);
        }
        "wb" => {
            opts.write(true);
        }
        "ab" => {
            opts.append(true);
        }
        "r+b" | "rb+" => {
            opts.read(true).write(true);
        }
        "w+b" | "wb+" => {
            opts.read(true).write(true).truncate(true);
        }
        "a+b" | "ab+" => {
            opts.read(true).write(true).append(true);
        }
        "rw" => {
            opts.read(true).write(true);
        }
        "rrr" => {
            opts.read(true);
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
        class_file_new_instance(&ctx, fh).map(JSValue::Object)
    } else {
        Ok(JSValue::NULL)
    }
}

fn std_exit(ctx: ContextRef, cc: CallContext) -> JSResult {
    println!("*** exit called");
    Ok(JSValue::UNDEFINED)
}

static MOD_STD_ENTRIES: [FieldDef; 3] = [
    JS_CFUNC_NATIVE_DEF!("open", 2, js_std_open, 0),
    JS_CFUNC_NATIVE_DEF!("exit", 1, std_exit, 0),
    JS_PROP_INT32_DEF!("out", 9, JS_PROP_C_W_E),
];

pub fn init_mod_std(mut ctx: ContextRef) -> Result<(), i32> {
    ctx.register_native_module("std", &MOD_STD_ENTRIES)?;
    Ok(())
}
