use std::ptr::NonNull;

use crate::{
    atom::Atom, context::JSContext, function::JSFunctionDef, parse::token::TokenData,
    value::JSValue,
};

use super::token::Token;

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
    pub ctx: JSContext,

    /// Source file name being parsed
    pub filename: &'s str,
    /// input bytes
    pub(super) input: &'s [u8],

    /// Current position in the input buffer
    pub(super) buf_ptr: *const u8,
    /// End of the input buffer
    pub(super) buf_end: *const u8,
    /// Pointer to the last processed character
    pub(super) last_ptr: *const u8,

    /// Current token being processed
    pub(super) token: Token,
    /// Indicates if a line feed was encountered before current token
    pub(super) got_lf: bool,
    /// Line number of the previously processed token
    pub(super) last_line_num: i32,
    /// Current line number being processed
    pub(super) line_num: i32,

    /// A raw pointer to current parsing function def.
    /// null if no current function def.
    pub(super) cur_func_def: *mut JSFunctionDef,

    /// Indicates if parsing a JavaScript module
    pub is_module: bool,
    /// Controls whether HTML-style comments are allowed
    pub allow_html_comments: bool,
    /// Enables parsing of JSON superset syntax
    pub ext_json: bool,
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
    fn new_internal(
        ctx: &JSContext,
        fd: *mut JSFunctionDef,
        source: &'s [u8],
        filename: &'s str,
    ) -> Self {
        Self {
            ctx: ctx.clone(),

            input: source,
            buf_ptr: source.as_ptr(),
            buf_end: unsafe { source.as_ptr().add(source.len()) },

            last_line_num: 0,
            line_num: 1,
            filename,
            token: Token {
                val: ' ' as i32,
                line_num: 1,
                ptr: std::ptr::null(),
                data: TokenData::Num(JSValue::Int(0)),
            },

            last_ptr: std::ptr::null(),
            cur_func_def: fd,
            is_module: false,
            allow_html_comments: false,
            ext_json: false,
            got_lf: false,
        }
    }

    /// Create new parser state with root function def.
    #[inline(always)]
    pub fn new(
        ctx: &JSContext,
        fd: NonNull<JSFunctionDef>,
        source: &'s [u8],
        filename: &'s str,
    ) -> Self {
        Self::new_internal(ctx, fd.as_ptr(), source, filename)
    }

    /// Create new parser state for json (which don't have function def).
    #[inline(always)]
    pub fn new_for_json(ctx: &JSContext, source: &'s [u8], filename: &'s str) -> Self {
        Self::new_internal(ctx, std::ptr::null_mut(), source, filename)
    }

    /// free atom if any.
    #[inline(always)]
    pub(super) fn clear_token_data(&mut self) {
        // if let TokenData::Ident { atom, .. } = &mut self.token.data {
        //     *atom = Atom::NULL;
        // }
        self.token.data = TokenData::None;
    }

    /// Get function def which is currently being parsed.
    /// Will panic if cur_func is null.
    #[inline(always)]
    pub(crate) fn cur_func(&self) -> &JSFunctionDef {
        debug_assert!(!self.cur_func_def.is_null());
        unsafe { &*self.cur_func_def }
    }

    /// Get function def which is currently being parsed.
    /// Will panic if cur_func is null.
    #[allow(clippy::mut_from_ref)]
    #[inline(always)]
    pub(super) fn cur_func_mut(&self) -> &mut JSFunctionDef {
        debug_assert!(!self.cur_func_def.is_null());
        unsafe { &mut *self.cur_func_def }
    }

    /// Run closure with cur_func mut ref.
    /// If cur_func is null, returns None.
    #[inline]
    pub(super) fn with_cur_func_mut<R>(
        &mut self,
        f: impl FnOnce(&mut JSParseState, &mut JSFunctionDef) -> R,
    ) -> Option<R> {
        unsafe { self.cur_func_def.as_mut().map(|fd| f(self, fd)) }
    }

    /// Perform push scope on current function def. For detail, see JSFunctionDef::push_scope()
    ///
    /// # Returns
    ///
    /// * `Ok(scope_id)` - ID of the newly created scope
    /// * `Err(-1)` - If scope creation fails
    pub(super) fn push_scope(&mut self) -> Result<u16, i32> {
        if !self.cur_func_def.is_null() {
            unsafe { (*self.cur_func_def).push_scope(self.last_line_num) }
        } else {
            #[cfg(debug_assertions)]
            unreachable!("empty current function def");
            #[cfg(not(debug_assertions))]
            Err(-1)
        }
    }

    /// Perform pop scope on current function def. For detail, see JSFunctionDef::pop_scope()
    pub(super) fn pop_scope(&mut self) {
        if !self.cur_func_def.is_null() {
            let last_line_num = self.last_line_num;
            unsafe {
                (*self.cur_func_def).pop_scope(last_line_num);
            }
        } else {
            #[cfg(debug_assertions)]
            unreachable!("empty current function def");
        }
    }

    /// Perform close scopes on current function def. For detail, see JSFunctionDef::close_scopes()
    pub(super) fn close_scopes(&mut self, scope: i32, scope_stop: i32) {
        if !self.cur_func_def.is_null() {
            unsafe {
                (*self.cur_func_def).close_scopes(scope as _, scope_stop as _, self.last_line_num);
            }
        } else {
            #[cfg(debug_assertions)]
            unreachable!("empty current function def");
        }
    }

    /// get parse pos
    pub const fn get_pos(&self) -> JSParsePos {
        JSParsePos {
            last_line_num: self.last_line_num,
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
pub struct JSParsePos {
    /// Line number of the last processed token
    pub last_line_num: i32,
    /// Current line number
    pub line_num: i32,
    /// Indicates if a line feed was encountered
    pub got_lf: bool,
    /// Pointer to position in source buffer
    pub ptr: *const u8,
}
