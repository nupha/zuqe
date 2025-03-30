use crate::{
    atom::{JSAtom, JS_ATOM_NULL},
    context::{JSContext, JS_MODE_ASYNC, JS_MODE_STRICT},
    function::{
        JSClosureVar, JSFunctionBytecode, JSFunctionKindEnum, JSVarDef, JSVarKindEnum, LabelSlot,
        PC2LINE_BASE, PC2LINE_OP_FIRST, PC2LINE_RANGE,
    },
    unicode::unicode_from_utf8,
    utils::{get_u16, get_u32},
    value::JSValue,
};

use super::{
    opcode_info_final, opcode_info_intermediate, opcodes, OP_COUNT_FINAL, OP_COUNT_INTERMEDIATE,
};

#[cfg(feature = "dump_bytecode")]
pub unsafe fn dump_opcode_one(ctx: &mut JSContext, b: &JSFunctionBytecode, pc: usize) -> usize {
    let mut pos = pc;
    let tab = b.bytecode_slice();
    let op = tab[pos];

    if op >= OP_COUNT_FINAL {
        println!("invalid opcode (0x{:02x})", op);
        return 0;
    }

    let oi = opcode_info_final(op);
    let size = (*oi).size as usize;
    if pos + size > tab.len() {
        println!("truncated opcode (0x{:02x})", op);
        return 0;
    }

    // print opcode name
    print!("{}", (*oi).name);
    pos += 1;

    // print arguments
    match (*oi).fmt {
        #[cfg(feature = "short_opcode")]
        "none_int" => {
            print!(" {}", op - opcodes::OP_push_0);
        }
        #[cfg(feature = "short_opcode")]
        "npopx" => {
            print!(" {}", op - opcodes::OP_call0);
        }
        "u8" => {
            print!(" {}", tab[pos]);
        }
        "i8" => {
            print!(" {}", tab[pos] as i8);
        }
        "u16" | "npop" => {
            print!(" {}", get_u16(tab.as_ptr().offset(pos as isize)));
        }
        "npop_u16" => {
            print!(
                " {},{}",
                get_u16(tab.as_ptr().offset(pos as _)),
                get_u16(tab.as_ptr().offset(pos as isize + 2)),
            );
        }
        "i16" => {
            print!(" {}", get_u16(tab.as_ptr().offset(pos as _)) as i16);
        }
        "i32" => {
            print!(" {}", get_u32(tab.as_ptr().offset(pos as _)) as i32);
        }
        "u32" => {
            print!(" {}", get_u32(tab.as_ptr().offset(pos as _)));
        }

        #[cfg(feature = "short_opcode")]
        "label8" => {
            addr = get_i8(tab.as_ptr().offset(pos));
            has_addr1();
        }
        #[cfg(feature = "short_opcode")]
        "label16" => {
            addr = get_i16(tab.as_ptr().offset(pos));
            has_addr1!();
        }

        f @ ("label" | "label_u16") => {
            // addr = get_u32(tab.as_ptr().add(pos)) as _;
            // has_addr1!();

            if f == "label_u16" {
                print!(", {}", get_u16(tab.as_ptr().add(pos + 4)));
            }
        }

        #[cfg(feature = "short_opcode")]
        "const8" => {
            idx = get_u8(tab.offset(pos));
            // C2RS: goto has_pool_idx replaced with inline logic
            printf(" {}: ", idx);
            if idx < cpool_count as i32 {
                (*cpool.offset(idx as isize)).dump(ctx.rt());
            }
        }

        "const" => {
            let idx = get_u32(tab.as_ptr().offset(pos as _)) as usize;
            print!(" {idx}:");
            b.get_cpool(idx).unwrap().dump(ctx.rt());
        }
        "atom" => {
            let a = get_u32(tab.as_ptr().add(pos as _));
            print!(" {a}:{}", JSAtom(a).to_str((*ctx).rt()));
        }
        f @ ("atom_u8" | "atom_u16") => {
            print!(
                " {}, {}",
                JSAtom(get_u32(tab.as_ptr().add(pos as _))).to_str((*ctx).rt()),
                if f == "atom_u8" {
                    tab[pos as usize + 4] as usize
                } else {
                    get_u16(tab.as_ptr().add(pos as usize + 4)) as usize
                }
            );
        }
        f @ ("atom_label_u8" | "atom_label_u16") => {
            print!(
                " {}",
                JSAtom(get_u32(tab.as_ptr().add(pos as _))).to_str((*ctx).rt())
            );

            // addr = get_u32(tab.as_ptr().offset(pos as isize + 4)) as _;
            // if pass == 1 {
            //     print!(", {}:{}", addr, (*label_slots.offset(addr as isize)).pos);
            // } else if pass == 2 {
            //     print!(", {}:{}", addr, (*label_slots.offset(addr as isize)).pos2);
            // } else if pass == 3 {
            //     print!(", {}", addr + pos + 4);
            // }

            if f == "atom_label_u8" {
                print!(", {}", *tab.as_ptr().offset(pos as isize + 8));
            } else {
                print!(", {}", get_u16(tab.as_ptr().offset(pos as isize + 8)));
            }
        }

        #[cfg(feature = "short_opcode")]
        "none_loc" => {
            idx = (op - OP_get_loc0) % 4;
            // C2RS: goto has_loc replaced with inline logic
            printf(" {}: ", idx);
            if idx < var_count {
                print_atom((*ctx).rt, (*vars.offset(idx as isize)).var_name);
            }
        }

        f @ ("loc8" | "loc") => {
            let idx = if f == "loc8" {
                *tab.as_ptr().offset(pos as _) as usize
            } else {
                get_u16(tab.as_ptr().offset(pos as _)) as usize
            };
            print!(
                " {idx}:{}",
                b.get_var(idx as _).unwrap().var_name.to_str((*ctx).rt())
            );
        }

        #[cfg(feature = "short_opcode")]
        "none_arg" => {
            idx = (op - opcodes::OP_get_arg0) % 4;
            // C2RS: goto has_arg replaced with inline logic
            printf(" {}: ", idx);
            if idx < arg_count {
                print_atom((*ctx).rt, (*args.offset(idx as isize)).var_name);
            }
        }

        "arg" => {
            let idx = get_u16(tab.as_ptr().offset(pos as _)) as usize;
            print!(
                " {idx}:{}",
                b.get_arg(idx as _).unwrap().var_name.to_str((*ctx).rt())
            );
        }

        #[cfg(feature = "short_opcode")]
        "none_var_ref" => {
            idx = (op - opcodes::OP_get_var_ref0) % 4;
            // C2RS: goto has_var_ref replaced with inline logic
            printf(" {}: ", idx);
            if idx < closure_var_count {
                print_atom(ctx, (*closure_var.offset(idx as isize)).var_name);
            }
        }

        "var_ref" => {
            let idx = get_u16(tab.as_ptr().offset(pos as _)) as usize;
            print!(
                " {idx}:{}",
                b.get_closure_var(idx as _)
                    .unwrap()
                    .var_name
                    .to_str((*ctx).rt())
            );
        }

        _ => {}
    }

    size
}

