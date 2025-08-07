use crate::{
    atom::{JS_ATOM_NULL, JSAtom},
    bytecode::OpcodeFormat,
    context::{JS_MODE_ASYNC, JS_MODE_STRICT, JSContext},
    function::{
        Bytecode, JSClosureVar, JSFunctionKindEnum, JSVarDef, JSVarKindEnum, Label, PC2LINE_BASE,
        PC2LINE_OP_FIRST, PC2LINE_RANGE,
    },
    unicode::unicode_from_utf8,
    utils::read_from_ptr,
    value::JSValue,
};

use super::{
    OP_COUNT_FINAL, OP_COUNT_INTERMEDIATE, opcode_info_final, opcode_info_intermediate, opcodes,
};

/// dump one opcode and its params at given pc.
#[cfg(feature = "dump_bytecode")]
pub fn dump_opcode_one(ctx: &JSContext, b: &Bytecode, pc: usize) -> usize {
    let mut pos = pc;
    let tab = b.opcodes();
    let op = tab[pos];

    if op >= OP_COUNT_FINAL {
        println!("invalid opcode (0x{:02x})", op);
        return 0;
    }

    let oi = opcode_info_final(op);
    let size = oi.size as usize;
    if pos + size > tab.len() {
        println!("truncated opcode (0x{:02x})", op);
        return 0;
    }

    // print opcode name
    print!("{}", oi.name);
    pos += 1;

    // print arguments
    match oi.fmt {
        #[cfg(feature = "short_opcode")]
        "none_int" => {
            print!(" {}", op - opcodes::OP_push_0);
        }
        #[cfg(feature = "short_opcode")]
        "npopx" => {
            print!(" {}", op - opcodes::OP_call0);
        }
        OpcodeFormat::U8 => {
            print!(" {}", tab[pos]);
        }
        OpcodeFormat::I8 => {
            print!(" {}", tab[pos] as i8);
        }
        OpcodeFormat::U16 | OpcodeFormat::NPOP => {
            print!(" {}", unsafe {
                read_from_ptr::<u16>(tab.as_ptr().offset(pos as isize))
            });
        }
        OpcodeFormat::NPOP_U16 => {
            print!(
                " {},{}",
                unsafe { read_from_ptr::<u16>(tab.as_ptr().offset(pos as _)) },
                unsafe { read_from_ptr::<u16>(tab.as_ptr().offset(pos as isize + 2)) },
            );
        }
        OpcodeFormat::I16 => {
            print!(" {}", unsafe {
                read_from_ptr::<i16>(tab.as_ptr().offset(pos as _))
            });
        }
        OpcodeFormat::I32 => {
            print!(" {}", unsafe {
                read_from_ptr::<i32>(tab.as_ptr().offset(pos as _))
            });
        }
        OpcodeFormat::U32 => {
            print!(" {}", unsafe {
                read_from_ptr::<u32>(tab.as_ptr().offset(pos as _))
            });
        }

        #[cfg(feature = "short_opcode")]
        OpcodeFormat::LABEL8 => {
            addr = get_i8(tab.as_ptr().offset(pos));
            has_addr1();
        }
        #[cfg(feature = "short_opcode")]
        OpcodeFormat::LABEL16 => {
            addr = get_i16(tab.as_ptr().offset(pos));
            has_addr1!();
        }

        f @ (OpcodeFormat::LABEL | OpcodeFormat::LABEL_U16) => {
            // addr = get_u32(tab.as_ptr().add(pos)) as _;
            // has_addr1!();
            if f == OpcodeFormat::LABEL_U16 {
                print!(", {}", unsafe {
                    read_from_ptr::<u16>(tab.as_ptr().add(pos + 4))
                });
            }
        }

        #[cfg(feature = "short_opcode")]
        OpcodeFormat::CONST8 => {
            idx = get_u8(tab.offset(pos));
            printf(" {}: ", idx);
            if idx < cpool_count as i32 {
                (*cpool.offset(idx as isize)).dump(ctx.rt());
            }
        }

        OpcodeFormat::CONST => {
            let idx = unsafe { read_from_ptr::<u32>(tab.as_ptr().add(pos)) };
            print!(" {idx}:{}", b.load_const(idx as usize).unwrap().repr());
        }
        OpcodeFormat::ATOM => {
            let a = unsafe { read_from_ptr::<u32>(tab.as_ptr().add(pos)) };
            JSAtom(a).with_utf8(ctx.rt(), |s| {
                print!(" {a}'{s}");
            });
        }
        f @ (OpcodeFormat::ATOM_U8 | OpcodeFormat::ATOM_U16) => {
            let atom = JSAtom(unsafe { read_from_ptr::<u32>(tab.as_ptr().add(pos)) });
            atom.with_utf8(ctx.rt(), |s| {
                print!(
                    " {}'{s}, {}",
                    atom.0,
                    if f == OpcodeFormat::ATOM_U8 {
                        tab[pos as usize + 4] as usize
                    } else {
                        unsafe { read_from_ptr::<u16>(tab.as_ptr().add(pos + 4)) as usize }
                    }
                );
            });
        }
        f @ (OpcodeFormat::ATOM_LABEL_U8 | OpcodeFormat::ATOM_LABEL_U16) => {
            let atom = JSAtom(unsafe { read_from_ptr::<u32>(tab.as_ptr().add(pos)) });
            atom.with_utf8(ctx.rt(), |s| {
                print!(" {}'{s}", atom.0);
            });

            // addr = get_u32(tab.as_ptr().offset(pos as isize + 4)) as _;
            // if pass == 1 {
            //     print!(", {}:{}", addr, (*label_slots.offset(addr as isize)).pos);
            // } else if pass == 2 {
            //     print!(", {}:{}", addr, (*label_slots.offset(addr as isize)).pos2);
            // } else if pass == 3 {
            //     print!(", {}", addr + pos + 4);
            // }

            if f == OpcodeFormat::ATOM_LABEL_U8 {
                print!(", {}", unsafe { *tab.as_ptr().add(pos + 8) });
            } else {
                print!(", {}", unsafe {
                    read_from_ptr::<u16>(tab.as_ptr().add(pos + 8))
                });
            }
        }

        #[cfg(feature = "short_opcode")]
        OpcodeFormat::NONE_LOC => {
            idx = (op - OP_get_loc0) % 4;
            printf(" {}: ", idx);
            if idx < var_count {
                print_atom(ctx.rt, (*vars.offset(idx as isize)).var_name);
            }
        }

        f @ (OpcodeFormat::LOC8 | OpcodeFormat::LOC) => {
            let idx = if f == OpcodeFormat::LOC8 {
                unsafe { *tab.as_ptr().add(pos) as usize }
            } else {
                unsafe { read_from_ptr::<u16>(tab.as_ptr().add(pos)) as usize }
            };

            if let Some(a) = b.with_var(idx as _, |v| v.var_name) {
                a.with_utf8(ctx.rt(), |s| {
                    print!(" {idx}:{s}");
                });
            } else {
                print!(" {idx}:?");
            }
        }

        #[cfg(feature = "short_opcode")]
        OpcodeFormat::NONE_ARG => {
            idx = (op - opcodes::OP_get_arg0) % 4;
            printf(" {}: ", idx);
            if idx < arg_count {
                print_atom(ctx.rt, (*args.offset(idx as isize)).var_name);
            }
        }

        OpcodeFormat::ARG => {
            let idx = unsafe { read_from_ptr::<u16>(tab.as_ptr().add(pos)) as usize };
            if let Some(a) = b.with_arg(idx as _, |vi| vi.var_name) {
                a.with_utf8(ctx.rt(), |s| {
                    print!(" {idx}:{s}");
                });
            } else {
                print!(" {idx}:?");
            }
        }

        #[cfg(feature = "short_opcode")]
        OpcodeFormat::NONE_VAR_REF => {
            idx = (op - opcodes::OP_get_var_ref0) % 4;
            printf(" {}: ", idx);
            if idx < closure_var_count {
                print_atom(ctx, (*closure_var.offset(idx as isize)).var_name);
            }
        }

        OpcodeFormat::VAR_REF => {
            let idx = unsafe { read_from_ptr::<u16>(tab.as_ptr().add(pos)) as usize };
            b.with_closure_var(idx as _, |v| v.var_name)
                .unwrap()
                .with_utf8(ctx.rt(), |s| {
                    print!(" {idx}:{s}");
                });
        }

        _ => {}
    }

    size
}

