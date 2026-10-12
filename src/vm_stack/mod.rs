// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod arena;
mod frame;

pub(crate) use {
    arena::{FrameAllocError, VmStack},
    frame::{
        AsyncStackFrame, ExecMode, FrameBuffer, FrameIndex, FrameState, StackFrame,
        VM_MAX_CALL_DEPTH, VirtualFrame,
    },
};
