use std::ffi::CStr;

use crate::{
    bytecode::{opcode_info_final, opcodes, OpCode, OP_COUNT_FINAL},
    context::JSContext,
    runtime::JS_STACK_SIZE_MAX,
    utils::read_from_ptr,
};

use super::JSFunctionDef;

/* compute the maximum stack size needed by the function */
#[repr(C)]
struct StackSizeState {
    pub bc_len: usize,
    pub stack_level_tab: Vec<u16>,
    pub catch_pos_tab: Vec<i32>, // -1 for unknown
    pub stack_len_max: u16,
    pub pc_stack: Vec<usize>,
}

// 'op' is only used for error indication
unsafe fn ss_check(
    ctx: &mut JSContext,
    s: &mut StackSizeState,
    pos: usize,
    op: u8,
    stack_len: u16,
    catch_pos: i32,
) -> Result<(), i32> {
    if pos >= s.bc_len {
        ctx.JS_ThrowInternalError(CStr::from_ptr(
            format!("bytecode buffer overflow 1 (op={op}, pc={pos})\0")
                .as_ptr()
                .cast(),
        ));
        return Err(-1);
    }

    if stack_len > s.stack_len_max {
        s.stack_len_max = stack_len;
        if s.stack_len_max > JS_STACK_SIZE_MAX as u16 {
            ctx.JS_ThrowInternalError(CStr::from_ptr(
                format!("stack overflow (op={op}, pc={pos})\0")
                    .as_ptr()
                    .cast(),
            ));
            return Err(-1);
        }
    }

    if s.stack_level_tab[pos] != 0xffff {
        // already explored: check that the stack size is consistent
        return if s.stack_level_tab[pos] != stack_len {
            ctx.JS_ThrowInternalError(CStr::from_ptr(
                format!(
                    "inconsistent stack size: {} {} (pc={pos})\0",
                    s.stack_level_tab[pos], stack_len
                )
                .as_ptr()
                .cast(),
            ));
            Err(-1)
        } else if s.catch_pos_tab[pos] != catch_pos {
            ctx.JS_ThrowInternalError(CStr::from_ptr(
                format!(
                    "inconsistent catch position: {} {} (pc={pos})\n",
                    s.catch_pos_tab[pos], catch_pos
                )
                .as_ptr()
                .cast(),
            ));
            Err(-1)
        } else {
            Ok(())
        };
    }

    // mark as explored and store the stack size
    s.stack_level_tab[pos] = stack_len;
    s.catch_pos_tab[pos] = catch_pos;

    // queue the new PC to explore
    if s.pc_stack.len() == s.pc_stack.capacity() {
        if s.pc_stack.try_reserve(4).is_err() {
            return Err(-1); // fail to grow
        }
    }

    s.pc_stack.push(pos);
    Ok(())
}

