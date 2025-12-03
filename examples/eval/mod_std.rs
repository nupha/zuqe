use zhuque::{
    context::{ContextHandle, ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::JS_PROP_C_W_E,
    value::JSValue,
};

use crate::class_file::JSFile;

fn js_std_open(ctx: ContextRef, cc: CallContext) -> JSResult {
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
            opts.write(true).create(true);
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
        JSFile::new_instance(&ctx, fh).map(JSValue::Object)
    } else {
        Ok(JSValue::NULL)
    }
}

fn std_exit(_: ContextRef, _: CallContext) -> JSResult {
    println!("*** exit called");
    Ok(JSValue::UNDEFINED)
}

static MOD_STD_ENTRIES: [FieldDef; 3] = [
    FieldDef::FUNCTION("open", 2, js_std_open, 0),
    FieldDef::FUNCTION("exit", 1, std_exit, 0),
    FieldDef::INT32("out", 9, JS_PROP_C_W_E),
];

pub fn init_mod_std(mut ctx: ContextHandle) -> Result<(), i32> {
    ctx.register_native_module("std", &MOD_STD_ENTRIES)?;
    Ok(())
}
