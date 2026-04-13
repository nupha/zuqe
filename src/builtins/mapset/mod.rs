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

pub const CLASS_DEF_MAP: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Map,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_map_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
                .entries(js_map_funcs)
                .export_to_global(),
        ),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_SET: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Set,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_set_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
                .magic(1)
                .entries(js_set_funcs)
                .export_to_global(),
        ),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_WEAK_MAP: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_WeakMap,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_weakmap_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
                .magic(2)
                .export_to_global(),
        ),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_WEAK_SET: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_WeakSet,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_weakset_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_map_constructor, 0, false)
                .magic(3)
                .export_to_global(),
        ),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_MAP_ITERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Map_Iterator,
    call: None,
    exotic: None,
    initialize: ClassInitType::InitFn(MapSet::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_SET_ITERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Set_Iterator,
    call: None,
    exotic: None,
    initialize: ClassInitType::InitFn(MapSet::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
