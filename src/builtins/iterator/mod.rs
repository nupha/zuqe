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

pub use {
    crate::object::{AsyncFromSyncIterator, AsyncIterableIterator},
    {a_iter::*, array_iter::*, async_iter::*, iter::*},
};
pub(crate) use {
    iter_concat::*, iter_helper::*, iter_shared::*, iter_wrap::*, iter_zip::*, iterator::*,
};

crate::def_native_object!(
    builtin,
    "ForInIterator",
    CLASS_DEF_FOR_IN_ITERATOR,
    class_id: (crate::JS_CLASS_FOR_IN_ITERATOR, crate::Atom::CONST_ForInIterPayload),
    crate::builtins::iterator::ForInIterPayload,
    &[],
    None
);

crate::def_native_object!(
    builtin,
    "AsyncFromSyncIterator",
    CLASS_DEF_ASYNC_FROM_SYNC_ITERATOR,
    class_id: (crate::JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, crate::Atom::CONST_empty_string),
    crate::builtins::iterator::JSAsyncFromSyncIteratorData, (), None,
    init_fn: AsyncFromSyncIterator::class_init
);

crate::def_native_object!(
    builtin,
    "AsyncIterableIterator",
    CLASS_DEF_ASYNC_ITERABLE_ITERATOR,
    class_id: (crate::JS_CLASS_ASYNC_ITERABLE_ITERATOR, crate::Atom::CONST_AsyncIterableIterator),
    crate::builtins::iterator::AsyncIterPayload, (), None,
    init_fn: AsyncIterableIterator::class_init
);

crate::def_native_object!(
    builtin,
    "Iterator",
    CLASS_DEF_ITERATOR,
    class_id: (crate::JS_CLASS_ITERATOR, crate::Atom::CONST_Iterator),
    init_fn: js_iterator_class_init
);

crate::def_native_object!(
    builtin,
    "IteratorHelper",
    CLASS_DEF_ITERATOR_HELPER,
    class_id: (crate::JS_CLASS_ITERATOR_HELPER, crate::Atom::CONST_empty_string),
    crate::builtins::iterator::IteratorHelperData, (), None,
    init_fn: js_iterator_helper_class_init
);

crate::def_native_object!(
    builtin,
    "IteratorWrap",
    CLASS_DEF_ITERATOR_WRAP,
    class_id: (crate::JS_CLASS_ITERATOR_WRAP, crate::Atom::CONST_empty_string),
    crate::builtins::iterator::IteratorWrapData, (), None,
    init_fn: js_iterator_wrap_class_init
);

crate::def_native_object!(
    builtin,
    "IteratorConcat",
    CLASS_DEF_ITERATOR_CONCAT,
    class_id: (crate::JS_CLASS_ITERATOR_CONCAT, crate::Atom::CONST_empty_string),
    crate::builtins::iterator::IteratorConcatData, (), None,
    init_fn: js_iterator_concat_class_init
);

crate::def_native_object!(
    builtin,
    "IteratorZip",
    CLASS_DEF_ITERATOR_ZIP,
    class_id: (crate::JS_CLASS_ITERATOR_ZIP, crate::Atom::CONST_empty_string),
    crate::builtins::iterator::IteratorZipData, (), None,
    init_fn: js_iterator_zip_class_init
);
