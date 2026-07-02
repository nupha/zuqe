// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

include!(concat!(env!("OUT_DIR"), "/opcodes.rs"));

pub mod arithmatic;
pub(crate) mod exec_stackless;
pub(crate) mod operators;

#[cfg(feature = "dump_bytecode")]
mod dump;
#[cfg(feature = "dump_bytecode")]
pub(super) use dump::dump_opcodes;

/// get opcode descr in intermediate phase (common + temp)
pub fn opcode_info_intermediate(op: u8) -> &'static Opcode {
    if op < OP_COUNT_COMMON {
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
pub(crate) enum SpecialObject {
    Arguments = 0,
    MappedArguments = 1,
    ThisFunc = 2,
    NewTarget = 3,
    HomeObject = 4,
    VarObject = 5,
    ImportMeta = 6,
}
