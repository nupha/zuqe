// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use crate::{context::ScopedContext, function::FunctionDef, number::IntOrFloat};

use super::tokenize::{Token, TokenData, TokenKind};

// Parse flags for controlling expression parsing behavior
/// Allow the 'in' binary operator
pub const PF_IN_ACCEPTED: i32 = 1 << 0;
/// Allow function calls parsing in js_parse_postfix_expr()
pub const PF_POSTFIX_CALL: i32 = 1 << 1;
/// Allow arrow functions parsing in js_parse_postfix_expr()
pub const PF_ARROW_FUNC: i32 = 1 << 2;
/// Allow the exponentiation operator in js_parse_unary()
pub const PF_POW_ALLOWED: i32 = 1 << 3;
// Forbid the exponentiation operator in js_parse_unary()
pub const PF_POW_FORBIDDEN: i32 = 1 << 4;

// Skip flags for controlling statement parsing behavior
pub const SKIP_HAS_SEMI: i32 = 1 << 0; // Statement has semicolon
pub const SKIP_HAS_ELLIPSIS: i32 = 1 << 1; // Statement has spread operator
pub const SKIP_HAS_ASSIGNMENT: i32 = 1 << 2; // Statement has assignment operator

/// Represents the state of the JavaScript parser
///
/// This structure maintains all necessary information for parsing JavaScript code,
/// including the current position, token, scope, and parsing context.
pub struct JSParseState<'s> {
    /// JavaScript context pointer for runtime operations
    /// FIXME: use &mut ScopedContext instead
    ctx: ScopedContext<'s>,

    /// Source file name being parsed
    pub(super) filename: &'s str,

    // TODO: bundle input, input_end, buf_ptr to stream cursor
    /// input bytes
    pub(super) input: &'s [u8],
    /// End pointer to input bytes, exclusive
    pub(super) input_end: *const u8,

    /// Current pointer to the input
    pub(super) buf_ptr: *const u8,
    /// Pointer to the last processed character
    pub(super) last_ptr: *const u8,

    /// Current token being processed
    pub(super) token: Token,
    /// Indicates if a line feed was encountered before current token
    pub(super) got_lf: bool,
    /// Line number of the previously processed token
    pub(super) last_line_num: i32,
    /// Byte column position of the previously processed token (within its line)
    pub(super) last_line_pos: u32,
    /// Byte offset into input where the current line starts
    pub(super) line_start_offset: usize,
    /// Current line number being processed
    pub(super) line_num: i32,

    /// A raw pointer to current parsing function def.
    /// null if no current function def.
    pub(super) cur_func_def: *mut FunctionDef,

    /// Indicates if parsing a JavaScript module
    pub(crate) is_module: bool,
    /// Controls whether HTML-style comments are allowed
    pub(crate) allow_html_comments: bool,

    /// Enables parsing of JSON superset syntax
    #[cfg(feature = "builtin_json")]
    pub(crate) ext_json: bool,
}

impl Drop for JSParseState<'_> {
    fn drop(&mut self) {
        self.clear_token_data();
    }
}

impl<'s> JSParseState<'s> {
    /// Create new parser state.
    ///
    /// # Arguments
    /// * `ctx` - JavaScript context pointer
    /// * `fd` - The root function def pointer. could be null.
    /// * `source` - Buffer containing source code
    /// * `filename` - Name of source file
    const fn new_internal(
        ctx: ScopedContext<'s>,
        fd: *mut FunctionDef,
        source: &'s [u8],
        filename: &'s str,
    ) -> Self {
        let ptr = source.as_ptr();

        Self {
            ctx,

            input: source,
            input_end: unsafe { ptr.add(source.len()) },
            buf_ptr: ptr,

            last_line_num: 0,
            last_line_pos: 0,
            line_start_offset: 0,
            line_num: 1,
            filename,
            token: Token {
                kind: TokenKind::Char(' '),
                line_num: 1,
                ptr: std::ptr::null(),
                data: TokenData::Num(IntOrFloat::Int64(0)),
            },

            last_ptr: std::ptr::null(),
            cur_func_def: fd,
            is_module: false,
            allow_html_comments: false,
            got_lf: false,

            #[cfg(feature = "builtin_json")]
            ext_json: false,
        }
    }

    /// Create new parser state with root function def.
    #[inline(always)]
    pub(crate) const fn new(
        ctx: ScopedContext<'s>,
        fd: NonNull<FunctionDef>,
        source: &'s [u8],
        filename: &'s str,
    ) -> Self {
        Self::new_internal(ctx, fd.as_ptr(), source, filename)
    }

