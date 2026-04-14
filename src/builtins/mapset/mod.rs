// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_map;
mod iter;
mod map;

pub use {class_map::*, iter::*, map::*};

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

pub(crate) const CLASS_DEF_MAP: ClassDef = ClassDef::builtin(
    Atom::CONST_Map,
    ClassInitType::Normal {
        proto_entries: js_map_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
                .entries(js_map_funcs)
                .export_to_global(),
        ),
    },
);

pub(crate) const CLASS_DEF_SET: ClassDef = ClassDef::builtin(
    Atom::CONST_Set,
    ClassInitType::Normal {
        proto_entries: js_set_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
                .magic(1)
                .entries(js_set_funcs)
                .export_to_global(),
        ),
    },
);

pub(crate) const CLASS_DEF_WEAK_MAP: ClassDef = ClassDef::builtin(
    Atom::CONST_WeakMap,
    ClassInitType::Normal {
        proto_entries: js_weakmap_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
                .magic(2)
                .export_to_global(),
        ),
    },
);

pub(crate) const CLASS_DEF_WEAK_SET: ClassDef = ClassDef::builtin(
    Atom::CONST_WeakSet,
    ClassInitType::Normal {
        proto_entries: js_weakset_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
                .magic(3)
                .export_to_global(),
        ),
    },
);

pub(crate) const CLASS_DEF_MAP_ITERATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_Map_Iterator,
    ClassInitType::InitFn(MapSet::class_init),
);

pub(crate) const CLASS_DEF_SET_ITERATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_Set_Iterator,
    ClassInitType::InitFn(MapSet::class_init),
);
