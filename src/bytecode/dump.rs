// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![cfg(feature = "dump_bytecode")]

use crate::{
    atom::Atom,
    bytecode::OpcodeFormat,
    context::ContextImpl,
    function::{Bytecode, ClosureVar, FunctionKind, JSVarDef, JSVarKindEnum, Label},
    stack_frame::ExecMode,
    utils::read_from_ptr,
    value::JSValue,
};

use super::{
    OP_COUNT_FINAL, OP_COUNT_INTERMEDIATE, opcode_info_final, opcode_info_intermediate, opcodes,
};

/// dump opcodes binary in pass 1,2,3
#[allow(clippy::too_many_arguments)]
pub(crate) fn dump_opcodes(
    ctx: &ContextImpl,
    pass: i32,
    bin: &[u8],
    args: &[JSVarDef],
    vars: &[JSVarDef],
    closure_vars: &[ClosureVar],
    consts: &[JSValue],
    source: Option<&str>,
    line_num: i32,
    labels: &[Label], // for pass 1, 2 only
    bc: Option<&Bytecode>,
) {
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
        let op = bin[pos];
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
                    pos += 1; // skip opcode
                    if matches!(
                        f,
                        OpcodeFormat::ATOM_LABEL_U8 | OpcodeFormat::ATOM_LABEL_U16
                    ) {
                        pos += 4; // skip atom
                    }

                    // read label addr
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
        let op = bin[pos];

        // print source code
        {
            if let Some(s) = source {
                if let Some(b) = bc {
                    line1 = b.find_line_num(pos as _).unwrap_or(-1) - line_num + 1;
                } else if op == opcodes::op_line_num {
                    line1 =
                        unsafe { read_from_ptr::<i32>(bin.as_ptr().add(pos + 1)) - line_num + 1 };
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

        let op_len = oi.size as usize;
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
                print!(" {:02X}", bin[pos + i]);
                x += 3;
            }
            print!("{:width$}", " ", width = x0 + 20 - x);
        }

        // print address
        if bits[pos] != 0 {
            print!("{pos:5}:  ");
        } else {
            print!("        ");
        }

        // print opcode name
        print!("{}", oi.name);
        pos += 1;

        // print arguments
        match oi.fmt {
            #[cfg(feature = "short_opcode")]
            OpcodeFormat::NONE_INT => {
                print!(" {}", op - opcodes::OP_push_0);
            }
            #[cfg(feature = "short_opcode")]
            OpcodeFormat::NPOPX => {
                print!(" {}", op - opcodes::OP_call0);
            }
            OpcodeFormat::U8 => {
                print!(" {}", bin[pos]);
            }
            OpcodeFormat::I8 => {
                print!(" {}", bin[pos] as i8);
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
                    read_from_ptr::<i16>(bin.as_ptr().add(pos))
                });
            }
            OpcodeFormat::I32 => {
                print!(" {}", unsafe {
                    read_from_ptr::<i32>(bin.as_ptr().add(pos))
                });
            }
            OpcodeFormat::U32 => {
                print!(" {}", unsafe {
                    read_from_ptr::<u32>(bin.as_ptr().add(pos))
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
                let addr = unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos)) };
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
                print!(" {idx}  ; {}", consts[idx].inspect(ctx).unwrap());
            }
            OpcodeFormat::ATOM => {
                let a = unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos)) };
                Atom::from_id(a).with_utf8(ctx.rt_ref(), |s| {
                    print!(" {a}'{s}");
                });
            }
            f @ (OpcodeFormat::ATOM_U8 | OpcodeFormat::ATOM_U16) => {
                let atom = Atom::from_id(unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos)) });
                print!(
                    " {}'{}, {}",
                    atom.id(),
                    atom.to_utf8(ctx.rt_ref()),
                    if f == OpcodeFormat::ATOM_U8 {
                        bin[pos + 4] as usize
                    } else {
                        unsafe { read_from_ptr::<u16>(bin.as_ptr().add(pos + 4)) as usize }
                    }
                );
            }
            f @ (OpcodeFormat::ATOM_LABEL_U8 | OpcodeFormat::ATOM_LABEL_U16) => {
                let atom =
                    Atom::from_id(unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos as _)) });

                print!(" {}'{}", atom.id(), atom.to_utf8(ctx.rt_ref()));

                let addr = unsafe { read_from_ptr::<u32>(bin.as_ptr().add(pos + 4)) };
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
                    " {idx}  ; {}",
                    vars.get(idx).unwrap().var_name.to_utf8(ctx.rt_ref())
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
                    " {idx}  ; {}",
                    args.get(idx).unwrap().var_name.to_utf8(ctx.rt_ref())
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
                    " {idx}  ; {}",
                    closure_vars[idx].var_name.to_utf8(ctx.rt_ref())
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
    pub fn dump(&self, ctx: &ContextImpl) {
        print!("{:?}: ", self);

        if let Some(debug) = self.debug.as_ref()
            && !debug.filename.is_null()
        {
            print!(
                "{}:{}: ",
                debug.filename.to_utf8(ctx.rt_ref()),
                debug.line_num
            );
        }

        println!(
            "function: {}{}",
            if self.func_kind() == FunctionKind::Generator {
                "*"
            } else {
                ""
            },
            self.func_name.to_utf8(ctx.rt_ref()),
        );

        if !self.mode().is_empty() {
            print!("  mode:");
            if self.mode().contains(ExecMode::STRICT) {
                print!(" strict");
            }
            if self.mode().contains(ExecMode::ASYNC) {
                print!(" async");
            }
            println!();
        }

        {
            let args = self.arguments();
            if !args.is_empty() {
                print!("  args:");
                for arg in args {
                    print!(" {}", arg.var_name.to_utf8(ctx.rt_ref()));
                }
                println!();
            }
        }

        let vars = self.local_vars();
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

                print!("{i:5}: {kind_str} {}", vd.var_name.to_utf8(ctx.rt_ref()));

                if vd.scope_level != 0 {
                    print!(" [level:{} next:{}]", vd.scope_level, vd.scope_next);
                }
                println!();
            }
        }

        let cvars = self.closure_vars();
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
                    cv.var_name.to_utf8(ctx.rt_ref()),
                    cv.var_index()
                );
            }
        }

        println!("  stack_size: {}", self.stack_size);
        println!("  opcodes:");

        dump_opcodes(
            ctx,
            3,
            self.opcodes(),
            self.arguments(),
            self.local_vars(),
            self.closure_vars(),
            self.consts(),
            self.debug.as_ref().and_then(|d| d.source.as_deref()),
            self.debug.as_ref().map_or(-1, |x| x.line_num),
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

/// dump instruction (opcode and params) at given pc.
pub(super) fn dump_run_instruction(
    ctx: &ContextImpl,
    b: &Bytecode,
    pc: usize,
    sp_base: *const JSValue,
    sp_top: *const JSValue,
) -> usize {
    if !sp_top.is_null() {
        debug_assert!(sp_base <= sp_top);
    }

    let mut pos = pc;
    let tab = b.opcodes();
    let op = tab[pos];

    print!("[{pos:4}] ");

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
                read_from_ptr::<u16>(tab.as_ptr().add(pos))
            });
        }
        OpcodeFormat::NPOP_U16 => {
            print!(
                " {},{}",
                unsafe { read_from_ptr::<u16>(tab.as_ptr().add(pos)) },
                unsafe { read_from_ptr::<u16>(tab.as_ptr().add(pos + 2)) },
            );
        }
        OpcodeFormat::I16 => {
            print!(" {}", unsafe {
                read_from_ptr::<i16>(tab.as_ptr().add(pos))
            });
        }
        OpcodeFormat::I32 => {
            print!(" {}", unsafe {
                read_from_ptr::<i32>(tab.as_ptr().add(pos))
            });
        }
        OpcodeFormat::U32 => {
            print!(" {}", unsafe {
                read_from_ptr::<u32>(tab.as_ptr().add(pos))
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
            print!(
                " {idx}:{}",
                b.load_const(idx as usize).unwrap().inspect(ctx).unwrap()
            );
        }
        OpcodeFormat::ATOM => {
            let a = unsafe { read_from_ptr::<u32>(tab.as_ptr().add(pos)) };
            Atom::from_id(a).with_utf8(ctx.rt_ref(), |s| {
                print!(" {a}'{s}");
            });
        }
        f @ (OpcodeFormat::ATOM_U8 | OpcodeFormat::ATOM_U16) => {
            let atom = Atom::from_id(unsafe { read_from_ptr::<u32>(tab.as_ptr().add(pos)) });
            atom.with_utf8(ctx.rt_ref(), |s| {
                print!(
                    " {}'{s}, {}",
                    atom.id(),
                    if f == OpcodeFormat::ATOM_U8 {
                        tab[pos + 4] as usize
                    } else {
                        unsafe { read_from_ptr::<u16>(tab.as_ptr().add(pos + 4)) as usize }
                    }
                );
            });
        }
        f @ (OpcodeFormat::ATOM_LABEL_U8 | OpcodeFormat::ATOM_LABEL_U16) => {
            let atom = Atom::from_id(unsafe { read_from_ptr::<u32>(tab.as_ptr().add(pos)) });
            atom.with_utf8(ctx.rt_ref(), |s| {
                print!(" {}'{s}", atom.id());
            });

            // label
            let diff = unsafe { read_from_ptr::<i32>(tab.as_ptr().add(pos + 4)) };
            print!(", {diff}");

            if f == OpcodeFormat::ATOM_LABEL_U8 {
                print!(", {}", unsafe { *tab.as_ptr().add(pos + 8) });
            } else {
                print!(", {}", unsafe {
                    read_from_ptr::<u16>(tab.as_ptr().add(pos + 8))
                });
            }

            // print jump abs pos
            print!("   ;; {diff} -> @{}", diff + pos as i32 + 4)
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
            if let Some(v) = b.get_var(idx as _) {
                print!(" {idx}  ; {}", v.var_name.to_utf8(ctx.rt_ref()));
            } else {
                print!(" {idx}");
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
            if let Some(vi) = b.get_arg(idx as _) {
                vi.var_name.with_utf8(ctx.rt_ref(), |s| {
                    print!(" {idx}  ; {s}");
                });
            } else {
                print!(" {idx}");
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
            let cv = b.get_closure_var(idx as _).unwrap();
            print!(" {idx}  ; {}", cv.var_name.to_utf8(ctx.rt_ref()));
            if cv.is_arg() {
                print!(", argument");
            }
            if cv.is_local() {
                print!(", local");
            }
            if cv.is_lexical() {
                print!(", lexical");
            }
            if cv.is_const() {
                print!(", const");
            }
        }

        _ => {}
    }

    println!();

    if !sp_top.is_null() && (oi.n_push > 0 || oi.n_pop > 0) {
        dump_run_stack(
            ctx,
            unsafe { std::slice::from_raw_parts(sp_base, sp_top.offset_from(sp_base) as usize) },
            true,
            oi.n_pop as _,
            '✕',
        );
    }

    size
}

pub(super) fn dump_run_stack(
    ctx: &ContextImpl,
    stack: &[JSValue],
    tree: bool,
    mark_count: usize,
    mark: char,
) {
    let n = stack.len();
    debug_assert!(mark_count <= n);
    let mark_start = n - mark_count;

    for (i, v) in stack.iter().enumerate() {
        println!(
            "{:>10}{} {} {}",
            if tree {
                if i == n - 1 { "└─" } else { "├─" }
            } else {
                ""
            },
            if i >= mark_start { mark } else { ' ' },
            // value repr
            {
                let s = {
                    #[cfg(feature = "builtin_proxy")]
                    if v.as_object_Proxy().is_some() {
                        "[object Proxy]".to_string() // to avoid ::inspect() on a revoked Proxy returnes error
                    } else {
                        v.inspect(ctx).unwrap()
                    }
                    #[cfg(not(feature = "builtin_proxy"))]
                    {
                        v.inspect(ctx).unwrap()
                    }
                };

                if s.chars().count() > 80 {
                    s.chars().take(80).collect::<String>() + "…"
                } else {
                    s.to_string()
                }
            },
            // scope
            if let Some(h) = v.gc_node_ptr() {
                format!("{h:p}@{:?}", unsafe { h.as_ref().scope_id() })
            } else {
                "".to_string()
            }
        );
    }
}
