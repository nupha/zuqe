use crate::{Atom, ScopedContext, atom::AtomKind, persistent::SerializeCtx};

use super::{BytecodeSerializeError, DeserializeCtx};

const BC_ATOM_NULL: u8 = 0;
const BC_ATOM_SMALL_INT: u8 = 1;
const BC_ATOM_LUT_INDEX: u8 = 2;
const BC_ATOM_NEW: u8 = 3;

impl SerializeCtx {
    pub(crate) fn write_atom(
        &mut self,
        cx: &mut ScopedContext,
        atom: Atom,
    ) -> Result<(), BytecodeSerializeError> {
        if atom.is_null() {
            self.write_u8(BC_ATOM_NULL);
            return Ok(());
        }
        if atom.is_small_int() {
            self.write_u8(BC_ATOM_SMALL_INT);
            self.write_u32(atom.id());
            return Ok(());
        }

        if let Some(&idx) = self.atom_lut.get(&atom.id()) {
            self.write_u8(BC_ATOM_LUT_INDEX);
            self.write_u32(idx as u32);
            return Ok(());
        }

        let idx = self.atom_count;
        self.atom_count += 1;
        self.atom_lut.insert(atom.id(), idx);

        let sref = atom.descriptor(cx.rt());
        let kind = sref
            .atom_info()
            .map(|a| a.kind())
            .unwrap_or(AtomKind::String);
        let bytes = sref.content().encode_utf8(false);

        self.write_u8(BC_ATOM_NEW);
        self.write_u8(kind as u8);
        self.write_u32(bytes.len() as u32);
        self.write_bytes(bytes.as_bytes());

        Ok(())
    }
}

impl<'a, 'b> DeserializeCtx<'a, 'b> {
    pub(crate) fn read_atom(&mut self) -> Result<Atom, BytecodeSerializeError> {
        let tag = self.read_u8()?;
        match tag {
            BC_ATOM_NULL => Ok(Atom::NULL),
            BC_ATOM_SMALL_INT => {
                let id = self.read_u32()?;
                Ok(Atom::from_id(id))
            }
            BC_ATOM_LUT_INDEX => {
                let idx = self.read_u32()? as usize;
                self.atom_table
                    .get(idx)
                    .copied()
                    .ok_or(BytecodeSerializeError::InvalidTag(BC_ATOM_LUT_INDEX))
            }
            BC_ATOM_NEW => {
                let kind = atom_kind_from_u8(self.read_u8()?)?;
                let n = self.read_u32()? as usize;
                let bytes = self.read_bytes(n)?;
                let sref = self.ctx.create_string_from_utf8(bytes).map_err(|_| {
                    BytecodeSerializeError::Encode("atom string build failed".into())
                })?;
                let atom = Atom::from_string(self.ctx, sref, kind).map_err(|_| {
                    BytecodeSerializeError::Encode("atom registration failed".into())
                })?;

                println!(
                    "atom: {atom:?}, {:?}",
                    atom.descriptor(self.ctx.rt()).content().encode_utf8(false)
                );
                debug_assert!(
                    atom.descriptor(self.ctx.rt()).content().len() == sref.content().len(),
                    "{:?}",
                    atom.descriptor(self.ctx.rt()).content()
                );
                self.atom_table.push(atom);
                Ok(atom)
            }
            _ => Err(BytecodeSerializeError::InvalidTag(tag)),
        }
    }
}

pub(super) const fn atom_kind_from_u8(v: u8) -> Result<AtomKind, BytecodeSerializeError> {
    match v {
        1 => Ok(AtomKind::String),
        2 => Ok(AtomKind::GlobalSymbol),
        3 => Ok(AtomKind::Symbol),
        4 => Ok(AtomKind::Private),
        other => Err(BytecodeSerializeError::InvalidTag(other)),
    }
}
