// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::UnsafeCell, collections::HashMap, mem};

use gc_lite::GcRef;

use crate::{
    JSValue, ScopedContext,
    atom::Atom,
    bytecode::{OpcodeFormat, opcode_info_final},
    function::{
        Bytecode, BytecodeAttrs, BytecodeDebugInfo, BytecodeImpl, ClosureVar,
        DEFINE_GLOBAL_LEX_VAR, FunctionKind, GcScopeKind, JSVarDef,
    },
    stack_frame::ExecMode,
};

use super::{BytecodeSerializeError, DeserializeCtx, SerializeCtx};

/// An opcode of these formats carries a `u32` atom id at `pos + 1` inside the
/// binary. `small-int` atoms are skipped (kept verbatim) because their id is
/// context-independent and `write_atom` cannot encode them.
fn opcode_has_atom(fmt: OpcodeFormat) -> bool {
    matches!(
        fmt,
        OpcodeFormat::ATOM
            | OpcodeFormat::ATOM_U8
            | OpcodeFormat::ATOM_U16
            | OpcodeFormat::ATOM_LABEL_U8
            | OpcodeFormat::ATOM_LABEL_U16
    )
}

/// Collect every atom id referenced inside the `binary` (opcode operands,
/// variable names, closure variable names). Const/small-int atoms are excluded
/// from the symbol table: const atoms are re-encoded as plain strings, and
/// small-int atoms are context-independent and kept verbatim.
fn collect_opcode_atoms(bc: &BytecodeImpl) -> Vec<u32> {
    let bin = &bc.binary;
    let mut ids: Vec<u32> = Vec::new();

    // opcode segment is the LAST section of the binary, running from
    // `opcodes_start` to the end (layout:
    // [consts][vardefs][global/closure vars][opcodes]).
    let mut pos = bc.opcodes_start as usize;
    while pos + 5 <= bin.len() {
        let op = bin[pos];
        let info = opcode_info_final(op);
        if opcode_has_atom(info.fmt) {
            let id = u32::from_le_bytes(bin[pos + 1..pos + 5].try_into().unwrap());
            if id != 0 && !ids.contains(&id) {
                ids.push(id);
            }
        }
        pos += info.size as usize;
    }

    // variable definitions: `var_name` is at offset 0 of each `JSVarDef`.
    let vd_size = mem::size_of::<JSVarDef>();
    let vd_n = bc.var_count as usize + bc.arg_count as usize;
    for i in 0..vd_n {
        let off = bc.vardefs_start as usize + i * vd_size;
        let id = u32::from_le_bytes(bin[off..off + 4].try_into().unwrap());
        if id != 0 && !ids.contains(&id) {
            ids.push(id);
        }
    }

    // closure variables: `var_name` is at `offset_of!(ClosureVar, var_name)`.
    // The closure-vars binary section is laid out as
    // `[global_var_ref_count globals][closure_var_count closures]`.
    let cv_size = mem::size_of::<ClosureVar>();
    let cv_name_off = mem::offset_of!(ClosureVar, var_name);
    let gv_count = bc.global_var_ref_count as usize;
    for i in 0..(gv_count + bc.closure_var_count as usize) {
        let off = bc.closure_var_start as usize + i * cv_size + cv_name_off;
        let id = u32::from_le_bytes(bin[off..off + 4].try_into().unwrap());
        if id != 0 && !ids.contains(&id) {
            ids.push(id);
        }
    }

    ids
}

