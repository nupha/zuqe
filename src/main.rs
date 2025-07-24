use zhuque::{eval::JS_EVAL_TYPE_GLOBAL, mem::JS_DEFAULT_MALLOC_FUNCTIONS, runtime::JSRuntime};

fn main() {
    unsafe {
        std::env::set_var("RUST_LOG", "INFO");
    }
    env_logger::init();

    if let Ok(mut js) = JSRuntime::new(JS_DEFAULT_MALLOC_FUNCTIONS, std::ptr::null_mut()) {
        println!("js runtime: {js:p}");

        let ctx = js
            .create_context_customize(|c| {
                c.init_core_objects_ex(false, false, false);
                c.intrinsic_global_object(&[]);
                Ok(())
            })
            .unwrap();

        let src = "(x / y) = 1;";
        //  let src = "function g(a){return a+10} g(20)"; // ok
        //  let src = "function g(a){return a+10}"; // ok
        // let src = "function g(){return 20}"; // ok
        //let src = "g = ()=>{return 20}"; // ok
        // let src = "g = function(){ }"; // ok
        //let src = "10/2"; // ok
        // let src = "a=2*10"; // ok
        //                    // let src = c"function f(){}"; // ok

        // let src = cr#"
        // 10 // ok
        // a=10 // ok
        // b=a/5
        // //function f(){} // ok
        // function f(a,b){ return 'aa'} // fail
        // //function g(a) { 10} // fail
        // "#;

        if false {
            let r = ctx.eval(src.as_bytes(), "<test>", JS_EVAL_TYPE_GLOBAL);
            println!("::::::::::::::: eval: {r:?}");
            js.as_mut().gc_dump_objects(1);
        }

        // JS_DumpAtoms(js.as_mut());

        println!("================ free eval result");
        js.as_mut().gc_dump_objects(1);
        // JS_DumpAtoms(js.as_mut());

        drop(ctx);
        drop(js);

        println!("done");
    } else {
        println!("can't create runtime");
    }
}
