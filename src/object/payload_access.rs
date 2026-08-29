// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use crate::class::ClassID;
use gc_lite::GcTrace;

include!(concat!(env!("OUT_DIR"), "/payload_accessors.rs"));

impl super::ObjectImpl {
    #[inline(always)]
    pub fn payload_ptr(&self) -> *mut c_void {
        self.payload
    }

    #[inline(always)]
    pub(crate) fn payload_ptr_set(&mut self, ptr: *mut c_void) {
        self.payload = ptr;
    }
}
