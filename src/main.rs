use std::ops::DerefMut;

use zhuque::{
    context::JS_FreeContext, eval::JS_EVAL_TYPE_GLOBAL, mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    runtime::JSRuntime, value::JS_FreeValue,
};

fn main() {
    std::env::set_var("RUST_LOG", "INFO");
    env_logger::init();

    unsafe {
        if let Ok(mut js) = JSRuntime::new(JS_DEFAULT_MALLOC_FUNCTIONS, std::ptr::null_mut()) {
            println!("js runtime: {js:p}");

            let mut ctx = js.create_context().unwrap();

            let src = c"function g(a){return a+10} g('x')"; // ok
                                                            //  let src = c"function g(a){return a+10} g(20)"; // ok
                                                            //  let src = c"function g(a){return a+10}"; // ok
                                                            // let src = c"function g(){return 20}"; // ok
                                                            //let src = c"g = ()=>{return 20}"; // ok
                                                            // let src = c"g = function(){ }"; // ok
                                                            //let src = c"10/2"; // ok
                                                            // let src = c"a=2*10"; // ok
                                                            //                    // let src = c"function f(){}"; // ok

            // let src = cr#"
            // 10 // ok
            // a=10 // ok
            // b=a/5
            // //function f(){} // ok
            // function f(a,b){ return 'aa'} // fail
            // //function g(a) { 10} // fail
            // "#;

            let r = ctx.deref_mut().eval(
                src,
                "<test>",
                JS_EVAL_TYPE_GLOBAL, // | JS_EVAL_FLAG_COMPILE_ONLY,
            );
            println!("::::::::::::::: eval: {r:?}");
            js.as_mut().gc_dump_objects(1);
            // JS_DumpAtoms(js.as_mut());

            println!("================ free eval result");
            JS_FreeValue(ctx.deref_mut(), r);
            js.as_mut().gc_dump_objects(1);
            // JS_DumpAtoms(js.as_mut());

            JS_FreeContext(ctx.deref_mut());
            drop(js);

            println!("done");
        } else {
            println!("can't create runtime");
        }
    }
}
