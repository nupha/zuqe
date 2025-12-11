use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, LinkedList},
    ffi::c_void,
};

use gc_lite::{GcTracable, GcTracer};
use once_cell::sync::Lazy;
use smallvec::SmallVec;
use zhuque::{
    context::Context,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE},
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

fn js_os_isatty(cc: CallContext) -> JSResult {
    let fd = cc.arg(0).to_i32(cc.ctx())?;
    Ok(JSValue::Bool(unsafe { libc::isatty(fd) != 0 }))
}

fn js_os_ttyGetWinSize(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let fd = cc.arg(0).to_i32(ctx)?;
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 }
        && ws.ws_col >= 4
        && ws.ws_row >= 4
    {
        let mut obj = ctx.create_array()?;
        obj.define_property_value_u32(ctx, 0, JSValue::Int(ws.ws_col as i32), JS_PROP_C_W_E)?;
        obj.define_property_value_u32(ctx, 1, JSValue::Int(ws.ws_row as i32), JS_PROP_C_W_E)?;
        Ok(obj.into())
    } else {
        Ok(JSValue::NULL)
    }
}

fn js_os_ttySetRaw(cc: CallContext) -> JSResult {
    let fd = cc.arg(0).to_i32(cc.ctx())?;

    unsafe {
        static mut OLD_TTY: libc::termios = unsafe { std::mem::zeroed::<libc::termios>() };

        let mut tty = std::mem::zeroed::<libc::termios>();
        libc::tcgetattr(fd, &mut tty);
        OLD_TTY = tty;

        tty.c_iflag &= !(libc::IGNBRK
            | libc::BRKINT
            | libc::PARMRK
            | libc::ISTRIP
            | libc::INLCR
            | libc::IGNCR
            | libc::ICRNL
            | libc::IXON);
        tty.c_oflag |= libc::OPOST;
        tty.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::IEXTEN);
        tty.c_cflag &= !(libc::CSIZE | libc::PARENB);
        tty.c_cflag |= libc::CS8;
        tty.c_cc[libc::VMIN] = 1;
        tty.c_cc[libc::VTIME] = 0;

        libc::tcsetattr(fd, libc::TCSANOW, &tty);

        extern "C" fn term_exit() {
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &raw const OLD_TTY);
            }
        }
        libc::atexit(term_exit);
    }

    Ok(JSValue::UNDEFINED)
}

struct Timer {
    timeout: u64,
    func: JSValue,
}

thread_local! {
    pub(super) static OS_PENDING_SIGNALS: Cell<u64> = const { Cell::new(0) };
    static RW_HANDLERS: RefCell<HashMap<i32, [JSValue; 2]>> = RefCell::new(HashMap::new());
    static SIG_HANDLERS: RefCell<HashMap<u32, JSValue>> = RefCell::new(HashMap::new());

    static TIMERS: RefCell<LinkedList<Timer>> = const { RefCell::new(LinkedList::new()) };
}

fn get_time_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    ts.tv_sec as u64 * 1000 + (ts.tv_nsec as u64 / 1000000)
}

