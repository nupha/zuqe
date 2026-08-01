// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod a_iter;
mod array_iter;
mod async_iter;
mod iter;
mod iter_concat;
mod iter_helper;
mod iter_shared;
mod iter_wrap;
mod iter_zip;
mod iterator;

pub use {a_iter::*, array_iter::*, async_iter::*, iter::*};

pub use crate::object::{AsyncFromSyncIterator, AsyncIterableIterator};
pub(crate) use {
    iter_concat::*, iter_helper::*, iter_shared::*, iter_wrap::*, iter_zip::*, iterator::*,
};

use crate::{Atom, ClassDef};

pub(crate) static CLASS_DEF_FOR_IN_ITERATOR: ClassDef =
    ClassDef::builtin(Atom::CONST_ForInIterPayload, &[], None);

pub(crate) static CLASS_DEF_ASYNC_FROM_SYNC_ITERATOR: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_empty_string, AsyncFromSyncIterator::class_init);

pub(crate) static CLASS_DEF_ASYNC_ITERABLE_ITERATOR: ClassDef = ClassDef::builtin_with_init_func(
    Atom::CONST_AsyncIterableIterator,
    AsyncIterableIterator::class_init,
);

pub(crate) static CLASS_DEF_ITERATOR: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_Iterator, js_iterator_class_init);

pub(crate) static CLASS_DEF_ITERATOR_HELPER: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_empty_string, js_iterator_helper_class_init);

pub(crate) static CLASS_DEF_ITERATOR_WRAP: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_empty_string, js_iterator_wrap_class_init);

pub(crate) static CLASS_DEF_ITERATOR_CONCAT: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_empty_string, js_iterator_concat_class_init);

pub(crate) static CLASS_DEF_ITERATOR_ZIP: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_empty_string, js_iterator_zip_class_init);
