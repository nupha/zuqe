use std::ffi::CStr;

use crate::{
    bytecode::OpCodeTemp,
    context::JSContext,
    function::{get_first_lexical_var, JSFunctionDef, JSVarScope},
    value::JSValue,
};

use super::{
    emit::{emit_op_temp, emit_u16},
    token::{JSToken, JSTokenNum, JSTokenUnion},
};

/* allow the 'in' binary operator */
pub const PF_IN_ACCEPTED: i32 = 1 << 0;
/* allow function calls parsing in js_parse_postfix_expr() */
pub const PF_POSTFIX_CALL: i32 = 1 << 1;
/* allow arrow functions parsing in js_parse_postfix_expr() */
pub const PF_ARROW_FUNC: i32 = 1 << 2;
/* allow the exponentiation operator in js_parse_unary() */
pub const PF_POW_ALLOWED: i32 = 1 << 3;
/* forbid the exponentiation operator in js_parse_unary() */
pub const PF_POW_FORBIDDEN: i32 = 1 << 4;

pub const SKIP_HAS_SEMI: i32 = 1 << 0;
pub const SKIP_HAS_ELLIPSIS: i32 = 1 << 1;
pub const SKIP_HAS_ASSIGNMENT: i32 = 1 << 2;

pub struct JSParseState {
    pub ctx: *mut JSContext,
    pub last_line_num: u32, // line number of last token
    pub line_num: u32,      // line number of current offset
    pub filename: &'static CStr,
    pub token: JSToken,
    pub got_lf: bool, // true if got line feed before the current token
    pub last_ptr: *const u8,
    pub buf_ptr: *const u8,
    pub buf_end: *const u8,

    /// current function def
    /// TODO: use Box<JSFunctionDef>
    pub cur_func: *mut JSFunctionDef,

    pub is_module: bool, // parsing a module
    pub allow_html_comments: bool,
    pub ext_json: bool, // true if accepting JSON superset
}

impl JSParseState {
    pub unsafe fn new(
        ctx: *mut JSContext,
        input: *const u8,
        input_len: usize,
        filename: &'static CStr,
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
            buf_end: input.add(input_len),

            last_ptr: std::ptr::null(),
            cur_func: std::ptr::null_mut(),
            is_module: false,
            allow_html_comments: false,
            ext_json: false,
            got_lf: false,
        };

        s
    }

    /// enter new scope
    pub unsafe fn push_scope(&mut self) -> Result<i32, i32> {
        if !self.cur_func.is_null() {
            let fd = self.cur_func.as_mut().unwrap();

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

    pub unsafe fn pop_scope(&mut self) {
        if !self.cur_func.is_null() {
            // disable scoped variables
            let fd = self.cur_func.as_mut().unwrap();
            let scope = fd.scope_level;

            fd.scope_level = fd.scopes[scope as usize].parent;
            fd.scope_first = get_first_lexical_var(fd, fd.scope_level);

            emit_op_temp(self, OpCodeTemp::LEAVE_SCOPE);
            emit_u16(self, scope as _);
        }
    }

    pub unsafe fn close_scopes(&mut self, mut scope: i32, scope_stop: i32) {
        while scope > scope_stop {
            emit_op_temp(self, OpCodeTemp::LEAVE_SCOPE);
            emit_u16(self, scope as _);
            scope = (*self.cur_func).scopes[scope as usize].parent;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JSParsePos {
    pub last_line_num: u32,
    pub line_num: u32,
    pub got_lf: bool,
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
