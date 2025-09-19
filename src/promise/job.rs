use crate::{
    context::JSContext, error::JSResult, function::CallContext, promise::Promise,
    runtime::JSRuntime, value::JSValue,
};

pub type JobHandleFn = fn(
    ctx: &JSContext,
    resolving: [JSValue; 2],
    handle: JSValue,
    is_reject: bool,
    arg: JSValue,
) -> JSResult;

#[repr(transparent)]
pub struct JobEntry(Box<dyn FnOnce() -> JSResult>);

impl JobEntry {
    pub fn run(self) -> JSResult {
        (self.0)()
    }
}

pub struct JSJobEntry {
    pub ctx: JSContext,
    pub callback: JobHandleFn,

    // arguments
    resolving: [JSValue; 2],
    handle: JSValue,
    is_reject: bool,
    arg: JSValue,
}

impl JSContext {
    /// Append a job to job queue.
    pub(super) fn queue_job(&self, run: impl FnOnce() -> JSResult + 'static) {
        self.rt_mut().job_list.push_back(JobEntry(Box::new(run)));
    }

    // /// Append a job to queue.
    // /// Note: `args` values are not consumed, args count must not greater than 5.
    // pub(super) fn queue_job(
    //     &self,
    //     callback: JobHandleFn,
    //     resolving: [JSValue; 2],
    //     handle: JSValue,
    //     is_reject: bool,
    //     arg: JSValue,
    // ) {
    //     self.rt_mut().job_list.push_back(Box::new(JSJobEntry {
    //         ctx: self.clone(),
    //         callback,
    //         resolving,
    //         handle,
    //         is_reject,
    //         arg,
    //     }));
    // }
}

// pub(super) fn promise_reaction_job(ctx: &JSContext, args: CallContext) -> JSResult {
//     debug_assert_eq!(args.length(), 5);

//     let handler = args.arg(2);
//     let mut is_reject = args.arg(3).to_bool()?;
//     let arg = args.arg(4);

//     let mut result = if handler.is_undefined() {
//         if is_reject {
//             ctx.throw(arg.clone());
//             JSValue::EXCEPTION
//         } else {
//             arg.clone()
//         }
//     } else {
//         handler
//             .call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(arg))
//             .unwrap_or(JSValue::EXCEPTION)
//     };

//     is_reject = result.is_exception();
//     if is_reject {
//         result = ctx.take_exception();
//     }

//     let func = args.arg(is_reject as usize);

//     // as an extension, we support undefined as value to avoid
//     // creating a dummy promise in the 'await' implementation of async functions
//     if !func.is_undefined() {
//         func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&result))
//     } else {
//         Ok(JSValue::UNDEFINED)
//     }
// }

pub(super) fn promise_reaction_job(
    ctx: &JSContext,
    resolving: [JSValue; 2],
    handler: JSValue,
    is_reject: bool,
    arg: JSValue,
) -> JSResult {
    let mut result = if handler.is_undefined() {
        if is_reject {
            ctx.throw(arg.clone());
            JSValue::EXCEPTION
        } else {
            arg.clone()
        }
    } else {
        handler
            .call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&arg))
            .unwrap_or(JSValue::EXCEPTION)
    };

    let is_reject = if result.is_exception() {
        result = ctx.take_exception();
        true
    } else {
        false
    };

    let func = &resolving[is_reject as usize];

    // as an extension, we support undefined as value to avoid
    // creating a dummy promise in the 'await' implementation of async functions
    if !func.is_undefined() {
        func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&result))
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub(super) fn js_promise_resolve_thenable_job(
    ctx: &JSContext,
    promise: Promise,
    thenable: JSValue,
    then: JSValue,
) -> JSResult {
    let rr = promise.create_resolving_functions(ctx)?;
    if let Ok(r) = then.call(ctx, &thenable, &rr) {
        Ok(r)
    } else {
        let exc = ctx.take_exception();
        rr[1].call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&exc))
    }
}

impl JSRuntime {
    /// take and execute first pending job in queue.
    /// this will only execute one job, keep rest jobs in queue.
    ///
    /// Returns:
    /// Some(ctx): the Context of the executed job.
    /// None: there is no pending job.
    /// Err: fail in execute job. ctx is the context of the executed job.
    pub fn execute_job_once(&mut self) -> Result<Option<()>, i32> {
        if let Some(job) = self.job_list.pop_front() {
            job.run()?;
            Ok(Some(()))

        //     let ctx = job.ctx;
        //     if (job.callback)(&ctx, job.resolving, job.handle, job.is_reject, job.arg).is_ok() {
        //         Ok(Some(ctx))
        //     } else {
        //         Err((ctx, -1))
        //     }
        } else {
            Ok(None) // no job in queue
        }
    }
}