impl JSFunctionDef {
    pub unsafe fn compute_stack_size(&mut self, ctx: &mut JSContext) -> Result<u16, i32> {
        let bc_len = self.byte_code.len();
        let bc_buf = self.byte_code.as_ptr();

        let mut s_s = StackSizeState {
            bc_len,
            pc_stack: Vec::with_capacity(16), // FIXME: alloc fail handle
            catch_pos_tab: vec![0; bc_len],   // FIXME: alloc fail handle
            stack_level_tab: vec![0xffff; bc_len], // FIXME: alloc fail handle
            stack_len_max: 0,
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
            let op = *bc_buf.add(pos);

            if op == 0 || op >= OP_COUNT_FINAL {
                ctx.JS_ThrowInternalError(CStr::from_ptr(
                    format!("invalid opcode (op={op}, pc={pos})\0")
                        .as_ptr()
                        .cast(),
                ));
                return Err(-1);
            }

            let oi = opcode_info_final(op);

            #[cfg(feature = "dump_bytecode")]
            println!(
                "{:5}: {:25} {:5} {:5}",
                pos,
                (*oi).name,
                stack_len,
                catch_pos
            );

            let mut pos_next = pos + (*oi).size as usize;
            if pos_next > (*s).bc_len {
                ctx.JS_ThrowInternalError(CStr::from_ptr(
                    format!("bytecode buffer overflow 2 (op={op}, pc={pos})\0")
                        .as_ptr()
                        .cast(),
                ));
                return Err(-1);
            }

            let mut n_pop = (*oi).n_pop as u16;
            match (*oi).fmt {
                "npop" | "npop_u16" => {
                    n_pop += read_from_ptr::<u16>(bc_buf.add(pos + 1));
                }
                #[cfg(feature = "short_opcode")]
                "npopx" => {
                    n_pop += op - OP_call0;
                }
                _ => {}
            }

            if stack_len < n_pop {
                ctx.JS_ThrowInternalError(CStr::from_ptr(
                    format!("bytecode buffer overflow 3 (op={op}, pc={pos})\0")
                        .as_ptr()
                        .cast(),
                ));
                return Err(-1);
            }

            stack_len = stack_len + (*oi).n_push as u16 - n_pop;
            if stack_len > s.stack_len_max {
                s.stack_len_max = stack_len;
                if s.stack_len_max > JS_STACK_SIZE_MAX {
                    ctx.JS_ThrowInternalError(CStr::from_ptr(
                        format!("stack overflow (op={op}, pc={pos})\0")
                            .as_ptr()
                            .cast(),
                    ));
                    return Err(-1);
                }
            }

            match op {
                opcodes::OP_tail_call
                | opcodes::OP_tail_call_method
                | opcodes::OP_return
                | opcodes::OP_return_undef
                | opcodes::OP_return_async
                | opcodes::OP_throw
                | opcodes::OP_throw_error
                | opcodes::OP_ret => {
                    continue;
                }
                opcodes::OP_goto => {
                    let diff = read_from_ptr::<i32>(bc_buf.add(pos + 1)); // could be minus
                    pos_next = (pos as i32 + 1 + diff) as usize;
                }

                #[cfg(feature = "short_opcode")]
                opcodes::OP_goto16 => {
                    diff = (read_from_ptr::<u16>(bc_buf.offset(pos as isize + 1)) as i16) as i32;
                    pos_next = pos + 1 + diff as usize;
                }
                #[cfg(feature = "short_opcode")]
                opcodes::OP_goto8 => {
                    diff = *bc_buf.offset(pos as isize + 1) as i8 as i32;
                    pos_next = pos + 1 + diff as usize;
                }
                #[cfg(feature = "short_opcode")]
                opcodes::OP_if_true8 | opcodes::OP_if_false8 => {
                    diff = *bc_buf.offset(pos as isize + 1) as i8 as i32;
                    if ss_check(ctx, s, pos + 1 + diff as usize, op, stack_len, catch_pos) != 0 {
                        *pstack_size = 0;
                        return -1;
                    }
                }

                opcodes::OP_if_true | opcodes::OP_if_false => {
                    let diff = read_from_ptr::<i32>(bc_buf.add(pos + 1)); // could be minus
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
                    let diff = read_from_ptr::<i32>(bc_buf.add(pos + 1));
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
                    let diff = read_from_ptr::<i32>(bc_buf.add(pos + 5));
                    ss_check(
                        ctx,
                        s,
                        (pos as i32 + 5 + diff) as usize,
                        op,
                        stack_len + 1,
                        catch_pos,
                    )?;
                }
                opcodes::OP_with_make_ref
                | opcodes::OP_with_get_ref
                | opcodes::OP_with_get_ref_undef => {
                    let diff = read_from_ptr::<i32>(bc_buf.add(pos + 5));
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
                    let diff = read_from_ptr::<i32>(bc_buf.add(pos + 5));
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
                    let diff = read_from_ptr::<i32>(bc_buf.add(pos + 1));
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
                    /* we assume the catch offset entry is only removed with some op codes */
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
                            + if *bc_buf.add(catch_pos as usize) != OpCode::CATCH as u8 {
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
                        ctx.JS_ThrowInternalError(CStr::from_ptr(
                            format!("nip_catch: no catch op (pc={pos})\0")
                                .as_ptr()
                                .cast(),
                        ));
                        return Err(-1);
                    }
                    stack_len = s.stack_level_tab[catch_pos as usize];
                    if *bc_buf.add(catch_pos as usize) != opcodes::OP_catch {
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

#[deprecated(note = "use JSFunctionDef::compute_stack_size() instead")]
pub unsafe fn compute_stack_size(ctx: *mut JSContext, fd: *mut JSFunctionDef) -> Result<u16, i32> {
    (*fd).compute_stack_size(ctx.as_mut().unwrap())
}
