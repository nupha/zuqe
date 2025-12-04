use zhuque::{
    context::Context,
    error::JSResult,
    function::{CallContext, FieldDef},
    value::JSValue,
};

fn js_os_now(_: CallContext) -> JSResult {
    let tm = unsafe {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    };

    Ok(JSValue::from(tm as f64 / 1e6))
}

static MOD_OS_ENTRIES: [FieldDef; 1] = [FieldDef::CALL("now", 0, js_os_now, 0)];

pub fn init_mod_os(mut ctx: Context) -> Result<(), i32> {
    ctx.register_module("os", &MOD_OS_ENTRIES)?;
    Ok(())
}

// pub struct ModuleOS;

// impl ModuleOS {
//     pub fn register(mut ctx: Context) -> Result<(), i32> {
//         ctx.register_native_module("os", &MOD_OS_ENTRIES)?;
//         Ok(())
//     }
// }