    /// Create new parser state for json (which don't have function def).
    #[inline(always)]
    pub const fn new_for_json(ctx: ScopedContext<'s>, source: &'s [u8], filename: &'s str) -> Self {
        Self::new_internal(ctx, std::ptr::null_mut(), source, filename)
    }

    #[inline(always)]
    pub const fn ctx(&mut self) -> &mut ScopedContext<'s> {
        &mut self.ctx
    }

    /// Consume the parser and return the inner scope, along with a value.
    ///
    /// This allows returning a value from `with_new_scope` after the parser
    /// has consumed the `ScopedContext`.
    pub fn finish<T>(self, value: T) -> (ScopedContext<'s>, T) {
        let this = std::mem::ManuallyDrop::new(self);
        let ctx = unsafe { std::ptr::read(&this.ctx) };
        (ctx, value)
    }

    /// free atom if any.
    #[inline(always)]
    pub(super) fn clear_token_data(&mut self) {
        self.token.data = TokenData::None;
    }

    /// Get function def which is currently being parsed.
    /// Will panic if cur_func is null.
    #[inline(always)]
    pub(crate) fn cur_func(&self) -> &FunctionDef {
        debug_assert!(!self.cur_func_def.is_null());
        unsafe { &*self.cur_func_def }
    }

    /// Get function def which is currently being parsed.
    /// Will panic if cur_func is null.
    #[allow(clippy::mut_from_ref)]
    #[inline(always)]
    pub(super) fn cur_func_mut(&self) -> &mut FunctionDef {
        debug_assert!(!self.cur_func_def.is_null());
        unsafe { &mut *self.cur_func_def }
    }

    /// Perform push scope on current function def. For detail, see FunctionDef::push_scope()
    ///
    /// # Returns
    ///
    /// * `Ok(scope_id)` - ID of the newly created scope
    /// * `Err(-1)` - If scope creation fails
    pub(super) fn push_scope(&mut self) -> Result<u16, i32> {
        if !self.cur_func_def.is_null() {
            unsafe { (*self.cur_func_def).push_scope(self.last_line_num, self.last_line_pos) }
        } else {
            #[cfg(debug_assertions)]
            unreachable!("empty current function def");
            #[cfg(not(debug_assertions))]
            Err(-1)
        }
    }

    /// Perform pop scope on current function def. For detail, see FunctionDef::pop_scope()
    pub(super) fn pop_scope(&mut self) {
        if !self.cur_func_def.is_null() {
            let last_line_num = self.last_line_num;
            let last_line_pos = self.last_line_pos;
            unsafe {
                (*self.cur_func_def).pop_scope(last_line_num, last_line_pos);
            }
        } else {
            #[cfg(debug_assertions)]
            unreachable!("empty current function def");
        }
    }

    /// Run `f` inside a fresh block scope: `push_scope` before and
    /// `pop_scope` after, mirroring the runtime `with_new_scope` style.
    /// On error the scope is intentionally not popped, matching the
    /// historical manual pairing (a parse error aborts the whole
    /// FunctionDef, so the dangling scope is never observed).
    pub(super) fn with_block_scope<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, i32>,
    ) -> Result<T, i32> {
        self.push_scope()?;
        let result = f(self)?;
        self.pop_scope();
        Ok(result)
    }

    /// Perform close scopes on current function def. For detail, see FunctionDef::close_scopes()
    pub(super) fn close_scopes(&mut self, scope: i32, scope_stop: i32) {
        self.cur_func_mut().close_scopes(
            scope as _,
            scope_stop as _,
            self.last_line_num,
            self.last_line_pos,
        );
    }

    /// get parse pos
    pub(super) const fn get_pos(&self) -> JSParsePos {
        JSParsePos {
            last_line_num: self.last_line_num,
            last_line_pos: self.last_line_pos,
            line_start_offset: self.line_start_offset,
            line_num: self.token.line_num,
            ptr: self.token.ptr,
            got_lf: self.got_lf,
        }
    }
}

/// Represents a position in the source code during parsing
///
/// Used for backtracking and position tracking during parsing operations.
#[derive(Debug)]
pub(super) struct JSParsePos {
    /// Line number of the last processed token
    pub last_line_num: i32,
    /// Byte column position of the last processed token
    pub last_line_pos: u32,
    /// Byte offset into input where the current line starts
    pub line_start_offset: usize,
    /// Current line number
    pub line_num: i32,
    /// Indicates if a line feed was encountered
    pub got_lf: bool,
    /// Pointer to position in source buffer
    pub ptr: *const u8,
}
