// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_map;
mod iter;
mod map;

pub use {class_map::*, iter::*, map::*};

use crate::{Atom, class::ClassDef};

pub(crate) static CLASS_DEF_MAP: ClassDef = ClassDef::builtin(
    Atom::CONST_Map,
    js_map_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
            .entries(js_map_funcs)
            .global(),
    ),
);

pub(crate) static CLASS_DEF_SET: ClassDef = ClassDef::builtin(
    Atom::CONST_Set,
    js_set_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
            .magic(1)
            .entries(js_set_funcs)
            .global(),
    ),
);

pub(crate) static CLASS_DEF_WEAK_MAP: ClassDef = ClassDef::builtin(
    Atom::CONST_WeakMap,
    js_weakmap_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
            .magic(2)
            .global(),
    ),
);

pub(crate) static CLASS_DEF_WEAK_SET: ClassDef = ClassDef::builtin(
    Atom::CONST_WeakSet,
    js_weakset_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
            .magic(3)
            .global(),
    ),
);

pub(crate) static CLASS_DEF_MAP_ITERATOR: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_Map_Iterator, MapSet::class_init);

pub(crate) static CLASS_DEF_SET_ITERATOR: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_Set_Iterator, MapSet::class_init);
