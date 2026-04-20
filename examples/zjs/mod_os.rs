// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, LinkedList},
    ffi::c_void,
};

use gc_lite::{GcTrace, GcTraceCtx};
use smallvec::SmallVec;
use zuqe::{
    Array, CallContext, FieldDef, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JSResult, JSValue,
    ScopedContext,
};

// Declare windows crt functions
#[cfg(windows)]
unsafe extern "system" {
    fn _get_osfhandle(fd: i32) -> windows_sys::Win32::Foundation::HANDLE;
    fn _setmode(fd: i32, mode: i32) -> i32;
}

fn js_os_now(_: &mut ScopedContext, _: CallContext) -> JSResult {
    Ok(JSValue::from(get_time_ns() as f64 / 1e6))
}

#[inline]
fn get_time_ns() -> u64 {
    // std::time::SystemTime::now()
    //     .duration_since(std::time::UNIX_EPOCH)
    //     .map_or(0, |t| t.as_nanos() as u64)

    #[cfg(unix)]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    #[cfg(windows)]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |t| t.as_nanos() as u64)
    }
}

#[inline]
fn get_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_millis() as u64)
}

fn js_os_isatty(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let fd = cc.arg(0).to_number(ctx)?.to_i32();
    Ok(JSValue::Bool(unsafe { libc::isatty(fd) != 0 }))
}

fn js_os_ttyGetWinSize(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let fd = cc.arg(0).to_number(ctx)?.to_i32();
    let w: i32;
    let h: i32;

    #[cfg(unix)]
    {
        let mut sz = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut sz) == 0 }
            && sz.ws_col >= 4
            && sz.ws_row >= 4
        {
            w = sz.ws_col as i32;
            h = sz.ws_row as i32;
        } else {
            return Ok(JSValue::NULL);
        }
    }

    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{
            CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo,
        };

        let mut info = std::mem::zeroed::<CONSOLE_SCREEN_BUFFER_INFO>();
        if GetConsoleScreenBufferInfo(_get_osfhandle(fd), &mut info) != 0 {
            w = info.dwSize.X as i32;
            h = info.dwSize.Y as i32;
        } else {
            return Ok(JSValue::NULL);
        }
    }

    let mut obj = Array::create(ctx)?;
    obj.define_property_value_u32(ctx, 0, JSValue::Int(w), JS_PROP_C_W_E)?;
    obj.define_property_value_u32(ctx, 1, JSValue::Int(h), JS_PROP_C_W_E)?;
    Ok(obj.into())
}

fn js_os_ttySetRaw(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let fd = cc.arg(0).to_number(ctx)?.to_i32();

    #[cfg(unix)]
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

    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{
            ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT, ENABLE_WRAP_AT_EOL_OUTPUT,
            SetConsoleMode,
        };

        SetConsoleMode(
            _get_osfhandle(fd),
            ENABLE_WINDOW_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT,
        );
        _setmode(fd, libc::O_BINARY);

        if fd == 0 {
            SetConsoleMode(
                _get_osfhandle(1),
                ENABLE_PROCESSED_OUTPUT
                    | ENABLE_WRAP_AT_EOL_OUTPUT
                    | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            );
        }
    }

    Ok(JSValue::UNDEFINED)
}

struct Timer {
    timeout_ms: u64,
    func: JSValue,
}

thread_local! {
    pub(super) static OS_PENDING_SIGNALS: Cell<u64> = const { Cell::new(0) };
    static RW_HANDLERS: RefCell<HashMap<i32, [JSValue; 2]>> = RefCell::new(HashMap::new());
    static SIG_HANDLERS: RefCell<HashMap<u32, JSValue>> = RefCell::new(HashMap::new());

    static TIMERS: RefCell<LinkedList<Timer>> = const { RefCell::new(LinkedList::new()) };
}