/// Rewrite every atom reference in `bin` (a slice of the binary *without* the
/// leading consts section) from its original atom id to its symbol-table index.
/// `consts_len` is subtracted so offsets map into `bin`.
fn rewrite_atoms_to_symidx(
    bin: &mut [u8],
    opcodes_start: u32,
    vardefs_start: i32,
    var_count: u16,
    arg_count: u16,
    closure_var_start: i32,
    closure_var_count: u16,
    global_var_ref_count: u16,
    consts_len: usize,
    sym_map: &HashMap<u32, u32>,
) {
    let mut pos = (opcodes_start as usize).saturating_sub(consts_len);
    while pos + 5 <= bin.len() {
        let op = bin[pos];
        let info = opcode_info_final(op);
        if opcode_has_atom(info.fmt) {
            let id = u32::from_le_bytes(bin[pos + 1..pos + 5].try_into().unwrap());
            if let Some(&sym) = sym_map.get(&id) {
                bin[pos + 1..pos + 5].copy_from_slice(&sym.to_le_bytes());
            }
        }
        pos += info.size as usize;
    }
    let vd_size = mem::size_of::<JSVarDef>();
    let vd_n = var_count as usize + arg_count as usize;
    for i in 0..vd_n {
        let off = (vardefs_start as usize).saturating_sub(consts_len) + i * vd_size;
        let id = u32::from_le_bytes(bin[off..off + 4].try_into().unwrap());
        if let Some(&sym) = sym_map.get(&id) {
            bin[off..off + 4].copy_from_slice(&sym.to_le_bytes());
        }
    }
    let cv_size = mem::size_of::<ClosureVar>();
    let cv_name_off = mem::offset_of!(ClosureVar, var_name);
    // Section layout: `[global_var_ref_count globals][closure_var_count closures]`.
    let gv_count = global_var_ref_count as usize;
    for i in 0..(gv_count + closure_var_count as usize) {
        let off =
            (closure_var_start as usize).saturating_sub(consts_len) + i * cv_size + cv_name_off;
        let id = u32::from_le_bytes(bin[off..off + 4].try_into().unwrap());
        if let Some(&sym) = sym_map.get(&id) {
            bin[off..off + 4].copy_from_slice(&sym.to_le_bytes());
        }
    }
}

/// Inverse of [`rewrite_atoms_to_symidx`]: replace symbol-table indices with
/// the freshly rebuilt atom ids. Anything not a valid index (small-int atoms)
/// is kept verbatim.
fn rewrite_symidx_to_atoms(
    bin: &mut [u8],
    opcodes_start: u32,
    vardefs_start: i32,
    var_count: u16,
    arg_count: u16,
    closure_var_start: i32,
    closure_var_count: u16,
    global_var_ref_count: u16,
    consts_len: usize,
    sym: &[Atom],
) {
    let mut pos = (opcodes_start as usize).saturating_sub(consts_len);
    while pos + 5 <= bin.len() {
        let op = bin[pos];
        let info = opcode_info_final(op);
        if opcode_has_atom(info.fmt) {
            let v = u32::from_le_bytes(bin[pos + 1..pos + 5].try_into().unwrap());
            if (v as usize) < sym.len() {
                bin[pos + 1..pos + 5].copy_from_slice(&sym[v as usize].id().to_le_bytes());
            }
        }
        pos += info.size as usize;
    }
    let vd_size = mem::size_of::<JSVarDef>();
    let vd_n = var_count as usize + arg_count as usize;
    for i in 0..vd_n {
        let off = (vardefs_start as usize).saturating_sub(consts_len) + i * vd_size;
        let v = u32::from_le_bytes(bin[off..off + 4].try_into().unwrap());
        if (v as usize) < sym.len() {
            bin[off..off + 4].copy_from_slice(&sym[v as usize].id().to_le_bytes());
        }
    }
    let cv_size = mem::size_of::<ClosureVar>();
    let cv_name_off = mem::offset_of!(ClosureVar, var_name);
    // Section layout: `[global_var_ref_count globals][closure_var_count closures]`.
    let gv_count = global_var_ref_count as usize;
    for i in 0..(gv_count + closure_var_count as usize) {
        let off =
            (closure_var_start as usize).saturating_sub(consts_len) + i * cv_size + cv_name_off;
        let v = u32::from_le_bytes(bin[off..off + 4].try_into().unwrap());
        if (v as usize) < sym.len() {
            let a = sym[v as usize].id();
            bin[off..off + 4].copy_from_slice(&a.to_le_bytes());
        }
    }
}

/// Serialize a compiled `Bytecode` into the current output context.
pub(super) fn serialize_bytecode(
    sc: &mut ScopedContext,
    bc: &Bytecode,
    ctx: &mut SerializeCtx,
) -> Result<(), BytecodeSerializeError> {
    let impl_ref: &BytecodeImpl = &*bc;
    serialize_bytecode_impl(sc, impl_ref, ctx)
}

