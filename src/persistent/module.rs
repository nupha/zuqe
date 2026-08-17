// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    module::{ExportEntry, ExportType, ImportEntry, ModuleDef, ModuleDependency},
    persistent::{BytecodeSerializeError, DeserializeCtx, SerializeCtx},
    value::JSValue,
};

pub(crate) fn serialize_module(
    sc: &mut ScopedContext,
    m: &ModuleDef,
    ctx: &mut SerializeCtx,
) -> Result<(), BytecodeSerializeError> {
    if m.is_native() {
        return Err(BytecodeSerializeError::UnsupportedObject(
            "native module (init_func) is not persistable".into(),
        ));
    }

    let md = &**m;

    ctx.write_atom(sc, md.module_name)?;
    ctx.write_u8(md.func_created as u8);

    ctx.write_u32(md.dependencies.len() as u32);
    for d in md.dependencies.iter() {
        ctx.write_atom(sc, d.module_name)?;
    }

    ctx.write_u32(md.import_entries.len() as u32);
    for e in md.import_entries.iter() {
        ctx.write_u16(e.var_idx);
        ctx.write_atom(sc, e.import_name)?;
        ctx.write_i32(e.req_module_idx);
    }

    ctx.write_u32(md.export_entries.len() as u32);
    for e in md.export_entries.iter() {
        let (kind, var_idx, req_idx) = match e.export_type {
            ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, .. } => (0u8, var_idx, -1i32),
            ExportType::JS_EXPORT_TYPE_INDIRECT(idx) => (1u8, 0u16, idx),
        };
        ctx.write_u8(kind);
        ctx.write_u16(var_idx);
        ctx.write_i32(req_idx);
        ctx.write_atom(sc, e.local_name)?;
        ctx.write_atom(sc, e.export_name)?;
    }

    ctx.write_u32(md.star_export_entries.len() as u32);
    for s in md.star_export_entries.iter() {
        ctx.write_u16(*s);
    }

    ctx.serialize_value(sc, md.bytecode)?;

    match md.module_ns {
        Some(o) => {
            ctx.write_u8(1);
            ctx.serialize_value(sc, JSValue::from(o))?;
        }
        None => ctx.write_u8(0),
    }

    Ok(())
}

pub(crate) fn deserialize_module(
    ctx: &mut DeserializeCtx,
) -> Result<ModuleDef, BytecodeSerializeError> {
    let module_name: Atom = ctx.read_atom()?;
    let func_created = ctx.read_u8()? != 0;

    let n = ctx.read_u32()? as usize;
    let mut dependencies = Vec::with_capacity(n);
    for _ in 0..n {
        let name: Atom = ctx.read_atom()?;
        dependencies.push(ModuleDependency {
            module_name: name,
            module: None,
            attributes: JSValue::UNDEFINED,
        });
    }

    let n = ctx.read_u32()? as usize;
    let mut import_entries = Vec::with_capacity(n);
    for _ in 0..n {
        let var_idx = ctx.read_u16()?;
        let import_name: Atom = ctx.read_atom()?;
        let req_module_idx = ctx.read_i32()?;
        import_entries.push(ImportEntry {
            var_idx,
            import_name,
            req_module_idx,
        });
    }

    let n = ctx.read_u32()? as usize;
    let mut export_entries = Vec::with_capacity(n);
    for _ in 0..n {
        let kind = ctx.read_u8()?;
        let var_idx = ctx.read_u16()?;
        let req_module_idx = ctx.read_i32()?;
        let local_name: Atom = ctx.read_atom()?;
        let export_name: Atom = ctx.read_atom()?;
        let export_type = match kind {
            0 => ExportType::JS_EXPORT_TYPE_LOCAL {
                var_idx,
                var_ref: None,
            },
            _ => ExportType::JS_EXPORT_TYPE_INDIRECT(req_module_idx),
        };
        export_entries.push(ExportEntry {
            export_type,
            local_name,
            export_name,
        });
    }

    let n = ctx.read_u32()? as usize;
    let mut star_export_entries = Vec::with_capacity(n);
    for _ in 0..n {
        star_export_entries.push(ctx.read_u16()?);
    }

    let bc_val = ctx.deserialize_value()?;
    let bytecode = bc_val
        .as_bytecode()
        .ok_or(BytecodeSerializeError::InvalidValue(
            "module entry is not bytecode".into(),
        ))?;

    let module_ns = match ctx.read_u8()? {
        1 => ctx.deserialize_value()?.as_object(),
        _ => None,
    };

    let mut md = ModuleDef::new(ctx.ctx, module_name)
        .map_err(|_| BytecodeSerializeError::InvalidValue("oom in module rebuild".into()))?;
    md.dependencies = dependencies;
    md.import_entries = import_entries;
    md.export_entries = export_entries;
    md.star_export_entries = star_export_entries;
    md.bytecode = JSValue::from(bytecode);
    md.func_created = func_created;
    md.module_ns = module_ns;

    Ok(md)
}