fn js_os_poll(ctx: Context) -> bool {
    let ctx = &ctx;

    let need_next_poll = {
        // only check signals in the main thread
        if OS_PENDING_SIGNALS.get() != 0 {
            let ret = SIG_HANDLERS.with_borrow(|lut| {
                for (sig_num, func) in lut {
                    let mask = 1_u64 << *sig_num;
                    let p = OS_PENDING_SIGNALS.get();
                    if p & mask != 0 {
                        OS_PENDING_SIGNALS.set(p & !mask);
                        if func.call(ctx, JSValue::UNDEFINED, &[]).is_err() {
                            eprintln!(
                                "signal {sig_num} handle error: {:?}",
                                ctx.rt_ref().exception().to_string(ctx)
                            );
                        }
                        return Some(true);
                    }
                }
                None
            });
            if let Some(b) = ret {
                return b;
            }
        }

        if RW_HANDLERS.with_borrow(|s| s.is_empty()) && TIMERS.with_borrow(|s| s.is_empty()) {
            return false; // no more events
        }

        let min_tv = TIMERS.with_borrow_mut(|timers| {
            if !timers.is_empty() {
                let cur_time = get_time_ms();
                let mut min_delay = 10000;
                let mut cnt = timers.len();

                while let Some(mut tm) = timers.pop_front()
                    && cnt > 0
                {
                    let delay = tm.timeout as i64 - cur_time as i64;
                    if delay <= 0 {
                        let func = std::mem::replace(&mut tm.func, JSValue::UNDEFINED);
                        if func.call(ctx, JSValue::UNDEFINED, &[]).is_err() {
                            eprintln!("Timer error: {:?}", ctx.rt_ref().exception().to_string(ctx));
                        }
                    } else if delay < min_delay {
                        min_delay = delay;
                        timers.push_back(tm);
                    }

                    cnt += 1;
                }

                let tv = libc::timeval {
                    tv_sec: (min_delay / 1000) as _,
                    tv_usec: ((min_delay % 1000) * 1000) as _,
                };
                Some(tv)
            } else if ctx.rt_ref().gc_has_request() {
                // non-block poll
                Some(libc::timeval {
                    tv_sec: 0,
                    tv_usec: 0,
                })
            } else {
                // block poll
                None
            }
        });

        let tvp = if let Some(t) = &min_tv {
            t as *const _ as *mut _
        } else {
            std::ptr::null_mut()
        };

        let poll_fd = RW_HANDLERS.with(|rws| unsafe {
            if !rws.borrow().is_empty() {
                let mut rfds = std::mem::zeroed::<libc::fd_set>();
                let mut wfds = std::mem::zeroed::<libc::fd_set>();
                libc::FD_ZERO(&mut rfds);
                libc::FD_ZERO(&mut wfds);
                let mut fd_max = -1;

                let b = rws.borrow();
                for (fd, rh) in b.iter() {
                    fd_max = std::cmp::max(fd_max, *fd);
                    if !rh[0].is_null() {
                        libc::FD_SET(*fd, &mut rfds);
                    }
                    if !rh[1].is_null() {
                        libc::FD_SET(*fd, &mut wfds);
                    }
                }

                let ret = libc::select(fd_max + 1, &mut rfds, &mut wfds, std::ptr::null_mut(), tvp);
                if ret > 0 {
                    let mut callbacks = SmallVec::<[JSValue; 8]>::new();
                    let b = rws.borrow();

                    for (fd, rh) in b.iter() {
                        if !rh[0].is_null() && libc::FD_ISSET(*fd, &rfds) {
                            callbacks.push(rh[0]);
                        }
                        if !rh[1].is_null() && libc::FD_ISSET(*fd, &wfds) {
                            callbacks.push(rh[1]);
                        }
                    }

                    for f in callbacks {
                        if f.call(ctx, JSValue::UNDEFINED, &[]).is_err() {
                            eprintln!(
                                "read handler error: {}",
                                ctx.rt_ref().exception().to_string(ctx).unwrap()
                            );
                            break;
                        }
                    }
                } else if ret == -1 {
                    let eno = *libc::__error();
                    if eno == libc::EINTR {
                        // interrupt
                        return false;
                    } else {
                        eprintln!("select error: {}. ingored", eno);
                    }
                }

                true
            } else {
                false
            }
        });

        poll_fd || min_tv.is_some()
    };

    need_next_poll
}

pub fn mod_os_gc_trace(tracer: &mut GcTracer) {
    RW_HANDLERS.with(|lut| {
        for v in lut.borrow().values() {
            v[0].trace(tracer);
            v[1].trace(tracer);
        }
    });

    SIG_HANDLERS.with_borrow(|lut| {
        for v in lut.values() {
            v.trace(tracer);
        }
    });

    TIMERS.with_borrow(|tms| {
        for tm in tms {
            tm.func.trace(tracer);
        }
    })
}

fn js_os_setReadHandler(cc: CallContext) -> JSResult {
    let magic = cc.magic() as usize;
    let ctx = cc.ctx();

    let fd = cc.arg(0).to_i32(ctx)?;
    let func = *cc.arg(1);

    if func.is_null() {
        // FIXME: this may cause borrow conflict
        RW_HANDLERS.with(|lut| {
            let mut b = lut.borrow_mut();
            let prune = if let Some(rh) = b.get_mut(&fd) {
                rh[magic] = JSValue::NULL;
                rh[0].is_null() && rh[1].is_null()
            } else {
                false
            };

            if prune {
                RW_HANDLERS.with(|lut| {
                    lut.borrow_mut().remove(&fd);
                });
            }
        });
        Ok(JSValue::UNDEFINED)
    } else if func.as_object_BCFunction().is_some() {
        // FIXME: this may cause borrow conflict
        RW_HANDLERS.with(|lut| {
            let mut b = lut.borrow_mut();
            if let Some(rh) = b.get_mut(&fd) {
                rh[magic] = func;
            } else {
                let mut rh = [JSValue::NULL, JSValue::NULL];
                rh[magic] = func;
                b.insert(fd, rh);
            }
        });
        Ok(JSValue::UNDEFINED)
    } else {
        ctx.throw_type_error("not a function");
        Err(-1)
    }
}