fn serialize_bytecode_impl(
    cx: &mut ScopedContext,
    bc: &BytecodeImpl,
    out: &mut SerializeCtx,
) -> Result<(), BytecodeSerializeError> {
    // func_name (atom).
    out.write_atom(cx, bc.func_name)?;

    // attrs (all fields serialized so `BytecodeImpl` can be rebuilt from them).
    out.write_u8(bc.attrs.func_kind as u8);
    out.write_u8(bc.attrs.has_prototype as u8);
    out.write_u8(bc.attrs.has_simple_parameter_list as u8);
    out.write_u8(bc.attrs.is_derived_class_constructor as u8);
    out.write_u8(bc.attrs.new_target_allowed as u8);
    out.write_u8(bc.attrs.super_call_allowed as u8);
    out.write_u8(bc.attrs.super_allowed as u8);
    out.write_u8(bc.attrs.arguments_allowed as u8);
    out.write_u8(bc.attrs.backtrace_barrier as u8);
    out.write_u16(bc.attrs.defined_arg_count);
    out.write_u8(bc.attrs.need_home_object as u8);
    out.write_u8(bc.attrs.exec_mode.bits());
    out.write_u8(bc.attrs.gc_scope_kind as u8);

    out.write_u8(bc.is_direct_or_indirect_eval as u8);
    out.write_u16(bc.stack_size);
    out.write_u16(bc.local_buf_size);
    out.write_u16(bc.arg_count);
    out.write_u16(bc.var_count);
    out.write_u16(bc.global_var_ref_count);
    out.write_u16(bc.closure_var_count);

    // The `binary` is self-contained: every atom reference (opcode operands,
    // variable names, closure variable names) is redirected through a symbol
    // table, and the consts section is serialized element-by-element so string
    // constants are rebuilt in the target context. This makes the blob usable
    // regardless of the source/destination context or runtime.
    let atom_ids = collect_opcode_atoms(bc);
    let sym_map: HashMap<u32, u32> = atom_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i as u32))
        .collect();

    // symbol table
    out.write_u32(atom_ids.len() as u32);
    for &id in &atom_ids {
        out.write_atom(cx, Atom::from_id(id))?;
    }

    let consts_len =
        bc.consts_section.map(|c| c.1 as usize).unwrap_or(0) * std::mem::size_of::<u64>();
    let consts_count = bc.consts_section.map(|c| c.1 as u32).unwrap_or(0);

    // consts section — serialized element-by-element (self-contained values)
    out.write_u32(consts_count);
    for i in 0..consts_count {
        let v = bc
            .load_const(i as usize)
            .ok_or_else(|| BytecodeSerializeError::InvalidValue("const slot missing".into()))?;
        out.serialize_value(cx, v)?;
    }

    // rest of the binary (everything after the consts section), atom ids
    // rewritten to symbol-table indices
    let rest = &bc.binary[consts_len..];
    let mut new_rest = rest.to_vec();
    rewrite_atoms_to_symidx(
        &mut new_rest,
        bc.opcodes_start,
        bc.vardefs_start,
        bc.var_count,
        bc.arg_count,
        bc.closure_var_start,
        bc.closure_var_count,
        bc.global_var_ref_count,
        consts_len,
        &sym_map,
    );
    out.write_u32(new_rest.len() as u32);
    out.write_bytes(&new_rest);

    // snapshot of global lexical variable values referenced by this bytecode
    // (`global_var_refs`). Only LEXICAL bindings (let/const, declared on the
    // global_var_obj) are persisted; property globals (Math, JSON, ...) are
    // resolved from the target runtime's own global object so built-in natives
    // never enter the blob. Values are not carried by the binary (they live in
    // the source context's global lexical scope), so they are serialized here
    // as detached copies. On deserialize they are re-bound into the target
    // context, making the blob independent of the source context/closure chain
    // (same detach semantics QuickJS uses for JS_CLOSURE_GLOBAL).
    let gv = bc.global_var_refs();
    out.write_u16(gv.len() as u16);
    for cv in gv {
        let name = cv.var_name;
        out.write_atom(cx, name)?;
        if cx.global_var_obj().has_shape_property(name) {
            let r = cx.get_global_var(name, false);
            match r {
                Ok(v) => {
                    out.write_u8(1);
                    out.serialize_value(cx, v)?;
                }
                Err(_) => {
                    out.write_u8(0);
                }
            }
        } else {
            out.write_u8(0);
        }
    }

    // consts_section: Option<(u16, u16)>
    match bc.consts_section {
        Some((a, b)) => {
            out.write_u8(1);
            out.write_u16(a);
            out.write_u16(b);
        }
        None => out.write_u8(0),
    }
    out.write_i32(bc.vardefs_start);
    out.write_i32(bc.closure_var_start);
    out.write_u32(bc.opcodes_start);

    // debug (filename atom + line number; pc2line/source dropped).
    match bc.debug {
        Some(ref d) => {
            out.write_u8(1);
            out.write_atom(cx, d.filename)?;
            out.write_i32(d.line_num);
        }
        None => out.write_u8(0),
    }
    
    Ok(())
}