/// dump opcodes binary in pass 1,2,3
#[cfg(feature = "dump_bytecode")]
pub fn dump_opcodes(
    ctx: &JSContext,
    pass: i32,
    bin: &[u8],
    args: &[JSVarDef],
    vars: &[JSVarDef],
    closure_vars: &[JSClosureVar],
    cpools: &[JSValue],
    source: Option<&str>,
    line_num: i32,
    labels: &[Label], // for pass 1, 2 only
    bc: Option<&Bytecode>,
) {
    use crate::{bytecode::OpcodeFormat, utils::read_from_ptr};

    let len = bin.len();
    let use_short_opcodes = bc.is_some();
    let op_max = if use_short_opcodes {
        OP_COUNT_FINAL
    } else {
        OP_COUNT_INTERMEDIATE
    };

    let mut pos = 0_usize;
    let mut pos_next: usize;

    // flag to control whether print addr as label.
    let mut bits = vec![0_u8; len];

    // scan for jump targets
    while pos < len {
        let op = bin[pos as usize];
        let oi = if use_short_opcodes {
            opcode_info_final(op)
        } else {
            opcode_info_intermediate(op)
        };
        pos_next = pos + oi.size as usize;

        if op < op_max {
            match oi.fmt {
                #[cfg(feature = "short_opcode")]
                "label8" => {
                    pos += 1;
                    addr = *opcodes.offset(pos) as i32;
                    if pass == 1 {
                        addr = (*labels.offset(addr as isize)).pos;
                    }
                    if pass == 2 {
                        addr = (*labels.offset(addr as isize)).pos2;
                    }
                    if pass == 3 {
                        addr += pos;
                    }
                    if addr >= 0 && addr < len {
                        *bits.offset(addr as isize) |= 1;
                    }
                }
                #[cfg(feature = "short_opcode")]
                "label16" => {
                    pos += 1;
                    let addr = read_from_ptr::<u16>(opcodes.add(pos)) as i32;
                    if pass == 1 {
                        addr = labels[addr as usize].pos;
                    } else if pass == 2 {
                        addr = labels[addr as usize].pos2;
                    } else if pass == 3 {
                        addr += pos;
                    }
                    if addr >= 0 && addr < len {
                        *bits.offset(addr as isize) |= 1;
                    }
                }

                f @ (OpcodeFormat::ATOM_LABEL_U8
                | OpcodeFormat::ATOM_LABEL_U16
                | OpcodeFormat::LABEL
                | OpcodeFormat::LABEL_U16) => {
                    if matches!(
                        f,
                        OpcodeFormat::ATOM_LABEL_U8 | OpcodeFormat::ATOM_LABEL_U16
                    ) {
                        pos += 4;
                    }
                    pos += 1;
                    let addr = unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos as _)) };

                    let pc = match pass {
                        1 => labels[addr as usize].pos,
                        2 => labels[addr as usize].pos2,
                        3 => addr as i32 + pos as i32,
                        _ => {
                            unreachable!()
                        }
                    };
                    if pc >= 0 && pc < bin.len() as i32 {
                        *bits.get_mut(pc as usize).unwrap() |= 1;
                    }
                }
                _ => {}
            }
        }

        pos = pos_next;
    }

    macro_rules! has_addr1 {
        ($addr:expr) => {
            if pass == 1 {
                print!(" {}:{}", $addr, labels[$addr as usize].pos);
            } else if pass == 2 {
                print!(" {}:{}", $addr, labels[$addr as usize].pos2);
            } else if pass == 3 {
                print!(" {}", $addr as i32 + pos as i32);
            }
        };
    }

    let mut in_source = false;
    if let Some(s) = source {
        // Always print first line: needed if single line
        print_lines(s, 0, 1);
        in_source = true;
    }

    //let mut size: i32;
    let mut line1 = 1;
    let mut line = 1;

    pos = 0;
    while pos < len {
        let op = bin[pos as usize];

        // print source code
        {
            if let Some(s) = source {
                if let Some(b) = bc {
                    line1 = b.find_line_num(pos as _).unwrap_or(-1) - line_num + 1;
                } else if op == opcodes::op_line_num {
                    line1 = unsafe {
                        read_from_ptr::<i32>(bin.as_ptr().add(pos as usize + 1)) - line_num + 1
                    };
                };

                if line1 > line {
                    if !in_source {
                        println!();
                    }
                    in_source = true;

                    print_lines(s, line as usize, line1 as usize);
                    line = line1;
                    // bits[pos as usize] |= 2;
                }
            }
            if in_source {
                println!();
            }
            in_source = false;
        }

        if op >= op_max {
            println!("invalid opcode (0x{:02x})", op);
            pos += 1;
            continue;
        }
        let oi = if use_short_opcodes {
            opcode_info_final(op)
        } else {
            opcode_info_intermediate(op)
        };

        let op_len = (*oi).size as usize;
        if pos + op_len > len {
            println!("truncated opcode (0x{:02x})", op);
            break;
        }

        // print bytecode in hex
        {
            let mut x = 0;
            print!("{pos:5} ");
            let x0 = 6;

            for i in 0..op_len {
                if i == 6 {
                    print!("\n      ");
                    x = 0;
                }
                print!(" {:02X}", bin[(pos + i) as usize]);
                x += 3;
            }
            print!("{:width$}", " ", width = x0 + 20 - x);
        }

        // print address
        if bits[pos as usize] != 0 {
            print!("{pos:5}:  ");
        } else {
            print!("        ");
        }

        // print opcode name
        print!("{}", (*oi).name);
        pos += 1;

        // print arguments
        match (*oi).fmt {
            #[cfg(feature = "short_opcode")]
            OpcodeFormat::NONE_INT => {
                print!(" {}", op - opcodes::OP_push_0);
            }
            #[cfg(feature = "short_opcode")]
            OpcodeFormat::NPOPX => {
                print!(" {}", op - opcodes::OP_call0);
            }
            OpcodeFormat::U8 => {
                print!(" {}", bin[pos as usize]);
            }
            OpcodeFormat::I8 => {
                print!(" {}", bin[pos as usize] as i8);
            }
            OpcodeFormat::U16 | OpcodeFormat::NPOP => {
                print!(" {}", unsafe {
                    read_from_ptr::<u16>(bin.as_ptr().add(pos))
                });
            }
            OpcodeFormat::NPOP_U16 => unsafe {
                print!(
                    " {},{}",
                    read_from_ptr::<u16>(bin.as_ptr().add(pos)),
                    read_from_ptr::<u16>(bin.as_ptr().add(pos + 2)),
                );
            },
            OpcodeFormat::I16 => {
                print!(" {}", unsafe {
                    read_from_ptr::<i16>(bin.as_ptr().offset(pos as _))
                });
            }
            OpcodeFormat::I32 => {
                print!(" {}", unsafe {
                    read_from_ptr::<i32>(bin.as_ptr().offset(pos as _))
                });
            }
            OpcodeFormat::U32 => {
                print!(" {}", unsafe {
                    read_from_ptr::<u32>(bin.as_ptr().offset(pos as _))
                });
            }

            #[cfg(feature = "short_opcode")]
            OpcodeFormat::LABEL8 => {
                let addr = get_i8(opcodes.as_ptr().offset(pos));
                has_addr1(addr);
            }
            #[cfg(feature = "short_opcode")]
            OpcodeFormat::LABEL16 => {
                let addr = get_i16(opcodes.as_ptr().offset(pos));
                has_addr1!(addr);
            }

            f @ (OpcodeFormat::LABEL | OpcodeFormat::LABEL_U16) => {
                let addr = unsafe { read_from_ptr::<u32>(bin.as_ptr().offset(pos as _)) };
                has_addr1!(addr);

                if f == OpcodeFormat::LABEL_U16 {
                    print!(", {}", unsafe {
                        read_from_ptr::<u16>(bin.as_ptr().add(pos + 4))
                    });
                }
            }

            #[cfg(feature = "short_opcode")]
            OpcodeFormat::CONST8 => {
                idx = get_u8(opcodes.offset(pos));
                // C2RS: goto has_pool_idx replaced with inline logic
                printf(" {}: ", idx);
                if idx < cpool_count as i32 {
                    (*cpool.offset(idx as isize)).dump(ctx.rt());
                }
            }

            OpcodeFormat::CONST => {
                let idx = unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos)) as usize };
                print!(" {idx}:{}", cpools[idx]);
            }
            OpcodeFormat::ATOM => {
                let a = unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos)) };
                JSAtom(a).with_utf8(ctx.rt(), |s| {
                    print!(" {a}:{s}");
                });
            }
            f @ (OpcodeFormat::ATOM_U8 | OpcodeFormat::ATOM_U16) => {
                let atom = JSAtom(unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos)) });
                atom.with_utf8(ctx.rt(), |s| {
                    print!(
                        " {s}, {}",
                        if f == OpcodeFormat::ATOM_U8 {
                            bin[pos as usize + 4] as usize
                        } else {
                            unsafe { read_from_ptr::<u16>(bin.as_ptr().add(pos + 4)) as usize }
                        }
                    );
                });
            }
            f @ (OpcodeFormat::ATOM_LABEL_U8 | OpcodeFormat::ATOM_LABEL_U16) => {
                let atom = JSAtom(unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos as _)) });
                atom.with_utf8(ctx.rt(), |s| {
                    print!(" {s}");
                });

                let addr = unsafe { read_from_ptr::<u32>(bin.as_ptr().offset(pos as isize + 4)) };
                if pass == 1 {
                    print!(", {}:{}", addr, labels[addr as usize].pos);
                } else if pass == 2 {
                    print!(", {}:{}", addr, labels[addr as usize].pos2);
                } else if pass == 3 {
                    print!(", {}", addr as i32 + pos as i32 + 4);
                }

                if f == OpcodeFormat::ATOM_LABEL_U8 {
                    print!(", {}", unsafe { *bin.as_ptr().add(pos + 8) });
                } else {
                    print!(", {}", unsafe {
                        read_from_ptr::<u16>(bin.as_ptr().add(pos + 8))
                    });
                }
            }

            #[cfg(feature = "short_opcode")]
            OpcodeFormat::NONE_LOC => {
                idx = (op - OP_get_loc0) % 4;
                printf(" {}: ", idx);
                if idx < var_count {
                    print_atom(ctx.rt, (*vars.offset(idx as isize)).var_name);
                }
            }

            f @ (OpcodeFormat::LOC8 | OpcodeFormat::LOC) => {
                let idx = if f == OpcodeFormat::LOC8 {
                    unsafe { *bin.as_ptr().add(pos) as usize }
                } else {
                    unsafe { read_from_ptr::<u16>(bin.as_ptr().add(pos)) as usize }
                };
                print!(
                    " {idx}:{}",
                    vars.get(idx).unwrap().var_name.to_utf8(ctx.rt())
                );
            }

            #[cfg(feature = "short_opcode")]
            OpcodeFormat::NONE_ARG => {
                idx = (op - opcodes::OP_get_arg0) % 4;
                printf(" {}: ", idx);
                if idx < arg_count {
                    print_atom(ctx.rt, (*args.offset(idx as isize)).var_name);
                }
            }

            OpcodeFormat::ARG => {
                let idx = unsafe { read_from_ptr::<u16>(bin.as_ptr().add(pos)) as usize };
                print!(
                    " {idx}:{}",
                    args.get(idx).unwrap().var_name.to_utf8(ctx.rt())
                );
            }

            #[cfg(feature = "short_opcode")]
            OpcodeFormat::NONE_VAR_REF => {
                idx = (op - opcodes::OP_get_var_ref0) % 4;
                printf(" {}: ", idx);
                if idx < closure_var_count {
                    print_atom(ctx, (*closure_var.offset(idx as isize)).var_name);
                }
            }

            OpcodeFormat::VAR_REF => {
                let idx = unsafe { read_from_ptr::<u16>(bin.as_ptr().add(pos)) as usize };
                print!(
                    " {idx}:{}",
                    closure_vars.get(idx).unwrap().var_name.to_utf8(ctx.rt())
                );
            }

            _ => {}
        }
        println!();

        pos += op_len - 1;
    }

    if let Some(s) = source {
        if !in_source {
            println!();
        }
        print_lines(s, line as _, usize::MAX);
    }
}

