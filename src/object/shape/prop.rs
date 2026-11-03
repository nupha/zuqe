// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcHead, GcTrace, GcTraceCtx};
use hash_link::Hashable;

use crate::{
    GcBind,
    atom::Atom,
    context::ScopedContext,
    object::{JS_PROP_LENGTH, JS_PROP_WRITABLE},
};

/// Property defination in shape
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShapeProperty {
    pub(crate) atom: Atom,
    pub(super) flags: u32,
}

impl GcBind for ShapeProperty {
    fn bind_node(&self, ctx: &ScopedContext, owner: std::ptr::NonNull<GcHead>) {
        ctx.bind_atom(owner, self.atom);
    }
}

impl GcTrace for ShapeProperty {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.atom.trace(tr);
    }
}

impl Hashable for ShapeProperty {
    /// use atom id as hash value
    #[inline(always)]
    fn get_hash(&self) -> u32 {
        self.atom.id()
    }
}

impl ShapeProperty {
    #[inline(always)]
    pub const fn flags(&self) -> u32 {
        self.flags
    }

    #[inline(always)]
    pub const fn set_flags(&mut self, f: u32) {
        self.flags = f;
    }

    /// is special property `length`?
    #[inline(always)]
    pub const fn is_length(&self) -> bool {
        self.flags & JS_PROP_LENGTH != 0
    }

    #[inline(always)]
    pub const fn is_writable(&self) -> bool {
        self.flags & JS_PROP_WRITABLE != 0
    }
}
