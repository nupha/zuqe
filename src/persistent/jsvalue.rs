// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Atom, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION, JSValue, JSValueTag, ScopedContext,
    atom::AtomKind,
    bigint::BigIntRef,
    function::{BFuncPayload, VarRef},
    object::{Object, ObjectPayload},
    string::StringRef,
};

use super::{
    BYTECODE_SERIALIZE_MAGIC, BYTECODE_SERIALIZE_VERSION, BytecodeSerializeError,
    atom_lut::atom_kind_from_u8,
};

/// Write-side context. Owns the output buffer plus the dedup state used to
/// keep serialized output compact and identity-preserving.
pub struct SerializeCtx {
    /// Encoded bytes.
    pub(super) buffer: Vec<u8>,
    /// Per-blob atom table: maps a registered atom id to its first
    /// occurrence index so repeated property keys are emitted once.
    pub(super) atom_count: usize,
    pub(super) atom_lut: std::collections::HashMap<u32, usize>,
    /// Reference table: tracks every object already serialized, indexed
    /// by occurrence order, so shared references / cycles emit a back-ref
    /// (`tag 3`) instead of recursing forever.
    pub(super) seen_obj: Vec<Object>,
}

impl SerializeCtx {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            atom_count: 0,
            atom_lut: std::collections::HashMap::new(),
            seen_obj: Vec::new(),
        }
    }

    #[inline]
    pub(crate) fn write_u8(&mut self, v: u8) {
        self.buffer.push(v);
    }

    #[inline]
    pub(crate) fn write_u32(&mut self, v: u32) {
        self.buffer.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_u64(&mut self, v: u64) {
        self.buffer.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_u16(&mut self, v: u16) {
        self.buffer.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_i32(&mut self, v: i32) {
        self.buffer.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_i64(&mut self, v: i64) {
        self.buffer.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_f64(&mut self, v: f64) {
        self.buffer.extend_from_slice(&v.to_bits().to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_bytes(&mut self, b: &[u8]) {
        self.buffer.extend_from_slice(b);
    }

    /// Write a UTF-8 string with a u32 length prefix.
    #[inline]
    pub(crate) fn write_string(&mut self, s: &str) {
        self.write_u32(s.len() as u32);
        self.write_bytes(s.as_bytes());
    }

    // --- object reference table ---------------------------------------

    /// Record `obj` in the reference table and return its index, registering
    /// it before serialization of its contents so back-references resolve.
    pub(crate) fn register_object(&mut self, obj: Object) -> u32 {
        let idx = self.seen_obj.len() as u32;
        self.seen_obj.push(obj);
        idx
    }

    /// Look up `obj` in the reference table. Returns `Some(idx)` if already
    /// serialized (caller should emit a `tag 3` back-reference instead).
    pub(crate) fn lookup_object(&self, obj: Object) -> Option<u32> {
        self.seen_obj
            .iter()
            .position(|o| *o == obj)
            .map(|i| i as u32)
    }

    // --- value writer ------------------------------------------------------

    /// Serialize a single `JSValue`, appending to `self.buffer`.
    pub(crate) fn serialize_value(
        &mut self,
        sc: &mut ScopedContext,
        val: JSValue,
    ) -> Result<(), BytecodeSerializeError> {
        match val.tag() {
            JSValueTag::Undefined => {
                self.write_u8(0);
                Ok(())
            }
            JSValueTag::Null => {
                self.write_u8(1);
                Ok(())
            }
            JSValueTag::Bool => {
                self.write_u8(2);
                self.write_u8(val.as_bool().unwrap() as u8);
                Ok(())
            }
            JSValueTag::Int => {
                self.write_u8(4);
                self.write_i64(val.as_int().unwrap() as i64);
                Ok(())
            }
            JSValueTag::Float64 => {
                self.write_u8(5);
                self.write_f64(val.as_float().unwrap());
                Ok(())
            }
            JSValueTag::String | JSValueTag::StringRope => {
                self.write_u8(6);
                self.serialize_string(val.as_string(false).unwrap())
            }
            JSValueTag::Symbol => {
                self.write_u8(7);
                self.serialize_symbol(val.as_symbol(false).unwrap())
            }
            JSValueTag::Object => {
                let obj = val.as_object().ok_or_else(|| {
                    BytecodeSerializeError::InvalidValue("object without payload".into())
                })?;
                // Functions / closures and native exotics are objects
                // whose behaviour is not reflected in own properties, so they
                // cannot be byte-reconstructed. Surface this explicitly rather
                // than persisting a non-callable husk.
                let cid = obj.class_id();
                if cid == JS_CLASS_C_FUNCTION {
                    // QuickJS-style native reference: only the function's JS
                    // `name` is recorded. On restore the target runtime must
                    // expose that name on the global object; if it does not, the
                    // reference cannot be resolved (mirrors QuickJS where native
                    // funcs are bound by name at load time, never byte-built).
                    let name_val = obj.get_property_str(sc, "name");
                    let name = match name_val {
                        Ok(v) if v.is_string() => v.as_string(false).unwrap(),
                        _ => {
                            return Err(BytecodeSerializeError::UnsupportedObject(
                                "native C function without a resolvable name".into(),
                            ));
                        }
                    };
                    self.write_u8(0xFD);
                    self.serialize_string(name)?;
                    return Ok(());
                }
                // closure function object carries its Bytecode + captured vars
                // + home_object, none of which appear as own properties, so serialize separately.
                if cid == JS_CLASS_BYTECODE_FUNCTION {
                    self.write_u8(0xFC);
                    let payload = match obj.payload() {
                        ObjectPayload::BYTECODE_FUNCTION(p) => p,
                        _ => unreachable!("class id matches but payload is not bytecode function"),
                    };
                    crate::persistent::bytecode::serialize_bytecode(sc, &payload.bytecode(), self)?;
                    let captured = payload.captured_vars();
                    self.write_u32(captured.len() as u32);
                    for vref in captured.iter() {
                        self.serialize_value(sc, vref.value())?;
                    }
                    match payload.home_object() {
                        Some(h) => {
                            self.write_u8(1);
                            self.serialize_value(sc, JSValue::from(h))?;
                        }
                        None => self.write_u8(0),
                    }
                    return Ok(());
                }
                // shared reference / cycle handling. If we have already
                // started serializing this exact object, emit a back-reference
                // (`tag 3`) instead of recursing. The object is registered
                // *before* its contents so a self/back reference resolves.
                if let Some(idx) = self.lookup_object(obj) {
                    self.write_u8(3);
                    self.write_u32(idx);
                    return Ok(());
                }
                self.register_object(obj);
                self.write_u8(8);
                crate::persistent::object::serialize_object(sc, obj, self)
            }
            JSValueTag::FunctionBytecode => {
                let bc = val.as_bytecode().ok_or_else(|| {
                    BytecodeSerializeError::InvalidValue("bytecode without payload".into())
                })?;
                self.write_u8(9);
                crate::persistent::bytecode::serialize_bytecode(sc, &bc, self)
            }
            #[cfg(feature = "big_int")]
            JSValueTag::BigInt => {
                self.write_u8(29);
                self.serialize_bigint(val.as_bigint().unwrap())
            }
            JSValueTag::Module => {
                let m = val.as_module().ok_or(BytecodeSerializeError::InvalidValue(
                    "module without backing def".into(),
                ))?;
                self.write_u8(30);
                crate::persistent::module::serialize_module(sc, &m, self)
            }
            JSValueTag::CatchOffset | JSValueTag::Uninitialized | JSValueTag::Exception => Err(
                BytecodeSerializeError::InvalidValue("internal value not serializable".into()),
            ),
        }
    }

    fn serialize_string(&mut self, s: StringRef) -> Result<(), BytecodeSerializeError> {
        let bytes = s.encode_utf8_bytes(false);
        self.write_u32(bytes.len() as u32);
        self.write_bytes(&bytes);
        Ok(())
    }

    fn serialize_symbol(&mut self, s: StringRef) -> Result<(), BytecodeSerializeError> {
        let kind = s.atom_info().map(|a| a.kind()).unwrap_or(AtomKind::Symbol);
        let bytes = s.encode_utf8_bytes(false);
        self.write_u8(kind as u8);
        self.write_u32(bytes.len() as u32);
        self.write_bytes(&bytes);
        Ok(())
    }

    #[cfg(feature = "big_int")]
    fn serialize_bigint(&mut self, b: BigIntRef) -> Result<(), BytecodeSerializeError> {
        let i = b.as_impl();
        let sign = i.sign() as u8;
        let limbs = i.limbs();
        self.write_u8(sign);
        self.write_u32(limbs.len() as u32);
        for &limb in limbs {
            self.write_u64(limb);
        }
        Ok(())
    }
}

/// Read-side context. Owns the input slice, a cursor and a `seen` table that
/// preserves object identity / shared references.
pub struct DeserializeCtx<'a, 'b> {
    pub(super) ctx: &'a mut ScopedContext<'b>,
    pub(super) buf: &'a [u8],
    pub(super) pos: usize,
    /// Objects already materialized, indexed by their occurrence order in the
    /// stream so back-references keep identity.
    pub(super) seen: Vec<JSValue>,
    /// Atom table: atoms deserialized so far, indexed by their occurrence
    /// order so back-references resolve to the same `Atom`.
    pub(super) atom_table: Vec<Atom>,
}

impl<'a, 'b> DeserializeCtx<'a, 'b> {
    pub fn new(ctx: &'a mut ScopedContext<'b>, buf: &'a [u8]) -> Self {
        Self {
            ctx,
            buf,
            pos: 0,
            seen: Vec::new(),
            atom_table: Vec::new(),
        }
    }

    #[inline]
    pub(crate) fn read_u8(&mut self) -> Result<u8, BytecodeSerializeError> {
        if self.pos >= self.buf.len() {
            return Err(BytecodeSerializeError::Eof);
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    #[inline]
    pub(crate) fn read_u32(&mut self) -> Result<u32, BytecodeSerializeError> {
        if self.pos + 4 > self.buf.len() {
            return Err(BytecodeSerializeError::Eof);
        }
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 4]);
        self.pos += 4;
        Ok(u32::from_le_bytes(b))
    }

    #[inline]
    pub(crate) fn read_u16(&mut self) -> Result<u16, BytecodeSerializeError> {
        if self.pos + 2 > self.buf.len() {
            return Err(BytecodeSerializeError::Eof);
        }
        let mut b = [0u8; 2];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 2]);
        self.pos += 2;
        Ok(u16::from_le_bytes(b))
    }

    #[inline]
    pub(crate) fn read_i32(&mut self) -> Result<i32, BytecodeSerializeError> {
        Ok(self.read_u32()? as i32)
    }

    #[inline]
    pub(crate) fn read_u64(&mut self) -> Result<u64, BytecodeSerializeError> {
        if self.pos + 8 > self.buf.len() {
            return Err(BytecodeSerializeError::Eof);
        }
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(u64::from_le_bytes(b))
    }

    #[inline]
    pub(crate) fn read_i64(&mut self) -> Result<i64, BytecodeSerializeError> {
        Ok(self.read_u64()? as i64)
    }

    #[inline]
    pub(crate) fn read_f64(&mut self) -> Result<f64, BytecodeSerializeError> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    #[inline]
    pub(crate) fn read_bytes(&mut self, n: usize) -> Result<Vec<u8>, BytecodeSerializeError> {
        if self.pos + n > self.buf.len() {
            return Err(BytecodeSerializeError::Eof);
        }
        let v = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(v)
    }

    #[inline]
    pub(crate) fn read_string(&mut self) -> Result<String, BytecodeSerializeError> {
        let n = self.read_u32()? as usize;
        let b = self.read_bytes(n)?;
        String::from_utf8(b).map_err(|e| BytecodeSerializeError::Encode(e.to_string()))
    }

    // --- atom table --------------------------------------------------------

    // --- reference table ---------------------------------------------------

    pub(crate) fn register_seen(&mut self, v: JSValue) -> JSValue {
        self.seen.push(v);
        v
    }

    #[inline]
    fn read_backref(&mut self) -> Result<JSValue, BytecodeSerializeError> {
        let idx = self.read_u32()? as usize;
        self.seen
            .get(idx)
            .copied()
            .ok_or(BytecodeSerializeError::InvalidValue(format!(
                "bad backref {idx}"
            )))
    }

    // --- value reader ------------------------------------------------------

    /// Deserialize a single `JSValue` from the current cursor.
    pub(crate) fn deserialize_value(&mut self) -> Result<JSValue, BytecodeSerializeError> {
        let tag = self.read_u8()?;
        match tag {
            0 => Ok(JSValue::UNDEFINED),
            1 => Ok(JSValue::NULL),
            2 => {
                let b = self.read_u8()?;
                Ok(JSValue::from(b != 0))
            }
            // tag 3 = back-reference to a previously seen object.
            3 => {
                let idx = self.read_u32()? as usize;
                self.seen
                    .get(idx)
                    .copied()
                    .ok_or(BytecodeSerializeError::InvalidTag(0xFE))
            }
            4 => {
                let v = self.read_i64()? as i32;
                Ok(JSValue::from(v))
            }
            5 => {
                let v = self.read_f64()?;
                Ok(JSValue::make_float(v))
            }
            6 => self.deserialize_string().map(|v| self.register_seen(v)),
            7 => self.deserialize_symbol().map(|v| self.register_seen(v)),
            29 => {
                #[cfg(feature = "big_int")]
                {
                    self.deserialize_bigint().map(|v| self.register_seen(v))
                }
                #[cfg(not(feature = "big_int"))]
                {
                    Err(BytecodeSerializeError::InvalidTag(29))
                }
            }
            // compiled bytecode — reconstructed and wrapped as a callable `Bytecode` value.
            9 => {
                let bc = crate::persistent::bytecode::deserialize_bytecode(self)?;
                Ok(JSValue::from(bc))
            }
            30 => {
                let m = crate::persistent::module::deserialize_module(self)?;
                Ok(self.register_seen(JSValue::from(m)))
            }
            // closure function object (captured vars + home_object)
            0xFC => {
                let bc = crate::persistent::bytecode::deserialize_bytecode(self)?;
                let n = self.read_u32()? as usize;
                let mut captured: Vec<VarRef> = Vec::with_capacity(n);
                for _ in 0..n {
                    let v = self.deserialize_value()?;
                    let vref = VarRef::create(self.ctx, v).map_err(|_| {
                        BytecodeSerializeError::InvalidValue("oom in closure var".into())
                    })?;
                    captured.push(vref);
                }
                let home_object = match self.read_u8()? {
                    1 => self.deserialize_value()?.as_object(),
                    _ => None,
                };
                let mut payload = BFuncPayload::new(bc, captured);
                if let Some(h) = home_object {
                    payload.set_home_object(Some(h));
                }
                let obj = self
                    .ctx
                    .create_instance_with_proto_payload(
                        None,
                        ObjectPayload::BYTECODE_FUNCTION(payload),
                    )
                    .map_err(|_| {
                        BytecodeSerializeError::InvalidValue("oom in closure object".into())
                    })?;
                Ok(JSValue::from(obj))
            }
            // native C function reference — resolved by name from the target
            // runtime's global object. If the name is absent there, the
            // reference cannot be reconstructed (QuickJS-aligned: native funcs
            // are never byte-built, only bound by name at load time).
            0xFD => {
                let name = self.read_string()?;
                let atom = Atom::from_str(self.ctx, &name)
                    .map_err(|_| BytecodeSerializeError::AtomNotFound(name.clone()))?;
                let gobj = self.ctx.global_obj();
                match gobj.get_property_internal(self.ctx, atom, JSValue::from(gobj), false) {
                    Ok(v) if !v.is_undefined_or_null() => Ok(v),
                    _ => Err(BytecodeSerializeError::UnsupportedObject(format!(
                        "native C function '{}' not found on global object",
                        name
                    ))),
                }
            }
            // object — implemented; function/native/closure exotics scaffolded.
            8 | 10 | 11 | 13..=28 => self.deserialize_complex(tag),
            other => Err(BytecodeSerializeError::InvalidTag(other)),
        }
    }

    fn deserialize_string(&mut self) -> Result<JSValue, BytecodeSerializeError> {
        let s = self.read_string()?;
        let sref = self
            .ctx
            .create_string_from_utf8(s.into_bytes())
            .map_err(|_| BytecodeSerializeError::Encode("failed to build string".into()))?;
        Ok(JSValue::make_string(sref))
    }

    fn deserialize_symbol(&mut self) -> Result<JSValue, BytecodeSerializeError> {
        let kind_raw = self.read_u8()?;
        let kind = atom_kind_from_u8(kind_raw)?;
        let bytes = self.read_string()?;
        let sref = self
            .ctx
            .create_string_from_utf8(bytes.clone().into_bytes())
            .map_err(|_| BytecodeSerializeError::Encode("failed to build symbol".into()))?;
        let atom = Atom::from_string_as_is(self.ctx, sref, kind)
            .map_err(|_| BytecodeSerializeError::AtomNotFound(bytes))?;
        let sym = self
            .ctx
            .create_symbol_from_atom(atom, kind)
            .map_err(|_| BytecodeSerializeError::Encode("failed to build symbol".into()))?;
        Ok(sym)
    }

    #[cfg(feature = "big_int")]
    fn deserialize_bigint(&mut self) -> Result<JSValue, BytecodeSerializeError> {
        let sign = self.read_u8()? != 0;
        let n = self.read_u32()? as usize;
        let mut limbs: Vec<u64> = Vec::with_capacity(n);
        for _ in 0..n {
            limbs.push(self.read_u64()?);
        }
        let b = BigIntRef::create(self.ctx, sign, limbs)
            .map_err(|_| BytecodeSerializeError::Encode("failed to build bigint".into()))?;
        Ok(JSValue::make_bigint(b))
    }

    /// Object / function / native / closure dispatch.
    fn deserialize_complex(&mut self, tag: u8) -> Result<JSValue, BytecodeSerializeError> {
        match tag {
            8 => crate::persistent::object::deserialize_object(self),
            _ => Err(BytecodeSerializeError::InvalidValue(
                "function/native/closure deserialization not yet implemented".into(),
            )),
        }
    }
}

// keep the imports referenced so the skeleton compiles without the deeper
// pieces wired up yet.
#[allow(unused_imports)]
use BYTECODE_SERIALIZE_MAGIC as _MAGIC;
#[allow(unused_imports)]
use BYTECODE_SERIALIZE_VERSION as _VERSION;
use gc_lite::{GcTrace, GcTraceCtx};
