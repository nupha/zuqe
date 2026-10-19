// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OP_COUNT_FINAL, OpCode, OpcodeFormat, opcode_info_final, opcodes},
    config::VM_STACK_LEN_MAX,
    context::ScopedContext,
    utils::read_from_ptr,
};

use super::FunctionDef;

/// compute the maximum stack size needed by the function
struct StackSizeState {
    bc_len: usize,
    stack_level_tab: Vec<u16>,
    catch_pos_tab: Vec<i32>, // -1 for unknown
    stack_len_max: u16,
    pc_stack: Vec<usize>,
    /// Identity of the function being checked, resolved only when an error is
    /// reported. A bare `(op, pc)` pair is not actionable without re-running a
    /// `dump_bytecode` build to find out which function it came from.
    func_name: Atom,
    filename: Atom,
}

/// `func=<name>, file=<name>` for the error messages below.
fn who(ctx: &mut ScopedContext, s: &StackSizeState) -> String {
    format!(
        "func={}, file={}",
        s.func_name.to_utf8(ctx.rt()),
        s.filename.to_utf8(ctx.rt())
    )
}

/// Raise `msg` as an internal error, with the function identity appended.
///
/// The message is built before the throw so the two borrows of `ctx` stay
/// sequential.
fn stack_error(ctx: &mut ScopedContext, s: &StackSizeState, msg: &str) -> i32 {
    let text = format!("{msg}, {}", who(ctx, s));
    ctx.throw_internal_error(&text)
}

// 'op' is only used for error indication
fn ss_check(
    ctx: &mut ScopedContext,
    s: &mut StackSizeState,
    pos: usize,
    op: u8,
    stack_len: u16,
    catch_pos: i32,
) -> Result<(), i32> {
    if pos >= s.bc_len {
        return Err(stack_error(
            ctx,
            s,
            &format!("bytecode buffer overflow 1 (op={op}, pc={pos})"),
        ));
    }

    if stack_len > s.stack_len_max {
        s.stack_len_max = stack_len;
        if s.stack_len_max > VM_STACK_LEN_MAX {
            return Err(stack_error(
                ctx,
                s,
                &format!("stack overflow (op={op}, pc={pos})\0"),
            ));
        }
    }

    if s.stack_level_tab[pos] != 0xffff {
        // already explored: check that the stack size is consistent
        return if s.stack_level_tab[pos] != stack_len {
            Err(stack_error(
                ctx,
                s,
                &format!(
                    "inconsistent stack size: {} {} (pc={pos})",
                    s.stack_level_tab[pos], stack_len
                ),
            ))
        } else if s.catch_pos_tab[pos] != catch_pos {
            Err(stack_error(
                ctx,
                s,
                &format!(
                    "inconsistent catch position: {} {} (pc={pos})",
                    s.catch_pos_tab[pos], catch_pos
                ),
            ))
        } else {
            Ok(())
        };
    }

    // mark as explored and store the stack size
    s.stack_level_tab[pos] = stack_len;
    s.catch_pos_tab[pos] = catch_pos;

    // queue the new PC to explore
    if s.pc_stack.len() == s.pc_stack.capacity() && s.pc_stack.try_reserve(4).is_err() {
        return Err(ctx.throw_out_of_memory());
    }

    s.pc_stack.push(pos);
    Ok(())
}