fn js_os_poll(ctx: &mut ScopedContext) -> bool {
    let mut callbacks = SmallVec::<[JSValue; 16]>::new();

    if OS_PENDING_SIGNALS.get() != 0 {
        SIG_HANDLERS.with(|lut| {
            let b = lut.borrow();

            for (sig_num, func) in b.iter() {
                let mask = 1_u64 << *sig_num;
                let p = OS_PENDING_SIGNALS.get();
                if p & mask != 0 {
                    OS_PENDING_SIGNALS.set(p & !mask);
                    callbacks.push(*func);
                }
            }
        });

        for func in callbacks.drain(..) {
            if func.call(ctx, JSValue::UNDEFINED, &[]).is_err() {
                match ctx.rt().exception().to_string(ctx) {
                    Ok(s) => {
                        eprintln!("signal handle error: {}", s);
                    }
                    Err(_) => {
                        eprintln!("signal handle error: <failed to stringify exception>");
                    }
                }
            }
        }

        return true;
    }

    if RW_HANDLERS.with(|s| s.borrow().is_empty()) && TIMERS.with(|s| s.borrow().is_empty()) {
        return false; // no more events (read-write and timer)
    }

    // poll timers
    let min_tv = TIMERS.with_borrow_mut(|timers| {
        if !timers.is_empty() {
            let cur_time = get_time_ms() as i64;
            let mut min_delay = i64::MAX;

            let cnt = timers.len();
            let mut n = 0;
            while n < cnt {
                #[cfg(debug_assertions)]
                let mut tmr = timers.pop_front().unwrap();
                #[cfg(not(debug_assertions))]
                let mut tmr = unsafe { timers.pop_front().unwrap_unchecked() };

                let diff = tmr.timeout_ms as i64 - cur_time;
                if diff <= 0 {
                    let func = std::mem::replace(&mut tmr.func, JSValue::UNDEFINED);
                    if func.call(ctx, JSValue::UNDEFINED, &[]).is_err() {
                        match ctx.rt().exception().to_string(ctx) {
                            Ok(s) => {
                                eprintln!("Timer error: {}", s);
                            }
                            Err(_) => {
                                eprintln!("Timer error: <failed to stringify exception>");
                            }
                        }
                    }
                } else if diff < min_delay {
                    min_delay = diff;
                    timers.push_back(tmr);
                }
                n += 1;
            }

            Some(libc::timeval {
                tv_sec: (min_delay / 1000) as _,
                tv_usec: ((min_delay % 1000) * 1000) as _,
            })
        } else if ctx.rt().gc_has_request() {
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

    // poll read-write
    let has_rw = RW_HANDLERS.with(|rws| {
        if !rws.borrow().is_empty() {
            #[cfg(unix)]
            unsafe {
                // unix poll
                let mut rfds = std::mem::zeroed::<libc::fd_set>();
                let mut wfds = std::mem::zeroed::<libc::fd_set>();
                libc::FD_ZERO(&mut rfds);
                libc::FD_ZERO(&mut wfds);
                let mut fd_max = -1;

                {
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
                }

                let tvp = if let Some(t) = &min_tv {
                    t as *const _ as *mut _
                } else {
                    std::ptr::null_mut()
                };

                let ret = libc::select(fd_max + 1, &mut rfds, &mut wfds, std::ptr::null_mut(), tvp);
                if ret > 0 {
                    let b = rws.borrow();
                    for (fd, rh) in b.iter() {
                        if !rh[0].is_null() && libc::FD_ISSET(*fd, &rfds) {
                            callbacks.push(rh[0]);
                        }
                        if !rh[1].is_null() && libc::FD_ISSET(*fd, &wfds) {
                            callbacks.push(rh[1]);
                        }
                    }
                } else if ret == -1 {
                    let eno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    if eno == libc::EINTR {
                        // interrupt
                        return false;
                    } else {
                        eprintln!("select error: {}. ingored", eno);
                    }
                }
            }

            #[cfg(windows)]
            unsafe {
                use windows_sys::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};

                let mut handles = [std::ptr::null_mut(); 64];
                let mut count = 0_u32;
                {
                    let b = rws.borrow();
                    for (fd, _) in b.iter() {
                        if *fd == 0 {
                            // stdin only!
                            handles[count as usize] = _get_osfhandle(*fd);
                            count += 1;
                            if count == 64 {
                                break;
                            }
                        }
                    }
                }

                if count > 0 {
                    let timeout = min_tv.map_or(INFINITE, |t| t.tv_sec as u32 * 1000);
                    let ret = WaitForMultipleObjects(count, handles.as_ptr(), 0, timeout);
                    if ret < count {
                        for (fd, rh) in rws.borrow().iter() {
                            if *fd == 0 {
                                // stdin only!
                                callbacks.push(rh[0]);
                            }
                        }
                    }
                } else if let Some(t) = min_tv {
                    std::thread::sleep(std::time::Duration::from_millis(
                        t.tv_sec as u64 * 1000 + t.tv_usec as u64 / 1000,
                    ));
                }
            }

            true
        } else {
            false
        }
    });

    for f in callbacks {
        if f.call(ctx, JSValue::UNDEFINED, &[]).is_err() {
            // read handler error
            // eprintln!(
            //     "read handler error: {}",
            //     ctx.rt_ref().exception().to_string(ctx).unwrap()
            // );
            break;
        }
    }

    has_rw || min_tv.is_some() || OS_PENDING_SIGNALS.get() != 0
}

pub fn mod_os_gc_trace(tr: &mut GcTraceCtx) {
    RW_HANDLERS.with_borrow_mut(|s| {
        for v in s.values() {
            v[0].trace(tr);
            v[1].trace(tr);
        }
    });
    SIG_HANDLERS.with_borrow_mut(|s| {
        for v in s.values() {
            v.trace(tr);
        }
    });
    TIMERS.with_borrow_mut(|s| {
        for tm in s {
            tm.func.trace(tr);
        }
    });

    // RW_HANDLERS.with(|lut| {
    //     for v in lut.borrow().values() {
    //         v[0].trace(tr);
    //         v[1].trace(tr);
    //     }
    // });

    // SIG_HANDLERS.with_borrow(|lut| {
    //     for v in lut.values() {
    //         v.trace(tr);
    //     }
    // });

    // TIMERS.with_borrow(|tms| {
    //     for tm in tms {
    //         tm.func.trace(tr);
    //     }
    // })
}

fn js_os_setReadHandler(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic() as usize;

    let fd = cc.arg(0).to_number(ctx)?.to_i32();
    let func = cc.arg(1);

    if func.is_null() {
        RW_HANDLERS.with(|lut| {
            let mut b = lut.borrow_mut();
            if let Some(rh) = b.get_mut(&fd) {
                rh[magic] = JSValue::NULL;
                if rh[0].is_null() && rh[1].is_null() {
                    b.remove(&fd);
                }
            }
        });
        Ok(JSValue::UNDEFINED)
    } else if func.as_object_BCWrapper().is_some() {
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

fn js_os_read_write(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    let fd = cc.arg(0).to_number(ctx)?.to_i32();
    let pos = cc.arg(2).to_index(ctx)? as u64;
    let len = cc.arg(3).to_index(ctx)? as u64;

    let mut buf_obj = cc.arg(1);
    let buf = if let Some(ab) = buf_obj.as_object_ArrayBuffer_mut() {
        ab.payload_mut()
    } else {
        ctx.throw_type_error("expect ArrayBuffer or SharedArrayBuffer instance");
        return Err(-1);
    };
    let size = buf.byte_length();

    if pos + len > size as u64 {
        ctx.throw_range_error("read/write array buffer overflow");
        return Err(-1);
    }

    let r = buf.with_bytes_mut(|b| unsafe {
        let ptr = b.as_mut_ptr().add(pos as usize).cast::<c_void>();
        if magic == 0 {
            libc::read(fd, ptr, len as _)
        } else {
            libc::write(fd, ptr, len as _)
        }
    });

    let ret = if r == -1 {
        -std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    } else {
        r as i32
    };
    Ok(JSValue::from(ret))
}

fn js_os_signal(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let sig_num = cc.arg(0).to_number(ctx)?.to_u32();
    if sig_num >= 64 {
        ctx.throw_range_error("invalid signal number");
        return Err(-1);
    }

    let func = cc.arg(1);

    if func.is_undefined_or_null() {
        SIG_HANDLERS.with(|lut| lut.borrow_mut().remove(&sig_num));
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

        extern "C" fn os_signal_handler(sig_num: i32) {
            OS_PENDING_SIGNALS.set(OS_PENDING_SIGNALS.get() | (1_u64 << sig_num));
        }
        unsafe {
            libc::signal(sig_num as _, os_signal_handler as *const c_void as usize);
        }
    }

    Ok(JSValue::UNDEFINED)
}

const MOD_OS_ENTRIES: &[FieldDef] = &[
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
    #[cfg(not(windows))]
    FieldDef::INT32("SIGQUIT", libc::SIGQUIT, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGPIPE", libc::SIGPIPE, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGALRM", libc::SIGALRM, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGUSR1", libc::SIGUSR1, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGUSR2", libc::SIGUSR2, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGCHLD", libc::SIGCHLD, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGCONT", libc::SIGCONT, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGSTOP", libc::SIGSTOP, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGTSTP", libc::SIGTSTP, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGTTIN", libc::SIGTTIN, JS_PROP_CONFIGURABLE),
    #[cfg(not(windows))]
    FieldDef::INT32("SIGTTOU", libc::SIGTTOU, JS_PROP_CONFIGURABLE),
];

pub fn init_mod_os(ctx: &mut ScopedContext) -> Result<(), i32> {
    ctx.create_module("os", MOD_OS_ENTRIES)?;
    ctx.add_tick_handler(|cx| -> bool { js_os_poll(cx) });
    Ok(())
}
