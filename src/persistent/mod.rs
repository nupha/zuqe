// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod atom_lut;
mod bytecode;
mod jsvalue;
mod module;
mod object;

pub use jsvalue::{DeserializeCtx, SerializeCtx};

use crate::{JSValue, ScopedContext};

/// Magic header written at the start of every serialized bytecode blob.
const BYTECODE_SERIALIZE_MAGIC: &[u8; 4] = b"ZQBC";
/// Serialization format version. Bump on incompatible layout changes.
const BYTECODE_SERIALIZE_VERSION: u32 = 1;

/// Error returned when (de)serializing a value fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BytecodeSerializeError {
    /// The buffer ended unexpectedly while reading.
    Eof,
    /// The serialized tag / discriminant is unknown to this runtime.
    InvalidTag(u8),
    /// The value carries state that cannot be persisted (e.g. a native object,
    /// a closure, a generator). The contained string names the unsupported
    /// kind.
    InvalidValue(String),
    /// A referenced atom could not be resolved in the current runtime.
    AtomNotFound(String),
    /// Generic encoding failure (e.g. f64 bit pattern).
    Encode(String),
    /// The object could not be serialized (e.g. native/exotic payload whose
    /// state is not reflected in own properties). The contained string names
    /// the unsupported operation.
    UnsupportedObject(String),
}

impl std::fmt::Display for BytecodeSerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BytecodeSerializeError::Eof => write!(f, "unexpected end of bytecode buffer"),
            BytecodeSerializeError::InvalidTag(t) => write!(f, "unknown bytecode tag {t:#x}"),
            BytecodeSerializeError::InvalidValue(k) => {
                write!(f, "value of kind `{k}` cannot be persisted")
            }
            BytecodeSerializeError::AtomNotFound(a) => {
                write!(f, "atom `{a}` not found in current runtime")
            }
            BytecodeSerializeError::Encode(m) => write!(f, "bytecode encode error: {m}"),
            BytecodeSerializeError::UnsupportedObject(k) => {
                write!(f, "object of kind `{k}` cannot be persisted")
            }
        }
    }
}

impl std::error::Error for BytecodeSerializeError {}

/// Types that can be written into a bytecode buffer.
pub trait BytecodeSerialize {
    /// Append the serialized form of `self` to `ctx`'s buffer.
    fn serialize(&self, ctx: &mut SerializeCtx) -> Result<(), BytecodeSerializeError>;
}

/// Types that can be reconstructed from a bytecode buffer.
pub trait BytecodeDeserialize: Sized {
    /// Read and reconstruct an instance of `Self` from `ctx`.
    fn deserialize(ctx: &mut DeserializeCtx) -> Result<Self, BytecodeSerializeError>;
}

/// Standalone persistence API on `JSValue` (associated functions, no `self`).
///
/// `dump` serializes any value (primitives, objects, arrays, **functions**,
/// **modules**) into a self-contained bytecode blob; `restore` rebuilds it in a
/// Runtime and yields a directly runnable value. The blob layout is
/// `MAGIC(4) | VERSION(u32) | value` and embeds its own atom symbol table, so it
/// can be loaded into a different Runtime than the one that produced it.
impl JSValue {
    /// Serialize `val` into a standalone bytecode blob.
    pub fn dump(
        ctx: &mut ScopedContext,
        val: JSValue,
    ) -> Result<Box<[u8]>, BytecodeSerializeError> {
        let mut sctx = SerializeCtx::new();
        sctx.buffer.extend_from_slice(BYTECODE_SERIALIZE_MAGIC);
        sctx.buffer
            .extend_from_slice(&BYTECODE_SERIALIZE_VERSION.to_le_bytes());

        // Persist the runtime's pre-compiled regexp table so that OP_REGEXP
        // indices remain valid across runtimes. Each entry stores only the
        // source pattern and flags — the compiled Regex is rebuilt on restore.
        #[cfg(feature = "builtin_regexp")]
        {
            let rps = &ctx.rt().regexp_precompiled;
            sctx.write_u32(rps.len() as u32);
            for rp in rps {
                let pat = rp.pattern.encode_utf8_bytes(false);
                sctx.write_u32(pat.len() as u32);
                sctx.write_bytes(&pat);
                let flg = rp.flags_str.encode_utf8_bytes(false);
                sctx.write_u32(flg.len() as u32);
                sctx.write_bytes(&flg);
            }
        }
        #[cfg(not(feature = "builtin_regexp"))]
        {
            sctx.write_u32(0);
        }

        sctx.serialize_value(ctx, val)?;
        Ok(sctx.buffer.into_boxed_slice())
    }

    /// Rebuild a previously [`dump`]ed value from its bytecode blob.
    pub fn restore<'a, 'b>(
        ctx: &'a mut ScopedContext<'b>,
        buf: &'a [u8],
    ) -> Result<JSValue, BytecodeSerializeError> {
        let mut inp = DeserializeCtx::new(ctx, buf);
        let magic = inp.read_bytes(4)?;
        if &magic != BYTECODE_SERIALIZE_MAGIC {
            return Err(BytecodeSerializeError::Encode("bad magic header".into()));
        }
        let version = inp.read_u32()?;
        if version != BYTECODE_SERIALIZE_VERSION {
            return Err(BytecodeSerializeError::Encode(format!(
                "unsupported bytecode version {version} (expected {BYTECODE_SERIALIZE_VERSION})"
            )));
        }

        // Restore the pre-compiled regexp table serialized by `dump`.
        #[cfg(feature = "builtin_regexp")]
        {
            let count = inp.read_u32()? as usize;
            for _ in 0..count {
                let pat_bytes = {
                    let n = inp.read_u32()? as usize;
                    inp.read_bytes(n)?
                };
                let flags_bytes = {
                    let n = inp.read_u32()? as usize;
                    inp.read_bytes(n)?
                };
                let pattern = String::from_utf8(pat_bytes)
                    .map_err(|e| BytecodeSerializeError::Encode(e.to_string()))?;
                let flags = String::from_utf8(flags_bytes)
                    .map_err(|e| BytecodeSerializeError::Encode(e.to_string()))?;
                let pat_sd = crate::string::StringData::from_str(&pattern)
                    .map_err(|_| BytecodeSerializeError::Encode("OOM in regexp pattern".into()))?;
                let flg_sd = crate::string::StringData::from_str(&flags)
                    .map_err(|_| BytecodeSerializeError::Encode("OOM in regexp flags".into()))?;
                let re_data = crate::builtins::regexp::RegExpData::create(
                    inp.ctx, pat_sd, flg_sd,
                )
                .map_err(|_| {
                    BytecodeSerializeError::InvalidValue("failed to recompile regexp".into())
                })?;
                inp.ctx.rt_mut().regexp_precompiled.push(re_data);
            }
        }
        #[cfg(not(feature = "builtin_regexp"))]
        {
            let count = inp.read_u32()?;
            if count != 0 {
                return Err(BytecodeSerializeError::Encode(
                    "blob contains regexp data but builtin_regexp is disabled".into(),
                ));
            }
        }

        inp.deserialize_value()
    }
}
