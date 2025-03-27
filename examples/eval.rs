use std::ffi::{CStr, CString};

use zhuque::{
    context::JS_FreeContext,
    eval::{JS_Eval, JS_EVAL_TYPE_GLOBAL},
    mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    runtime::JSRuntime,
    value::{JSValue, JS_FreeValue, JS_IsException, JS_VALUE_GET_OBJ},
};

fn main() {
    std::env::set_var("RUST_LOG", "INFO");
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() == 1 {
        println!("argument required");
        std::process::exit(-1);
    }

    let filename = args.get(1).unwrap().clone();
    let script = std::fs::read_to_string(&filename).unwrap();
    let src = format!("{script}\0"); // add tailing '\0'

    unsafe {
        if let Ok(mut js) = JSRuntime::new(JS_DEFAULT_MALLOC_FUNCTIONS, std::ptr::null_mut()) {
            let ctx = js.create_context().unwrap();

            let fname = CString::new(filename).unwrap();

            let r = JS_Eval(
                ctx,
                src.as_ptr().cast(),
                src.len() - 1,
                CStr::from_ptr(fname.as_ptr()),
                JS_EVAL_TYPE_GLOBAL, // | JS_EVAL_FLAG_COMPILE_ONLY,
            );

            if JS_IsException(r) {
                let exc = js.current_exception;
                js.current_exception = JSValue::NULL;
                println!("!!! EXCEPTION {exc:?}");

                (*JS_VALUE_GET_OBJ(exc)).dump(js.as_ref());
                JS_FreeValue(ctx, exc);
            } else {
                println!(">>> RESULT: {r:?}");
                r.dump((*ctx).rt());
                println!();
                JS_FreeValue(ctx, r);
            }

            // JS_DumpGCObjects(js.as_mut());
            // JS_DumpAtoms(js.as_mut());

            // println!("================ free eval result");
            // JS_DumpGCObjects(js.as_mut());
            // JS_DumpAtoms(js.as_mut());

            JS_FreeContext(ctx.as_mut().unwrap());
            drop(js);
        } else {
            println!("can't create runtime");
        }
    }
}
