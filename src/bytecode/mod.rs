include!(concat!(env!("OUT_DIR"), "/opcodes.rs"));

mod dump;
mod operators;
mod run;

pub mod arithmatic;

pub use operators::*;

#[cfg(feature = "dump_bytecode")]
pub use dump::{dump_byte_code, js_dump_function_bytecode};

/// get opcode descr for (common only)
pub fn opcode_info_common(op: u8) -> &'static JSOpCode {
    JS_COMMON_OPCODES_INFO.get(op as usize).unwrap()
}

/// get opcode descr in intermediate phase (common + temp)
pub fn opcode_info_intermediate(op: u8) -> &'static JSOpCode {
    if op < OP_COUNT_COMMON as u8 {
        JS_COMMON_OPCODES_INFO.get(op as usize).unwrap()
    } else {
        JS_TEMP_OPCODES_INFO
            .get(op as usize - OP_COUNT_COMMON as usize)
            .unwrap()
    }
}

/// get opcode descr in final phase (common + optionally short)
pub fn opcode_info_final(op: u8) -> &'static JSOpCode {
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
    OP_SPECIAL_OBJECT_ARGUMENTS = 0,
    OP_SPECIAL_OBJECT_MAPPED_ARGUMENTS = 1,
    OP_SPECIAL_OBJECT_THIS_FUNC = 2,
    OP_SPECIAL_OBJECT_NEW_TARGET = 3,
    OP_SPECIAL_OBJECT_HOME_OBJECT = 4,
    OP_SPECIAL_OBJECT_VAR_OBJECT = 5,
    OP_SPECIAL_OBJECT_IMPORT_META = 6,
}
