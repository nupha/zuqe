// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod a_iter;
mod array_iter;
mod iter;

pub use {a_iter::*, array_iter::*, iter::*};

use crate::{Atom, ClassDef, class::ClassInitType};

pub const CLASS_DEF_FOR_IN_ITERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_ForInIterPayload,
    call: None,
    exotic: None,
    initialize: ClassInitType::Normal {
        proto_entries: &[],
        ctor: None,
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_ASYNC_FROM_SYNC_ITERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_empty_string,
    call: None,
    exotic: None,
    initialize: ClassInitType::InitFn(AsyncFromSyncIterator::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
