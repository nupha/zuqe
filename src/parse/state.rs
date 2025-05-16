use crate::{
    bytecode::OpCodeTemp,
    context::{JSContext, JSContextHandle},
    function::{get_first_lexical_var, JSFunctionDef, JSVarScope},
    value::JSValue,
};

use super::{
    emit::{emit_op_temp, emit_u16},
    token::{JSToken, JSTokenNum, JSTokenUnion},
};

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
    pub ctx: JSContextHandle,
    /// Line number of the previously processed token
    pub last_line_num: u32,
    /// Current line number being processed
    pub line_num: u32,
    /// Source file name being parsed
    pub filename: &'s str,
    /// Current token being processed
    pub token: JSToken,
    /// Indicates if a line feed was encountered before current token
    pub got_lf: bool,
    /// Pointer to the last processed character
    pub last_ptr: *const u8,
    /// Current position in the input buffer
    pub buf_ptr: *const u8,
    /// End of the input buffer
    pub buf_end: *const u8,

    /// Raw pointer to current parsing function def.
    pub cur_func: *mut JSFunctionDef,

    /// Indicates if parsing a JavaScript module
    pub is_module: bool,
    /// Controls whether HTML-style comments are allowed
    pub allow_html_comments: bool,
    /// Enables parsing of JSON superset syntax
    pub ext_json: bool,
}

impl<'s> JSParseState<'s> {
    /// Creates a new parser state with initial values
    ///
    /// # Safety
    /// This function is unsafe due to raw pointer usage
    ///
    /// # Arguments
    /// * `ctx` - JavaScript context pointer
    /// * `input` - Input buffer containing source code
    /// * `input_len` - Length of input buffer
    /// * `filename` - Name of source file
    pub fn new(
        ctx: JSContextHandle,
        input: *const u8,
        input_len: usize,
        filename: &'s str,
    ) -> Self {
        let s = Self {
            ctx,
            last_line_num: 0,
            line_num: 1,
            filename,
            token: JSToken {
                val: ' ' as i32,
                line_num: 1,
                ptr: std::ptr::null(),
                u: JSTokenUnion {
                    num: JSTokenNum { val: JSValue::ZERO },
                },
            },
            buf_ptr: input,
            buf_end: unsafe { input.add(input_len) },

            last_ptr: std::ptr::null(),
            cur_func: std::ptr::null_mut(),
            is_module: false,
            allow_html_comments: false,
            ext_json: false,
            got_lf: false,
        };

        s
    }

    /// Enters a new lexical scope for variable declarations
    ///
    /// Creates a new scope for variable declarations and emits the corresponding bytecode.
    /// The scope is tracked in the current function's scope chain.
    ///
    /// # Safety
    /// This function is unsafe due to raw pointer manipulation
    ///
    /// # Returns
    /// * `Ok(scope_id)` - ID of the newly created scope
    /// * `Err(-1)` - If scope creation fails
    pub unsafe fn push_scope(&mut self) -> Result<i32, i32> {
        if !self.cur_func.is_null() {
            let fd = self.cur_func();

            let scope = fd.scopes.len(); // new scope id

            if fd.scopes.len() == fd.scopes.capacity() {
                fd.scopes
                    .try_reserve_exact(fd.scopes.capacity() >> 2)
                    .map_err(|_| -1)?;
            }

            fd.scopes.push(JSVarScope {
                parent: fd.scope_level as i32,
                first: fd.scope_first,
            });
            fd.scope_level = scope as i32;

            emit_op_temp(self, OpCodeTemp::ENTER_SCOPE);
            emit_u16(self, scope as u16);
            Ok(scope as _)
        } else {
            Ok(0)
        }
    }

    /// Get current function def being parsed.
    /// Will panic if cur_func is null.
    #[inline(always)]
    pub unsafe fn cur_func(&mut self) -> &mut JSFunctionDef {
        self.cur_func.as_mut().unwrap()
    }

    /// Exits the current lexical scope
    ///
    /// Restores the parent scope and updates variable accessibility.
    /// Emits bytecode for scope cleanup.
    ///
    /// # Safety
    /// This function is unsafe due to raw pointer manipulation
    pub unsafe fn pop_scope(&mut self) {
        if !self.cur_func.is_null() {
            // disable scoped variables
            let fd = self.cur_func();
            let scope = fd.scope_level;

            fd.scope_level = fd.scopes[scope as usize].parent;
            fd.scope_first = get_first_lexical_var(fd, fd.scope_level);

            emit_op_temp(self, OpCodeTemp::LEAVE_SCOPE);
            emit_u16(self, scope as _);
        }
    }

    /// Closes multiple scopes up to a specified stop point
    ///
    /// Emits leave scope operations for all scopes from current until scope_stop.
    /// Used during control flow changes that require multiple scope exits.
    ///
    /// # Safety
    /// This function is unsafe due to raw pointer manipulation
    ///
    /// # Arguments
    /// * `scope` - Starting scope ID to close from
    /// * `scope_stop` - Stop closing scopes at this ID
    pub unsafe fn close_scopes(&mut self, mut scope: i32, scope_stop: i32) {
        while scope > scope_stop {
            emit_op_temp(self, OpCodeTemp::LEAVE_SCOPE);
            emit_u16(self, scope as _);
            scope = self.cur_func().scopes[scope as usize].parent;
        }
    }
}

#[derive(Debug, Clone, Copy)]
/// Represents a position in the source code during parsing
///
/// Used for backtracking and position tracking during parsing operations.
pub struct JSParsePos {
    /// Line number of the last processed token
    pub last_line_num: u32,
    /// Current line number
    pub line_num: u32,
    /// Indicates if a line feed was encountered
    pub got_lf: bool,
    /// Pointer to position in source buffer
    pub ptr: *const u8,
}

impl Default for JSParsePos {
    fn default() -> Self {
        Self {
            last_line_num: Default::default(),
            line_num: Default::default(),
            got_lf: Default::default(),
            ptr: std::ptr::null_mut(),
        }
    }
}

pub unsafe fn js_parse_get_pos(s: *mut JSParseState, sp: *mut JSParsePos) -> i32 {
    (*sp).last_line_num = (*s).last_line_num;
    (*sp).line_num = (*s).token.line_num;
    (*sp).ptr = (*s).token.ptr;
    (*sp).got_lf = (*s).got_lf;

    return 0;
}

#[deprecated(note = "use JSParseState::seek_token() instead")]
pub unsafe fn js_parse_seek_token(s: *mut JSParseState, sp: *const JSParsePos) -> i32 {
    if (*s).seek_token(sp.as_ref().unwrap()).is_err() {
        -1
    } else {
        0
    }
}
