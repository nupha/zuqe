// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod a_state;
mod afun;
mod argument;
mod bfun;
mod bound;
mod bytecode;
mod call;
mod closure;
mod cpool;
mod emit;
mod fields;
mod func_def;
mod label;
mod mkbc;
mod nfun;
mod optimize;
mod resolve_labels;
mod resolve_scoped_private;
mod resolve_scoped_vars;
mod resolve_vars;
mod scope;
mod stack;
mod var;
mod var_ref;

pub use {
    bfun::*, bound::*, bytecode::*, call::*, fields::*, func_def::*, label::*, nfun::*, optimize::*,
    scope::*, var::*, var_ref::*,
};

pub use crate::object::{
    AsyncFunction, BCWrapper, BoundFunction, BytecodeFunction, NativeFunction,
};

pub(crate) use {a_state::*, argument::*, closure::*, emit::*};