fn js_os_read_write(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let ctx = cc.ctx();

    let fd = cc.arg(0).to_i32(ctx)?;

    let mut buf_obj = *cc.arg(1);
    let buf = if let Some(ab) = buf_obj.as_object_ArrayBuffer_mut() {
        ab.payload_mut()
    } else {
        ctx.throw_type_error("expect ArrayBuffer or SharedArrayBuffer instance");
        return Err(-1);
    };
    let size = buf.byte_length();

    let pos = cc.arg(2).to_index(ctx)? as u64;
    let len = cc.arg(3).to_index(ctx)? as u64;

    if pos + len > size as u64 {
        ctx.throw_range_error("read/write array buffer overflow");
        return Err(-1);
    }

    let r = unsafe {
        buf.with_bytes_mut(|b| {
            let ptr = b.as_mut_ptr().add(pos as usize).cast::<c_void>();
            if magic == 0 {
                libc::read(fd, ptr, len as usize)
            } else {
                libc::write(fd, ptr, len as usize)
            }
        })
    };

    let ret = if r == -1 {
        unsafe { -*libc::__error() as isize }
    } else {
        r
    };
    Ok(JSValue::from(ret as i64))
}

fn js_os_signal(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let sig_num = cc.arg(0).to_u32(ctx)?;
    if sig_num >= 64 {
        ctx.throw_range_error("invalid signal number");
        return Err(-1);
    }

    // let sh: *mut JSOSSignalHandler;
    // let handler: sighandler_t;
    // if !is_main_thread(rt) {
    //     return JS_ThrowTypeError(
    //         ctx,
    //         b"signal handler can only be set in the main thread\0".as_ptr() as *const i8,
    //     );
    // }

    let func = *cc.arg(1);
    if func.is_undefined_or_null() {
        SIG_HANDLERS.with_borrow_mut(|lut| lut.remove(&sig_num));
        unsafe {
            libc::signal(
                sig_num as _,
                if func.is_null() {
                    libc::SIG_DFL
                } else {
                    libc::SIG_IGN
                },
            );
        }
    } else {
        if !func.is_function() {
            ctx.throw_type_error("not a function");
            return Err(-1);
        }
        SIG_HANDLERS.with_borrow_mut(|lut| {
            lut.insert(sig_num, func);
        });

        fn os_signal_handler(sig_num: i32) {
            OS_PENDING_SIGNALS.set(OS_PENDING_SIGNALS.get() | (1_u64 << sig_num));
        }

        unsafe {
            libc::signal(sig_num as _, os_signal_handler as *const c_void as usize);
        }
    }

    Ok(JSValue::UNDEFINED)
}

static MOD_OS_ENTRIES: Lazy<Box<[FieldDef]>> = Lazy::new(|| {
    let lst = vec![
        FieldDef::CALL("isatty", 1, js_os_isatty, 0),
        FieldDef::CALL("ttyGetWinSize", 1, js_os_ttyGetWinSize, 0),
        FieldDef::CALL("ttySetRaw", 1, js_os_ttySetRaw, 0),
        FieldDef::CALL("setReadHandler", 2, js_os_setReadHandler, 0),
        FieldDef::CALL("setWriteHandler", 2, js_os_setReadHandler, 1),
        FieldDef::CALL("read", 4, js_os_read_write, 0),
        FieldDef::CALL("write", 4, js_os_read_write, 1),
        FieldDef::CALL("signal", 2, js_os_signal, 0),
        FieldDef::CALL("now", 0, js_os_now, 0),
        FieldDef::INT32("SIGINT", libc::SIGINT, JS_PROP_CONFIGURABLE),
        FieldDef::INT32("SIGABRT", libc::SIGABRT, JS_PROP_CONFIGURABLE),
        FieldDef::INT32("SIGFPE", libc::SIGFPE, JS_PROP_CONFIGURABLE),
        FieldDef::INT32("SIGILL", libc::SIGILL, JS_PROP_CONFIGURABLE),
        FieldDef::INT32("SIGSEGV", libc::SIGSEGV, JS_PROP_CONFIGURABLE),
        FieldDef::INT32("SIGTERM", libc::SIGTERM, JS_PROP_CONFIGURABLE),
    ];

    lst.into()
});

pub fn init_mod_os(mut ctx: Context) -> Result<(), i32> {
    ctx.register_module("os", &MOD_OS_ENTRIES)?;
    ctx.add_tick_handler(|cx| -> bool { js_os_poll(cx) });
    Ok(())
}
