use std::ptr::NonNull;

use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom},
    context::JSContext,
    function::JSFunctionDef,
    parse::token::TokenData,
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
        self.clear_atom_in_token();
    }
}

impl<'s> JSParseState<'s> {
    /// Creates a new parser state with root function def.
    ///
    /// # Arguments
    /// * `ctx` - JavaScript context pointer
    /// * `fd` - The root function def pointer
    /// * `input` - Input buffer containing source code
    /// * `input_len` - Length of input buffer
    /// * `filename` - Name of source file
    pub fn new(
        ctx: &JSContext,
        fd: NonNull<JSFunctionDef>,
        source: &'s [u8],
        filename: &'s str,
    ) -> Self {
        // TODO: create root function def
        // let mut fd = JSFunctionDef::new(ctx, std::ptr::null_mut(), false, false, filename, 1)?;

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
            cur_func_def: fd.as_ptr(),
            is_module: false,
            allow_html_comments: false,
            ext_json: false,
            got_lf: false,
        }
    }

    /// free atom if any.
    pub fn clear_atom_in_token(&mut self) {
        match &mut self.token.data {
            TokenData::Ident { atom, .. } => {
                JS_FreeAtom(&self.ctx, *atom);
                *atom = JS_ATOM_NULL;
            }
            TokenData::Str { str, .. } => *str = JSValue::UNDEFINED,
            TokenData::Num(n) => *n = JSValue::UNDEFINED,
            TokenData::Regexp { body, flags } => {
                *body = JSValue::UNDEFINED;
                *flags = JSValue::UNDEFINED;
            }
            TokenData::None | TokenData::Char(_) => {}
        }
    }

    /// Get current function def being parsed.
    /// Will panic if cur_func is null.
    #[inline(always)]
    pub const fn cur_func(&self) -> &JSFunctionDef {
        unsafe { self.cur_func_def.as_ref().unwrap() }
    }

    /// Get current function def being parsed.
    /// Will panic if cur_func is null.
    #[inline(always)]
    pub const fn cur_func_mut(&self) -> &mut JSFunctionDef {
        unsafe { self.cur_func_def.as_mut().unwrap() }
    }

    /// Run closure with cur_func mut ref.
    /// If cur_func is null, returns None.
    #[inline]
    pub fn with_cur_func_mut<R>(
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
    pub fn push_scope(&mut self) -> Result<u16, i32> {
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
    pub fn pop_scope(&mut self) {
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
    pub fn close_scopes(&mut self, scope: i32, scope_stop: i32) {
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

#[deprecated(note = "use JSParseState::get_pos() instead")]
pub unsafe fn js_parse_get_pos(s: *mut JSParseState, sp: *mut JSParsePos) -> i32 {
    (*sp).last_line_num = (*s).last_line_num;
    (*sp).line_num = (*s).token.line_num;
    (*sp).ptr = (*s).token.ptr;
    (*sp).got_lf = (*s).got_lf;
    0
}

#[deprecated(note = "use JSParseState::seek_token() instead")]
pub unsafe fn js_parse_seek_token(s: *mut JSParseState, sp: *const JSParsePos) -> i32 {
    if (*s).seek_token(sp.as_ref().unwrap()).is_err() {
        -1
    } else {
        0
    }
}
