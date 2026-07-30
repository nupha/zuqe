// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod array;
mod class;
mod destructing;
mod directives;
mod emit;
mod error;
mod esm;
mod expr;
mod expr_call;
mod expr_lvalue;
mod expr_object;
mod function;
mod obj;
mod parse;
mod state;
mod statement;
mod string;
mod template;
mod tokenize;
mod var;

#[cfg(feature = "builtin_json")]
mod json;
#[cfg(feature = "builtin_json")]
pub(crate) use json::JSONParseRecord;
#[cfg(feature = "builtin_regexp")]
mod regexp;

pub use {function::JSParseFunctionEnum, state::JSParseState};