impl Bytecode {
    /// dump Bytecode
    #[cfg(feature = "dump_bytecode")]
    pub fn dump(&self, ctx: &JSContext) {
        print!("{:?}: ", self);

        let b = self.0.borrow();

        if b.has_debug && b.debug.filename != JS_ATOM_NULL {
            print!(
                "{}:{}: ",
                b.debug.filename.to_utf8(ctx.rt()),
                b.debug.line_num
            );
        }

        println!(
            "function: {}{}",
            if b.func_kind == JSFunctionKindEnum::JS_FUNC_GENERATOR {
                "*"
            } else {
                ""
            },
            b.func_name.to_utf8(ctx.rt()),
        );

        if b.js_mode != 0 {
            print!("  mode:");
            if b.js_mode & JS_MODE_STRICT as u8 != 0 {
                print!(" strict");
            }
            if b.js_mode & JS_MODE_ASYNC as u8 != 0 {
                print!(" async");
            }
            println!();
        }

        {
            let args = b.args_slice();
            if !args.is_empty() {
                print!("  args:");
                for arg in args {
                    print!(" {}", arg.var_name.to_utf8(ctx.rt()));
                }
                println!();
            }
        }

        let vars = b.vars_slice();
        if !vars.is_empty() {
            println!("  locals:");
            for (i, vd) in vars.iter().enumerate() {
                let kind_str = match vd.var_kind() {
                    JSVarKindEnum::JS_VAR_CATCH => "catch",
                    JSVarKindEnum::JS_VAR_FUNCTION_DECL
                    | JSVarKindEnum::JS_VAR_NEW_FUNCTION_DECL => "function",
                    _ => {
                        if vd.is_const() {
                            "const"
                        } else if vd.is_lexical() {
                            "let"
                        } else {
                            "var"
                        }
                    }
                };

                print!("{i:5}: {kind_str} {}", vd.var_name.to_utf8(ctx.rt()),);

                if vd.scope_level != 0 {
                    print!(" [level:{} next:{}]", vd.scope_level, vd.scope_next);
                }
                println!();
            }
        }

        let cvars = b.closure_vars_slice();
        if !cvars.is_empty() {
            println!("  closure vars:");
            for (i, cv) in cvars.iter().enumerate() {
                let var_type = if cv.is_local() { "local" } else { "parent" };
                let loc_type = if cv.is_arg() { "arg" } else { "loc" };
                let const_str = if cv.is_const() {
                    "const"
                } else if cv.is_lexical() {
                    "let"
                } else {
                    "var"
                };

                println!(
                    "{i:5}: {} {var_type}:{loc_type}{} {const_str}",
                    cv.var_name.to_utf8(ctx.rt()),
                    cv.var_idx
                );
            }
        }

        println!("  stack_size: {}", (*b).stack_size);
        println!("  opcodes:");

        dump_opcodes(
            ctx,
            3,
            b.opcodes(),
            b.args_slice(),
            b.vars_slice(),
            b.closure_vars_slice(),
            b.cpools_slice(),
            if b.has_debug {
                b.debug.source.as_deref()
            } else {
                None
            },
            if b.has_debug {
                b.debug.line_num as _
            } else {
                -1
            },
            &[],
            Some(self),
        );

        // if (*b).has_debug {
        //     dump_pc2line(
        //         ctx,
        //         (*b).debug
        //             .pc2line_buf
        //             .as_ref()
        //             .map_or(std::ptr::null(), |x| x.as_ptr()),
        //         (*b).debug.pc2line_buf.as_ref().map_or(0, |x| x.len()) as _,
        //         (*b).debug.line_num as _,
        //     );
        // }

        println!();
    }
}