impl FunctionDef {
    pub fn compute_stack_size(&mut self, ctx: &mut ScopedContext) -> Result<u16, i32> {
        let bc_ptr = self.byte_code.as_ptr();
        let bc_len = self.byte_code.len();

        let mut s_s = StackSizeState {
            bc_len,
            pc_stack: Vec::with_capacity(16), // FIXME: alloc fail handle
            catch_pos_tab: vec![0; bc_len],   // FIXME: alloc fail handle
            stack_level_tab: vec![0xffff; bc_len], // FIXME: alloc fail handle
            stack_len_max: 0,
            func_name: self.func_name,
            filename: self.filename,
        };
        let s = &mut s_s;

        #[cfg(feature = "dump_bytecode")]
        {
            println!("compute stack size");
            println!("{:6} {:25} {:5} {:5}", "pos", "op", "stack", "catch");
        }

        if ss_check(ctx, s, 0, OpCode::INVALID as u8, 0, -1).is_err() {
            return Err(-1);
        }

        while let Some(pos) = s.pc_stack.pop() {
            let mut stack_len = s.stack_level_tab[pos];
            let mut catch_pos = s.catch_pos_tab[pos];
            let op = self.byte_code[pos];

            if op == 0 || op >= OP_COUNT_FINAL {
                return Err(stack_error(
                    ctx,
                    s,
                    &format!("invalid opcode (op={op}, pc={pos})"),
                ));
            }

            let oi = opcode_info_final(op);

            #[cfg(feature = "dump_bytecode")]
            println!("{:5}: {:25} {:5} {:5}", pos, oi.name, stack_len, catch_pos);

            let mut pos_next = pos + oi.size as usize;
            if pos_next > s.bc_len {
                return Err(stack_error(
                    ctx,
                    s,
                    &format!("bytecode buffer overflow 2 (op={op}, pc={pos})"),
                ));
            }

            let mut n_pop = oi.n_pop as u16;
            match oi.fmt {
                OpcodeFormat::NPOP | OpcodeFormat::NPOP_U16 => {
                    n_pop += unsafe { read_from_ptr::<u16>(bc_ptr.add(pos + 1)) };
                }
                #[cfg(feature = "short_opcode")]
                OpcodeFormat::NPOPX => {
                    n_pop += op - OP_call0;
                }
                _ => {}
            }

            if stack_len < n_pop {
                return Err(stack_error(
                    ctx,
                    s,
                    &format!("bytecode buffer overflow 3 (op={op}, pc={pos})"),
                ));
            }

            stack_len = stack_len + oi.n_push as u16 - n_pop;

            if stack_len > s.stack_len_max {
                s.stack_len_max = stack_len;
                if s.stack_len_max > VM_STACK_LEN_MAX {
                    return Err(stack_error(
                        ctx,
                        s,
                        &format!("stack overflow (op={op}, pc={pos})"),
                    ));
                }
            }

            match op {
                opcodes::OP_tail_call
                | opcodes::OP_tail_call_method
                | opcodes::OP_tail_eval
                | opcodes::OP_return
                | opcodes::OP_return_undef
                | opcodes::OP_return_async
                | opcodes::OP_throw
                | opcodes::OP_throw_error
                | opcodes::OP_ret => {
                    continue;
                }
                opcodes::OP_goto => {
                    let diff = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 1)) }; // could be minus
                    pos_next = (pos as i32 + 1 + diff) as usize;
                }

                #[cfg(feature = "short_opcode")]
                opcodes::OP_goto16 => {
                    diff = (read_from_ptr::<u16>(bc_ptr.offset(pos as isize + 1)) as i16) as i32;
                    pos_next = pos + 1 + diff as usize;
                }
                #[cfg(feature = "short_opcode")]
                opcodes::OP_goto8 => {
                    diff = self.byte_code[pos + 1] as i8 as i32;
                    pos_next = pos + 1 + diff as usize;
                }
                #[cfg(feature = "short_opcode")]
                opcodes::OP_if_true8 | opcodes::OP_if_false8 => {
                    diff = self.byte_code[pos + 1] as i8 as i32;
                    if ss_check(ctx, s, pos + 1 + diff as usize, op, stack_len, catch_pos) != 0 {
                        *pstack_size = 0;
                        return -1;
                    }
                }

