// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod a_iter;
mod array_iter;
mod iter;

pub(crate) use {a_iter::*, array_iter::*, iter::*};

use crate::{Atom, ClassDef};

pub(crate) const CLASS_DEF_FOR_IN_ITERATOR: ClassDef =
    ClassDef::builtin(Atom::CONST_ForInIterPayload, &[], None);

pub(crate) const CLASS_DEF_ASYNC_FROM_SYNC_ITERATOR: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_empty_string, AsyncFromSyncIterator::class_init);
