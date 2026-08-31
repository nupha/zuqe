// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_regexp;
mod class_regexp_iter;
mod re;

pub use {class_regexp::*, class_regexp_iter::*, re::*};

use crate::object::RegExp;

crate::def_native_object!(
    builtin,
    "RegExp",
    CLASS_DEF_REGEXP,
    class_id: (crate::JS_CLASS_REGEXP, crate::Atom::CONST_RegExp),
    crate::builtins::regexp::RegExpData,
    js_regexp_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_regexp_constructor, 2, true)
            .entries(js_regexp_funcs)
            .expose_to_global()
    )
);

crate::def_native_object!(
    builtin,
    "RegExp String Iterator",
    CLASS_DEF_REGEXP_STRING_ITERATOR,
    class_id: (
        crate::JS_CLASS_REGEXP_STRING_ITERATOR,
        crate::Atom::CONST_RegExp_String_Iterator,
    ),
    crate::builtins::regexp::RegExpStringIterPayload, (), None,
    init_fn: RegExp::class_init
);