#[cfg(feature = "dump_bytecode")]
pub unsafe fn dump_byte_code(
    ctx: *mut JSContext,
    pass: i32,
    tab: &[u8],
    args: &[JSVarDef],
    vars: &[JSVarDef],
    closure_vars: &[JSClosureVar],
    cpools: &[JSValue],
    source: *const i8,
    line_num: i32,
    label_slots: *const LabelSlot,
    b: *const JSFunctionBytecode,
) {
    let len = tab.len() as i32;

    let use_short_opcodes = !b.is_null();
    let op_max = if use_short_opcodes {
        OP_COUNT_FINAL
    } else {
        OP_COUNT_INTERMEDIATE
    };

    let mut addr: i32;
    let mut pos: i32 = 0;
    let mut pos_next: i32;
    let mut bits = vec![0_u8; tab.len()];

    // scan for jump targets
    while pos < len {
        let op = tab[pos as usize];
        let oi = if use_short_opcodes {
            opcode_info_final(op)
        } else {
            opcode_info_intermediate(op)
        };
        pos_next = pos + (*oi).size as i32;

        if op < op_max {
            match (*oi).fmt {
                #[cfg(feature = "short_opcode")]
                "label8" => {
                    pos += 1;
                    addr = *tab.offset(pos) as i8 as i32;
                    // C2RS: goto has_addr replaced with inline logic
                    if pass == 1 {
                        addr = (*label_slots.offset(addr as isize)).pos;
                    }
                    if pass == 2 {
                        addr = (*label_slots.offset(addr as isize)).pos2;
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
                    addr = get_u16(tab.offset(pos)) as i16 as i32;
                    // C2RS: goto has_addr replaced with inline logic
                    if pass == 1 {
                        addr = (*label_slots.offset(addr as isize)).pos;
                    }
                    if pass == 2 {
                        addr = (*label_slots.offset(addr as isize)).pos2;
                    }
                    if pass == 3 {
                        addr += pos;
                    }
                    if addr >= 0 && addr < len {
                        *bits.offset(addr as isize) |= 1;
                    }
                }

                f @ ("atom_label_u8" | "atom_label_u16" | "label" | "label_u16") => {
                    if matches!(f, "atom_label_u8" | "atom_label_u16") {
                        pos += 4;
                    }
                    pos += 1;
                    addr = get_u32(tab.as_ptr().add(pos as _)) as i32;

                    let pc = match pass {
                        1 => (*label_slots.offset(addr as isize)).pos,
                        2 => (*label_slots.offset(addr as isize)).pos2,
                        3 => addr + pos,
                        _ => {
                            unreachable!()
                        }
                    };
                    if pc >= 0 && pc < tab.len() as i32 {
                        *bits.get_mut(pc as usize).unwrap() |= 1;
                    }
                }
                _ => {}
            }
        }

        pos = pos_next;
    }

    macro_rules! has_addr1 {
        () => {
            if pass == 1 {
                print!(" {}:{}", addr, (*label_slots.offset(addr as isize)).pos);
            } else if pass == 2 {
                print!(" {}:{}", addr, (*label_slots.offset(addr as isize)).pos2);
            } else if pass == 3 {
                print!(" {}", addr + pos);
            }
        };
    }

    let mut in_source = false;
    if !source.is_null() {
        // Always print first line: needed if single line
        print_lines(source.cast(), 0, 1);
        in_source = true;
    }

    let mut size: i32;
    let mut line1 = 1;
    let mut line = 1;

    pos = 0;
    while pos < len {
        let op = tab[pos as usize];

        // print source code
        {
            if !source.is_null() {
                if !b.is_null() {
                    line1 = (*b).find_line_num(pos as _).unwrap() as i32 - line_num + 1;
                } else if op == opcodes::op_line_num {
                    line1 = get_u32(tab.as_ptr().add(pos as usize + 1)) as i32 - line_num + 1;
                };

                if line1 > line {
                    if !in_source {
                        println!();
                    }
                    in_source = true;
                    print_lines(source.cast(), line, line1);
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
        size = (*oi).size as i32;
        if pos + size > len {
            println!("truncated opcode (0x{:02x})", op);
            break;
        }

        // print bytecode in hex
        {
            let mut x = 0;
            print!("{pos:5} ");
            let x0 = 6;

            for i in 0..size {
                if i == 6 {
                    print!("\n      ");
                    x = 0;
                }
                print!(" {:02X}", tab[(pos + i) as usize]);
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
            "none_int" => {
                print!(" {}", op - opcodes::OP_push_0);
            }
            #[cfg(feature = "short_opcode")]
            "npopx" => {
                print!(" {}", op - opcodes::OP_call0);
            }
            "u8" => {
                print!(" {}", tab[pos as usize]);
            }
            "i8" => {
                print!(" {}", tab[pos as usize] as i8);
            }
            "u16" | "npop" => {
                print!(" {}", get_u16(tab.as_ptr().offset(pos as isize)));
            }
            "npop_u16" => {
                print!(
                    " {},{}",
                    get_u16(tab.as_ptr().offset(pos as _)),
                    get_u16(tab.as_ptr().offset(pos as isize + 2)),
                );
            }
            "i16" => {
                print!(" {}", get_u16(tab.as_ptr().offset(pos as _)) as i16);
            }
            "i32" => {
                print!(" {}", get_u32(tab.as_ptr().offset(pos as _)) as i32);
            }
            "u32" => {
                print!(" {}", get_u32(tab.as_ptr().offset(pos as _)));
            }

            #[cfg(feature = "short_opcode")]
            "label8" => {
                addr = get_i8(tab.as_ptr().offset(pos));
                has_addr1();
            }
            #[cfg(feature = "short_opcode")]
            "label16" => {
                addr = get_i16(tab.as_ptr().offset(pos));
                has_addr1!();
            }

            f @ ("label" | "label_u16") => {
                addr = get_u32(tab.as_ptr().offset(pos as _)) as _;
                has_addr1!();

                if f == "label_u16" {
                    print!(", {}", get_u16(tab.as_ptr().add(pos as usize + 4)));
                }
            }

            #[cfg(feature = "short_opcode")]
            "const8" => {
                idx = get_u8(tab.offset(pos));
                // C2RS: goto has_pool_idx replaced with inline logic
                printf(" {}: ", idx);
                if idx < cpool_count as i32 {
                    (*cpool.offset(idx as isize)).dump(ctx.rt());
                }
            }

            "const" => {
                let idx = get_u32(tab.as_ptr().offset(pos as _)) as usize;
                print!(" {idx}:");
                if let Some(v) = cpools.get(idx) {
                    (*v).dump((*ctx).rt());
                } else {
                    #[cfg(debug_assertions)]
                    unreachable!();
                }
            }
            "atom" => {
                let a = get_u32(tab.as_ptr().add(pos as _));
                print!(" {a}:{}", JSAtom(a).to_str((*ctx).rt()));
            }
            f @ ("atom_u8" | "atom_u16") => {
                print!(
                    " {}, {}",
                    JSAtom(get_u32(tab.as_ptr().add(pos as _))).to_str((*ctx).rt()),
                    if f == "atom_u8" {
                        tab[pos as usize + 4] as usize
                    } else {
                        get_u16(tab.as_ptr().add(pos as usize + 4)) as usize
                    }
                );
            }
            f @ ("atom_label_u8" | "atom_label_u16") => {
                print!(
                    " {}",
                    JSAtom(get_u32(tab.as_ptr().add(pos as _))).to_str((*ctx).rt())
                );

                addr = get_u32(tab.as_ptr().offset(pos as isize + 4)) as _;
                if pass == 1 {
                    print!(", {}:{}", addr, (*label_slots.offset(addr as isize)).pos);
                } else if pass == 2 {
                    print!(", {}:{}", addr, (*label_slots.offset(addr as isize)).pos2);
                } else if pass == 3 {
                    print!(", {}", addr + pos + 4);
                }

                if f == "atom_label_u8" {
                    print!(", {}", *tab.as_ptr().offset(pos as isize + 8));
                } else {
                    print!(", {}", get_u16(tab.as_ptr().offset(pos as isize + 8)));
                }
            }

            #[cfg(feature = "short_opcode")]
            "none_loc" => {
                idx = (op - OP_get_loc0) % 4;
                // C2RS: goto has_loc replaced with inline logic
                printf(" {}: ", idx);
                if idx < var_count {
                    print_atom((*ctx).rt, (*vars.offset(idx as isize)).var_name);
                }
            }

            f @ ("loc8" | "loc") => {
                let idx = if f == "loc8" {
                    *tab.as_ptr().offset(pos as _) as usize
                } else {
                    get_u16(tab.as_ptr().offset(pos as _)) as usize
                };
                print!(
                    " {idx}:{}",
                    vars.get(idx).unwrap().var_name.to_str((*ctx).rt())
                );
            }

            #[cfg(feature = "short_opcode")]
            "none_arg" => {
                idx = (op - opcodes::OP_get_arg0) % 4;
                // C2RS: goto has_arg replaced with inline logic
                printf(" {}: ", idx);
                if idx < arg_count {
                    print_atom((*ctx).rt, (*args.offset(idx as isize)).var_name);
                }
            }

            "arg" => {
                let idx = get_u16(tab.as_ptr().offset(pos as _)) as usize;
                print!(
                    " {idx}:{}",
                    args.get(idx).unwrap().var_name.to_str((*ctx).rt())
                );
            }

            #[cfg(feature = "short_opcode")]
            "none_var_ref" => {
                idx = (op - opcodes::OP_get_var_ref0) % 4;
                // C2RS: goto has_var_ref replaced with inline logic
                printf(" {}: ", idx);
                if idx < closure_var_count {
                    print_atom(ctx, (*closure_var.offset(idx as isize)).var_name);
                }
            }

            "var_ref" => {
                let idx = get_u16(tab.as_ptr().offset(pos as _)) as usize;
                print!(
                    " {idx}:{}",
                    closure_vars.get(idx).unwrap().var_name.to_str((*ctx).rt())
                );
            }

            _ => {}
        }
        println!();

        pos += (*oi).size as i32 - 1;
    }

    if !source.is_null() {
        if !in_source {
            println!();
        }
        print_lines(source.cast(), line, i32::MAX);
    }
}

#[cfg(feature = "dump_bytecode")]
pub unsafe fn js_dump_function_bytecode(ctx: *mut JSContext, b: *mut JSFunctionBytecode) {
    if (*b).has_debug && (*b).debug.filename != JS_ATOM_NULL {
        print!(
            "{}:{}: ",
            (*b).debug.filename.to_str((*ctx).rt()),
            (*b).debug.line_num
        );
    }

    println!(
        "function: {}{}",
        if (*b).func_kind == JSFunctionKindEnum::JS_FUNC_GENERATOR {
            "*"
        } else {
            ""
        },
        (*b).func_name.to_str((*ctx).rt()),
    );

    if (*b).js_mode != 0 {
        print!("  mode:");
        if (*b).js_mode & JS_MODE_STRICT as u8 != 0 {
            print!(" strict");
        }
        if (*b).js_mode & JS_MODE_ASYNC as u8 != 0 {
            print!(" async");
        }
        println!();
    }

    if let Some(args) = (*b).args_slice_mut() {
        print!("  args:");
        for arg in args {
            print!(" {}", arg.var_name.to_str((*ctx).rt()));
        }
        println!();
    }

    if let Some(vars) = (*b).vars_slice_mut() {
        println!("  locals:");
        for (i, vd) in vars.iter().enumerate() {
            let kind_str = match vd.var_kind() {
                JSVarKindEnum::JS_VAR_CATCH => "catch",
                JSVarKindEnum::JS_VAR_FUNCTION_DECL | JSVarKindEnum::JS_VAR_NEW_FUNCTION_DECL => {
                    "function"
                }
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

            print!(
                "{i:5}: {kind_str} {}'{}",
                vd.var_name.to_str((*ctx).rt()),
                vd.var_name.ref_count((*ctx).rt()).unwrap_or(999)
            );

            if (*vd).scope_level != 0 {
                print!(" [level:{} next:{}]", vd.scope_level, vd.scope_next);
            }
            println!();
        }
    }

    if let Some(cvars) = (*b).closure_vars_slice_mut() {
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
                cv.var_name.to_str((*ctx).rt()),
                cv.var_idx
            );
        }
    }

    println!("  stack_size: {}", (*b).stack_size);
    println!("  opcodes:");

    dump_byte_code(
        ctx,
        3,
        (*b).bytecode_slice(),
        (*b).args_slice_mut().unwrap_or([].as_mut_slice()),
        (*b).vars_slice_mut().unwrap_or([].as_mut_slice()),
        (*b).closure_vars_slice_mut().unwrap_or([].as_mut_slice()),
        (*b).cpools_slice().unwrap_or([].as_slice()),
        if (*b).has_debug {
            (*b).debug.source.cast()
        } else {
            std::ptr::null()
        },
        if (*b).has_debug {
            (*b).debug.line_num as _
        } else {
            -1
        },
        std::ptr::null_mut(),
        b,
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

pub unsafe fn dump_pc2line(ctx: *const JSContext, buf: *const u8, len: i32, mut line_num: i32) {
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

/// print source code in ranges from_line..to_line
unsafe fn print_lines(source: *const u8, from_line: i32, to_line: i32) {
    let mut s = source;
    let mut p = skip_lines(s, from_line);
    if *p != 0 {
        for _ in from_line..to_line {
            s = p; // this line
            p = skip_lines(s, 1); // next line
            let length = p.offset_from(s) as usize; // len of this line

            print!(
                ";; {:.*}",
                length,
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(s, length))
            );
            if *p == 0 {
                if *p.offset(-1) != b'\n' {
                    println!();
                }
                break;
            }
        }
    }
}

unsafe fn skip_lines(p: *const u8, n: i32) -> *const u8 {
    let mut p = p;
    let mut n = n;

    while n > 0 && *p != 0 {
        n -= 1;
        while *p != 0 && *p != b'\n' {
            p = p.offset(1);
        }
        if *p == b'\n' {
            p = p.offset(1); // Move past the '\n'
        }
    }
    p
}
