// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_map;
mod iter;
mod map;

pub use {class_map::*, iter::*, map::*};

use crate::object::MapSet;

crate::def_native_object!(
    builtin,
    "Map",
    CLASS_DEF_MAP,
    class_id: (crate::JS_CLASS_MAP, crate::Atom::CONST_Map),
    crate::builtins::mapset::JSMapState,
    js_map_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
            .entries(js_map_funcs)
            .expose_to_global()
    )
);

crate::def_native_object!(
    builtin,
    "Set",
    CLASS_DEF_SET,
    class_id: (crate::JS_CLASS_SET, crate::Atom::CONST_Set),
    crate::builtins::mapset::JSMapState,
    js_set_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
            .magic(1)
            .entries(js_set_funcs)
            .expose_to_global()
    )
);

crate::def_native_object!(
    builtin,
    "WeakMap",
    CLASS_DEF_WEAK_MAP,
    class_id: (crate::JS_CLASS_WEAKMAP, crate::Atom::CONST_WeakMap),
    crate::builtins::mapset::JSMapState,
    js_weakmap_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
            .magic(2)
            .expose_to_global()
    )
);

crate::def_native_object!(
    builtin,
    "WeakSet",
    CLASS_DEF_WEAK_SET,
    class_id: (crate::JS_CLASS_WEAKSET, crate::Atom::CONST_WeakSet),
    crate::builtins::mapset::JSMapState,
    js_weakset_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
            .magic(3)
            .expose_to_global()
    )
);

crate::def_native_object!(
    builtin,
    "Map Iterator",
    CLASS_DEF_MAP_ITERATOR,
    class_id: (crate::JS_CLASS_MAP_ITERATOR, crate::Atom::CONST_Map_Iterator),
    crate::builtins::mapset::JSMapIteratorData, (), None,
    init_fn: MapSet::class_init
);

crate::def_native_object!(
    builtin,
    "Set Iterator",
    CLASS_DEF_SET_ITERATOR,
    class_id: (crate::JS_CLASS_SET_ITERATOR, crate::Atom::CONST_Set_Iterator),
    crate::builtins::mapset::JSMapIteratorData, (), None,
    init_fn: MapSet::class_init
);
