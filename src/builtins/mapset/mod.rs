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
    proto_entries: js_map_proto_funcs,
    ctor_entries: js_map_funcs,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_map_proto_funcs,
        ctor: Some(crate::class::ClassCtorDef {
            callback: crate::Callback::Constructor(js_map_constructor),
            length: 0,
            magic: 0,
            entries: js_map_funcs,
            global_name: Some(Atom::CONST_Map),
        }),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_SET: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Set,
    proto_entries: js_set_proto_funcs,
    ctor_entries: js_set_funcs,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_set_proto_funcs,
        ctor: Some(crate::class::ClassCtorDef {
            callback: crate::Callback::Constructor(js_map_constructor),
            length: 0,
            magic: 1,
            entries: js_set_funcs,
            global_name: Some(Atom::CONST_Set),
        }),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_WEAK_MAP: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_WeakMap,
    proto_entries: js_weakmap_proto_funcs,
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_weakmap_proto_funcs,
        ctor: Some(crate::class::ClassCtorDef {
            callback: crate::Callback::Constructor(js_map_constructor),
            length: 0,
            magic: 2,
            entries: &[],
            global_name: Some(Atom::CONST_WeakMap),
        }),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_WEAK_SET: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_WeakSet,
    proto_entries: js_weakset_proto_funcs,
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_weakset_proto_funcs,
        ctor: Some(crate::class::ClassCtorDef {
            callback: crate::Callback::Constructor(js_map_constructor),
            length: 0,
            magic: 3,
            entries: &[],
            global_name: Some(Atom::CONST_WeakSet),
        }),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_MAP_ITERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Map_Iterator,
    proto_entries: js_map_iterator_proto_funcs,
    ctor_entries: &[],
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
    proto_entries: js_set_iterator_proto_funcs,
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: ClassInitType::InitFn(MapSet::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
