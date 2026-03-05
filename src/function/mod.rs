// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod afun;
mod argument;
mod astate;
mod bfun;
mod bound;
mod bytecode;
mod call;
mod closure;
mod cpool;
mod emit;
mod fdef;
mod fields;
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
    afun::*, astate::*, bfun::*, bound::*, bytecode::*, call::*, fdef::*, fields::*, label::*,
    nfun::*, optimize::*, scope::*, var::*, var_ref::*,
};

pub(crate) use {argument::*, closure::*, emit::*};