pub unsafe fn dump_pc2line(ctx: &JSContext, buf: *const u8, len: i32, mut line_num: i32) {
    if len <= 0 {
        return;
    }

    println!("{:>5} {:>5}", "PC", "LINE");

    let mut p = buf;
    let p_end = buf.offset(len as isize);
    let mut p_next: *const u8 = std::ptr::null_mut();
    let mut pc = 0;

    while p < p_end {
        let mut op = *p as i32;
        p = p.offset(1);

        if op == 0 {
            if let Ok(n) = unicode_from_utf8(p, (p_end as usize - p as usize) as i32, &mut p_next) {
                pc += n as i32;
                p = p_next;
            } else {
                println!(
                    "invalid pc2line encode pos={}",
                    (p as usize - buf as usize) as i32
                );
                return;
            }

            let mut v = if let Ok(n) =
                unicode_from_utf8(p, (p_end as usize - p as usize) as i32, &mut p_next)
            {
                n as i32
            } else {
                println!(
                    "invalid pc2line encode pos={}",
                    (p as usize - buf as usize) as i32
                );
                return;
            };

            if (v & 1) == 0 {
                v = v >> 1;
            } else {
                v = -(v >> 1) - 1;
            }

            line_num += v;
            p = p_next;
        } else {
            op -= PC2LINE_OP_FIRST;
            pc += (op / PC2LINE_RANGE) as i32;
            line_num += (op % PC2LINE_RANGE) as i32 + PC2LINE_BASE;
        }

        println!("{:5} {:5}", pc, line_num);
    }
}

/// print source code in index ranges from_line..to_line, line index is 0-based.
fn print_lines(source: &str, from_line: usize, to_line: usize) {
    debug_assert!(from_line < to_line);

    for ln in source.lines().skip(from_line).take(to_line - from_line) {
        println!(";; {ln}");
    }
}

// unsafe fn skip_lines(p: &str, n: usize) -> Option<&str> {
//     if let Some(s) = p.splitn(n + 1, '\n').last() {
//         if s.starts_with('\r') {
//             Some(&s[1..])
//         } else {
//             None
//         }
//     } else {
//         None
//     }

//     // let mut p = p;
//     // let mut n = n;

//     // while n > 0 && *p != 0 {
//     //     n -= 1;
//     //     while *p != 0 && *p != b'\n' {
//     //         p = p.offset(1);
//     //     }
//     //     if *p == b'\n' {
//     //         p = p.offset(1); // Move past the '\n'
//     //     }
//     // }
//     // p
// }
