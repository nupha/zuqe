include!(concat!(env!("OUT_DIR"), "/opcodes.rs"));

mod dump;
mod operators;
mod run;

pub mod arithmatic;

pub use operators::*;

#[cfg(feature = "dump_bytecode")]
pub use dump::dump_opcodes;

/// get opcode descr for (common only)
pub fn opcode_info_common(op: u8) -> &'static Opcode {
    JS_COMMON_OPCODES_INFO.get(op as usize).unwrap()
}

/// get opcode descr in intermediate phase (common + temp)
pub fn opcode_info_intermediate(op: u8) -> &'static Opcode {
    if op < OP_COUNT_COMMON as u8 {
        JS_COMMON_OPCODES_INFO.get(op as usize).unwrap()
    } else {
        JS_TEMP_OPCODES_INFO
            .get(op as usize - OP_COUNT_COMMON as usize)
            .unwrap()
    }
}

/// get opcode descr in final phase (common + optionally short)
pub fn opcode_info_final(op: u8) -> &'static Opcode {
    #[cfg(not(feature = "short_opcode"))]
    {
        JS_COMMON_OPCODES_INFO.get(op as usize).unwrap()
    }

    #[cfg(feature = "short_opcode")]
    {
        if op < OP_COUNT_COMMON as u8 {
            JS_COMMON_OPCODES_INFO.get(op as usize).unwrap()
        } else {
            JS_TEMP_OPCODES_INFO
                .get(op as usize - OP_COUNT_COMMON)
                .unwrap()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SpecialObject {
    Arguments = 0,
    MappedArguments = 1,
    ThisFunc = 2,
    NewTarget = 3,
    HomeObject = 4,
    VarObject = 5,
    ImportMeta = 6,
}