/// Rebuild a `Bytecode` from the stream. Allocates a fresh `BytecodeImpl` and
/// wraps it as a callable `Bytecode` value.
pub(crate) fn deserialize_bytecode(
    inp: &mut DeserializeCtx,
) -> Result<Bytecode, BytecodeSerializeError> {
    let func_name = inp.read_atom()?;
    let func_kind = func_kind_from_u8(inp.read_u8()?)?;
    let has_prototype = inp.read_u8()? != 0;
    let has_simple_parameter_list = inp.read_u8()? != 0;
    let is_derived_class_constructor = inp.read_u8()? != 0;
    let new_target_allowed = inp.read_u8()? != 0;
    let super_call_allowed = inp.read_u8()? != 0;
    let super_allowed = inp.read_u8()? != 0;
    let arguments_allowed = inp.read_u8()? != 0;
    let backtrace_barrier = inp.read_u8()? != 0;
    let defined_arg_count = inp.read_u16()?;
    let need_home_object = inp.read_u8()? != 0;
    let exec_mode = ExecMode::from_bits_retain(inp.read_u8()?);
    let gc_scope_kind = gc_scope_from_u8(inp.read_u8()?)?;

    let attrs = BytecodeAttrs {
        func_kind,
        has_prototype,
        has_simple_parameter_list,
        is_derived_class_constructor,
        new_target_allowed,
        super_call_allowed,
        super_allowed,
        arguments_allowed,
        backtrace_barrier,
        defined_arg_count,
        need_home_object,
        exec_mode,
        gc_scope_kind,
    };

    let is_direct_or_indirect_eval = inp.read_u8()? != 0;
    let stack_size = inp.read_u16()?;
    let local_buf_size = inp.read_u16()?;
    let arg_count = inp.read_u16()?;
    let var_count = inp.read_u16()?;
    let global_var_ref_count = inp.read_u16()?;
    let closure_var_count = inp.read_u16()?;

    // symbol table — rebuilt in this context
    let sym_count = inp.read_u32()? as usize;
    let mut sym: Vec<Atom> = Vec::with_capacity(sym_count);
    for _ in 0..sym_count {
        sym.push(inp.read_atom()?);
    }

    let consts_count = inp.read_u32()? as usize;
    let consts_len = consts_count * std::mem::size_of::<u64>();
    let mut binary: Vec<u8> = vec![0u8; consts_len];
    for i in 0..consts_count {
        let v = inp.deserialize_value()?;
        let u = unsafe { std::mem::transmute::<crate::JSValue, u64>(v) };
        binary[i * 8..i * 8 + 8].copy_from_slice(&u.to_le_bytes());
    }

    // rest of the binary, symbol indices rewritten back to fresh atom ids.
    // This is read BEFORE consts_section/vardefs_start/... to match the order
    // used by `serialize_bytecode_impl`.
    let rest_len = inp.read_u32()? as usize;
    let mut rest = inp.read_bytes(rest_len)?.to_vec();

    // global lexical variable value snapshots (see serialize side). Read here,
    // before `consts_section`/metadata, to match the serialize order so the
    // byte stream stays aligned. The bindings are re-created after `bc_impl`.
    let gv_snap_count = inp.read_u16()? as usize;
    let mut gv_snap: Vec<(Atom, Option<JSValue>)> = Vec::with_capacity(gv_snap_count);
    for _ in 0..gv_snap_count {
        let name = inp.read_atom()?;
        let present = inp.read_u8()? != 0;
        if present {
            let v = inp.deserialize_value()?;
            gv_snap.push((name, Some(v)));
        } else {
            gv_snap.push((name, None));
        }
    }

    // metadata needed to relocate atom references inside `rest`
    let consts_section = if inp.read_u8()? != 0 {
        Some((inp.read_u16()?, inp.read_u16()?))
    } else {
        None
    };
    let vardefs_start = inp.read_i32()?;
    let closure_var_start = inp.read_i32()?;
    let opcodes_start = inp.read_u32()?;

    rewrite_symidx_to_atoms(
        &mut rest,
        opcodes_start,
        vardefs_start,
        var_count,
        arg_count,
        closure_var_start,
        closure_var_count,
        global_var_ref_count,
        consts_len,
        &sym,
    );
    binary.extend_from_slice(&rest);
    let binary: Box<[u8]> = binary.into_boxed_slice();

    let (debug_filename, debug_line_num) = if inp.read_u8()? != 0 {
        (inp.read_atom()?, inp.read_i32()?)
    } else {
        (Atom::NULL, 0)
    };

    let bc_impl = BytecodeImpl {
        realm: inp.ctx.ctx_ptr,
        func_name,
        attrs,
        is_direct_or_indirect_eval,
        stack_size,
        local_buf_size,
        binary,
        consts_section,
        vardefs_start,
        arg_count,
        var_count,
        global_var_ref_count,
        closure_var_start,
        closure_var_count,
        opcodes_start,
        debug: Some(BytecodeDebugInfo {
            filename: debug_filename,
            line_num: debug_line_num,
            source: None,
            pc2line_buf: None,
            #[cfg(debug_assertions)]
            debug_setting: Default::default(),
        }),

        #[cfg(feature = "inline_cache")]
        inline_cache: UnsafeCell::new(Box::new([])),
    };

    // Re-create the global lexical bindings referenced by this bytecode using
    // the detached value snapshots (QuickJS JS_CLOSURE_GLOBAL detach semantics).
    // This makes the blob work in any context, independent of the source closure
    // chain or whether the global lexical var already exists there.
    for (name, val) in gv_snap {
        if let Some(v) = val {
            // Re-create the global lexical binding. If it already exists in the
            // target context, `define_global_var` errors — ignore that and just
            // overwrite the value via `set_global_var` (detached copy semantics).
            // Clear any pending exception left by the (expected) redeclaration error.
            let _ = inp
                .ctx
                .define_global_var(name, DEFINE_GLOBAL_LEX_VAR as u32);
            inp.ctx.clear_exception();
            let _ = inp.ctx.set_global_var(name, v, 1u8);
        }
    }

    let gc_ref: GcRef<BytecodeImpl> = inp
        .ctx
        .alloc_local(bc_impl)
        .map_err(|_| BytecodeSerializeError::InvalidValue("cannot alloc BytecodeImpl".into()))?;
    Ok(Bytecode::from(gc_ref))
}

fn func_kind_from_u8(v: u8) -> Result<FunctionKind, BytecodeSerializeError> {
    match v {
        0 => Ok(FunctionKind::Normal),
        1 => Ok(FunctionKind::Generator),
        2 => Ok(FunctionKind::Async),
        3 => Ok(FunctionKind::AsyncGenerator),
        _ => Err(BytecodeSerializeError::InvalidTag(v)),
    }
}

fn gc_scope_from_u8(v: u8) -> Result<GcScopeKind, BytecodeSerializeError> {
    match v {
        0 => Ok(GcScopeKind::Full),
        1 => Ok(GcScopeKind::Light),
        2 => Ok(GcScopeKind::None),
        _ => Err(BytecodeSerializeError::InvalidTag(v)),
    }
}