                opcodes::OP_if_true | opcodes::OP_if_false => {
                    let diff = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 1)) }; // could be minus
                    ss_check(
                        ctx,
                        s,
                        (pos as i32 + 1 + diff) as usize,
                        op,
                        stack_len,
                        catch_pos,
                    )?;
                }
                opcodes::OP_gosub => {
                    let diff = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 1)) };
                    ss_check(
                        ctx,
                        s,
                        (pos as i32 + 1 + diff) as usize,
                        op,
                        stack_len + 1,
                        catch_pos,
                    )?;
                }
                opcodes::OP_with_get_var | opcodes::OP_with_delete_var => {
                    let diff = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 5)) };
                    ss_check(
                        ctx,
                        s,
                        (pos as i32 + 5 + diff) as usize,
                        op,
                        stack_len + 1,
                        catch_pos,
                    )?;
                }
                opcodes::OP_with_make_ref | opcodes::OP_with_get_ref => {
                    let diff = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 5)) };
                    ss_check(
                        ctx,
                        s,
                        (pos as i32 + 5 + diff) as usize,
                        op,
                        stack_len + 2,
                        catch_pos,
                    )?;
                }
                opcodes::OP_with_put_var => {
                    let diff = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 5)) };
                    ss_check(
                        ctx,
                        s,
                        (pos as i32 + 5 + diff) as usize,
                        op,
                        stack_len - 1,
                        catch_pos,
                    )?;
                }
                opcodes::OP_catch => {
                    let diff = unsafe { read_from_ptr::<i32>(bc_ptr.add(pos + 1)) };
                    ss_check(
                        ctx,
                        s,
                        (pos as i32 + 1 + diff) as usize,
                        op,
                        stack_len,
                        catch_pos,
                    )?;
                    catch_pos = pos as _;
                }
                opcodes::OP_for_of_start | opcodes::OP_for_await_of_start => {
                    catch_pos = pos as _;
                    // we assume the catch offset entry is only removed with some op codes
                }
                opcodes::OP_drop
                | opcodes::OP_nip
                | opcodes::OP_nip1
                | opcodes::OP_iterator_close => {
                    let catch_level = (stack_len as i32
                        + if op == opcodes::OP_drop {
                            0
                        } else if op == opcodes::OP_iterator_close {
                            2
                        } else {
                            -1
                        }) as u16;

                    // check_catch:
                    // Note: for for_of_start/for_await_of_start we consider
                    // the catch offset is on the first stack entry instead of the thirst
                    if catch_pos >= 0 {
                        let level = s.stack_level_tab[catch_pos as usize]
                            + if self.byte_code[catch_pos as usize] != OpCode::CATCH as u8 {
                                // for_of_start, for_wait_of_start
                                1
                            } else {
                                0
                            };
                        if catch_level == level {
                            catch_pos = s.catch_pos_tab[catch_pos as usize];
                        }
                    }
                }
                opcodes::OP_nip_catch => {
                    if catch_pos < 0 {
                        return Err(stack_error(
                            ctx,
                            s,
                            &format!("nip_catch: no catch op (pc={pos})"),
                        ));
                    }
                    stack_len = s.stack_level_tab[catch_pos as usize];
                    if self.byte_code[catch_pos as usize] != opcodes::OP_catch {
                        // for_of_start, for_wait_of_start
                        stack_len += 1;
                    }
                    stack_len += 1; // no stack overflow is possible by construction
                    catch_pos = s.catch_pos_tab[catch_pos as usize];
                }
                _ => {}
            }

            ss_check(ctx, s, pos_next, op, stack_len, catch_pos)?;
        }

        Ok(s.stack_len_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Runtime, ScopedContext};

    /// Every stack-size error must name the function it came from. A bare
    /// `(op, pc)` pair is not actionable: finding the function that produced it
    /// used to require re-running with `dump_bytecode`.
    #[test]
    fn stack_errors_name_the_function() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        // An empty bytecode buffer makes the very first `ss_check` overflow.
        let mut s = StackSizeState {
            bc_len: 0,
            stack_level_tab: Vec::new(),
            catch_pos_tab: Vec::new(),
            stack_len_max: 0,
            pc_stack: Vec::new(),
            func_name: Atom::CONST_arguments,
            filename: Atom::CONST_eval,
        };

        let mut scope = ctx.root_scope();
        assert_eq!(ss_check(&mut scope, &mut s, 0, 7, 0, -1), Err(-1));

        let text = scope.take_exception().to_text(&mut scope).message;
        assert!(text.contains("func=arguments"), "no function name: {text}");
        assert!(text.contains("file=eval"), "no filename: {text}");
        assert!(text.contains("pc=0"), "no pc: {text}");
    }
}
