use std::ffi::c_char;

use crate::{
    atom::{JSAtom, JSAtomInfo, JSAtomStruct, JSAtomTypeHint},
    class::{JSClassDef, JSClassID},
    context::JSContext,
    function::{JSFunctionBytecode, JSFunctionDef, JSNativeCallback, JSVarKindEnum, JSVarRef},
    generator::JSAsyncGeneratorData,
    promise::{JSPromiseReactionData, JSPromiseStateEnum, JobCallback, PromiseValue},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    value::{object::JSObject, JSString, JSValue, JSValueConst},
};

#[deprecated(note = "use JSValue::is_constructor() instead")]
#[inline(always)]
pub unsafe fn JS_IsConstructor(ctx: *const JSContext, val: JSValueConst) -> bool {
    val.is_constructor()
}

#[deprecated(note = "not used")]
pub unsafe fn check_exception_free(ctx: *mut JSContext, obj: JSValue) -> bool {
    unreachable!("unused");
}

#[deprecated(note = "use JSRuntime::atom_to_str() instead")]
pub unsafe fn JS_AtomGetStrRT(
    rt: *mut JSRuntime,
    buf: *mut c_char,
    buf_size: usize,
    atom: JSAtom,
) -> *const c_char {
    unreachable!("use JSRuntime::atom_to_str() instead")
}

#[deprecated(note = "use JSAtom::to_str() instead")]
pub unsafe fn JS_AtomGetStr(
    ctx: *mut JSContext,
    buf: *mut c_char,
    buf_size: usize,
    atom: JSAtom,
) -> *const c_char {
    unreachable!("unused");
}

#[deprecated(note = "use JSRuntime::add_atom() instead")]
/* string case (internal). Return Err if error. 'str' is freed. */
pub unsafe fn __JS_NewAtom(
    rt: *mut JSRuntime,
    str: *mut JSString,
    atom_type: JSAtomTypeHint,
) -> Result<JSAtom, i32> {
    unreachable!("use JSRuntime::add_atom() instead");
}

#[deprecated(note = "use JSRuntime::add_atom_str() instead")]
pub unsafe fn __JS_NewAtomInit(
    rt: &mut JSRuntime,
    str: &str,
    atom_type: JSAtomInfo,
) -> Result<JSAtom, i32> {
    unreachable!("use JSRuntime::add_atom_str() instead");
}

#[deprecated(note = "use JSString::atom_type() instead")]
pub unsafe fn js_get_atom_index(rt: &JSRuntime, p: *const JSAtomStruct) -> JSAtom {
    unreachable!("use JSString::atom_type() instead");

    // let mut i: u32 = (*p).hash_next; /* atom_index */
    // if (*p).atom_type() != JSAtomType::Symbol {
    //     i = *rt
    //         .atom_hash
    //         .add(((*p).hash() & (rt.atom_hash_size as u32 - 1)) as usize);
    //     let mut p1 = *rt.atom_array.add(i as usize) as *const JSString;

    //     while p1 != p {
    //         debug_assert!(i != 0);
    //         i = (*p1).hash_next;
    //         p1 = *rt.atom_array.add(i as usize);
    //     }
    // }
}

#[deprecated(note = "use JSAtom::to_value() instead")]
pub unsafe fn JS_AtomToString(ctx: *mut JSContext, atom: JSAtom) -> JSValue {
    unreachable!("use JSAtom::to_value() instead");
    // __JS_AtomToValue(ctx, atom, true)
}

/// Warning: 'p' is freed
#[deprecated(note = "use JSRuntime::add_atom_jsstring() instead")]
pub unsafe fn JS_NewAtomStr(ctx: *mut JSContext, p: *mut JSString) -> JSAtom {
    unreachable!("use JSRuntime::add_atom_jsstring() instead");
}

#[deprecated(note = "use JSAtom::to_utf8_string() instead")]
pub unsafe fn JS_AtomToCString(ctx: *mut JSContext, atom: JSAtom) -> *const c_char {
    unreachable!("use JSAtom::to_utf8_string() instead");
}

/// `data` values are not consumed.
#[deprecated(note = "use JSContext::create_native_function_with_data() instead")]
pub unsafe fn JS_NewCFunctionData(
    ctx: *mut JSContext,
    func: JSNativeCallback,
    length: u8,
    magic: i32,
    data_count: u8,
    data: *const JSValueConst,
) -> JSValue {
    (*ctx)
        .create_native_callable_object(
            func,
            length,
            magic,
            std::slice::from_raw_parts(data, data_count as usize),
        )
        .unwrap_or(JSValue::EXCEPTION)
}

/// op1, op2 ref_count unchanged
#[deprecated(note = "use JSValue::is_same() instead")]
pub unsafe fn js_same_value(ctx: *mut JSContext, op1: JSValueConst, op2: JSValueConst) -> bool {
    unreachable!("use JSValue::is_same_to() instead");
    // js_strict_eq2(
    //     ctx,
    //     op1.dup(),
    //     op2.dup(),
    //     JSStrictEqModeEnum::JS_EQ_SAME_VALUE,
    // )
}

#[deprecated(note = "unused")]
pub unsafe fn free_arg_list(ctx: *mut JSContext, tab: *mut JSValue, len: u32) {
    unreachable!("unused");
    // for i in 0..len {
    //     JS_FreeValue(ctx, *tab.add(i as _));
    // }
    // js_free(ctx, tab.cast());
}

#[deprecated(note = "unused")]
pub unsafe fn JS_ResizeAtomHash(rt: *mut JSRuntime, new_hash_size: i32) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSContext::get_class_proto() instead")]
pub unsafe fn JS_GetClassProto(ctx: *mut JSContext, class_id: JSClassID) -> JSValue {
    (*ctx).get_class_proto(class_id)
}

#[deprecated(note = "use JSContext::set_class_proto() instead")]
pub unsafe fn JS_SetClassProto(ctx: *mut JSContext, class_id: JSClassID, obj: JSValue) {
    (*ctx).set_class_proto(class_id, obj);
}

#[deprecated(note = "use JSRuntime::add_class() instead")]
pub unsafe fn JS_NewClass(
    rt: *mut JSRuntime,
    class_id: JSClassID,
    class_def: *const JSClassDef,
) -> Result<(), i32> {
    (*rt).add_class(class_id, class_def.as_ref().unwrap())
}

/* create a new object internal class. Return -1 if error, 0 if OK. The finalizer can be NULL if none is needed. */
#[deprecated(note = "use JSRuntime::add_class_internal() instead")]
pub unsafe fn JS_NewClass1(
    rt: *mut JSRuntime,
    class_id: JSClassID,
    class_def: *const JSClassDef,
    name: JSAtom,
) -> Result<(), i32> {
    unreachable!("use JSRuntime::add_class_internal() instead");
}

/// return -1 if exception
#[deprecated(note = "not used")]
pub unsafe fn expand_fast_array(ctx: *mut JSContext, p: *mut JSObject, new_len: u32) -> i32 {
    unreachable!("unused");
    // let mut slack: usize = 0;

    // /* XXX: potential arithmetic overflow */
    // let mut new_size = std::cmp::max(new_len, (*p).u.array.u1.size * 3 / 2);
    // let new_array_prop = js_realloc2(
    //     ctx,
    //     (*p).u.array.data.values.cast(),
    //     std::mem::size_of::<JSValue>() * new_size as usize,
    //     &mut slack,
    // )
    // .cast::<JSValue>();
    // if new_array_prop.is_null() {
    //     return -1;
    // }

    // new_size += (slack / std::mem::size_of::<JSValue>()) as u32;

    // (*(*p).u.array).data.values = new_array_prop;
    // (*(*p).u.array).u1.size = new_size;

    // return 0;
}

#[deprecated(note = "use JSContext::create_array() instead")]
pub unsafe fn js_allocate_fast_array(ctx: *const JSContext, len: i64) -> JSValue {
    unreachable!("use JSContext::create_array() instead");
}

#[deprecated(note = "use JSParseState::parse_string_escape() instead")]
pub unsafe fn lre_parse_escape(pp: *mut *const u8, allow_utf16: i32) -> Result<u32, i32> {
    unreachable!("use JSParseState::parse_string_escape() instead")
}

#[deprecated(note = "use JSFunctionDef::find_var_in_child_scope() instead")]
pub unsafe fn find_var_in_child_scope(
    ctx: *mut JSContext,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    scope_level: i32,
) -> Option<u16> {
    unreachable!("use JSFunctionDef::find_var_in_child_scope() instead");
}

#[deprecated(note = "use JSFunctionDef::add_scope_var() instead")]
pub unsafe fn add_scope_var(
    ctx: *mut JSContext,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    var_kind: JSVarKindEnum,
) -> i32 {
    unreachable!("use JSFunctionDef::add_scope_var() instead");
}

#[deprecated(note = "use JSStackFrame::get_var_ref() instead")]
pub unsafe fn get_var_ref(
    ctx: *mut JSContext,
    sf: *mut JSStackFrame,
    var_idx: u16,
    is_arg: bool,
) -> *mut JSVarRef {
    unreachable!("use JSStackFrame::get_var_ref() instead")
}

#[deprecated(note = "use JSAsyncGeneratorData::free() instead")]
pub unsafe fn js_async_generator_free(rt: *mut JSRuntime, s: *mut JSAsyncGeneratorData) {
    unreachable!("use JSAsyncGeneratorData::free() instead");
}

#[deprecated(note = "use AsyncGeneratorValue::function_call() instead")]
pub unsafe fn js_async_generator_function_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    unreachable!("use AsyncGeneratorValue::function_call() instead");
}

#[deprecated(note = "unused")]
unsafe fn js_promise_executor_new(ctx: *mut JSContext) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use JSPromiseReactionData::free() instead")]
pub(super) unsafe fn promise_reaction_data_free(
    rt: *mut JSRuntime,
    rd: *mut JSPromiseReactionData,
) {
    unreachable!("use JSPromiseReactionData::free() instead");
}

#[deprecated(note = "use JSContext::create_promise() instead")]
pub unsafe fn js_new_promise_capability(
    ctx: *mut JSContext,
    resolving_funcs: *mut JSValue,
    ctor: JSValueConst,
) -> JSValue {
    unreachable!("use JSContext::create_promise() instead")
}

/// create and return [resolve, reject] functions for `promise` object.
/// Note: `promise` is not consumed.
#[deprecated(note = "use PromiseValue::create_resolving_functions() instead")]
pub unsafe fn js_create_resolving_functions(
    ctx: &mut JSContext,
    promise: JSValueConst,
) -> Result<[JSValue; 2], i32> {
    unreachable!("use PromiseValue::create_resolving_functions() instead");
}

#[deprecated(note = "use JSString::new() instead")]
pub unsafe fn js_alloc_string_rt(
    rt: *mut JSRuntime,
    max_len: usize,
    is_wide_char: bool,
) -> *mut JSString {
    unreachable!("use JSString::new() instead");
    // match JSString::new_rt(rt.as_mut().unwrap(), max_len, is_wide_char) {
    //     Ok(s) => Box::into_raw(s),
    //     Err(_) => std::ptr::null_mut(),
    // }
}

#[deprecated(note = "use JSString::new() instead")]
pub unsafe fn js_alloc_string(
    ctx: *mut JSContext,
    max_len: usize,
    is_wide_char: bool,
) -> *mut JSString {
    unreachable!("use JSString::new() instead");

    // match JSString::with_capacity(ctx.as_mut().unwrap(), max_len, is_wide_char) {
    //     Ok(s) => Box::into_raw(s),
    //     Err(_) => std::ptr::null_mut(),
    // }
}

#[deprecated(note = "use JSString::free() instead")]
pub unsafe fn js_free_string(rt: &mut JSRuntime, str: *mut JSString) {
    unreachable!("unused");
}

#[deprecated(note = "use StringContent::cmp() instead")]
pub unsafe fn js_string_memcmp(p1: *const JSString, p2: *const JSString, len: usize) -> i32 {
    unreachable!("use StringContent::cmp() instead");

    // let res: i32;
    // if !(*p1).is_wide_char() {
    //     if !(*p2).is_wide_char() {
    //         res = memcmp((*p1).as_str8().cast(), (*p2).as_str8().cast(), len as usize);
    //     } else {
    //         res = -memcmp16_8(
    //             (*p2).as_str16().cast(),
    //             (*p1).as_str8().cast(),
    //             len as usize,
    //         );
    //     }
    // } else {
    //     if !(*p2).is_wide_char() {
    //         res = memcmp16_8(
    //             (*p1).as_str16().cast(),
    //             (*p2).as_str8().cast(),
    //             len as usize,
    //         );
    //     } else {
    //         res = memcmp16(
    //             (*p1).as_str16().cast(),
    //             (*p2).as_str16().cast(),
    //             len as usize,
    //         );
    //     }
    // }
    // res
}

#[deprecated(note = "use StringContent::hash() instead")]
pub unsafe fn hash_string(str: *const JSString, h: u32) -> u32 {
    unreachable!("use StringContent::hash() instead");
}

pub unsafe fn copy_str16(dst: *mut u16, p: *const JSString, offset: usize, len: usize) {
    unreachable!("unused");

    // if (*p).is_wide_char() {
    //     std::ptr::copy((*p).as_str16().add(offset), dst, len);
    // } else {
    //     let src1 = (*p).as_str8().add(offset);
    //     for i in 0..len {
    //         dst.add(i).write(src1.add(i).read() as u16);
    //     }
    // }
}

#[deprecated]
pub unsafe fn JS_ConcatString1(
    ctx: *mut JSContext,
    p1: *const JSString,
    p2: *const JSString,
) -> JSValue {
    unreachable!("unused");

    // let len = (*p1).len() + (*p2).len();
    // if len > JS_STRING_LEN_MAX {
    //     return ctx
    //         .as_mut()
    //         .unwrap()
    //         .JS_ThrowInternalError(c"string too long");
    // }
    // let is_wide_char = (*p1).is_wide_char() | (*p2).is_wide_char();
    // let mut p = if let Ok(s) = JSString::with_capacity(ctx.as_mut().unwrap(), len, is_wide_char) {
    //     s
    // } else {
    //     return JSValue::EXCEPTION;
    // };

    // if !is_wide_char {
    //     std::ptr::copy((*p1).as_str8(), p.as_str8_mut(), (*p1).len());
    //     std::ptr::copy(
    //         (*p2).as_str8(),
    //         (*p).as_str8_mut().add((*p1).len()),
    //         (*p2).len(),
    //     );
    //     *p.as_str8_mut().add(len) = 0;
    // } else {
    //     copy_str16(p.as_str16_mut(), p1, 0, (*p1).len());
    //     copy_str16(p.as_str16_mut().add((*p1).len()), p2, 0, (*p2).len());
    // }

    // return JS_MKPTR!(JSTag::String, Box::into_raw(p).cast());
}

#[deprecated(note = "use JSContext::create_string8() instead")]
pub unsafe fn js_new_string8(ctx: *mut JSContext, buf: *const u8, len: usize) -> JSValue {
    unreachable!("js_new_string8() is obsolete");
}

#[deprecated(note = "use JSContext::create_string16() instead")]
pub unsafe fn js_new_string16(ctx: *mut JSContext, buf: *const u16, len: usize) -> JSValue {
    unreachable!("js_new_string16() is obsolete");
}

#[deprecated(note = "use JSContext::create_string_char() instead")]
pub unsafe fn js_new_string_char(ctx: *mut JSContext, c: u16) -> JSValue {
    unreachable!("js_new_string_char() is obsolete");
}

/* return (NULL, 0) if exception. */
/* return pointer into a JSString with a live ref_count */
/* cesu8 determines if non-BMP1 codepoints are encoded as 1 or 2 utf-8 sequences */
#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCStringLen2(
    ctx: *mut JSContext,
    plen: *mut usize,
    val1: JSValueConst,
    cesu8: bool,
) -> *const c_char {
    unreachable!("use JSValue::to_utf8_string() instead");
}

#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCStringLen(
    ctx: *mut JSContext,
    plen: *mut usize,
    val1: JSValueConst,
) -> *const c_char {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCString(ctx: *mut JSContext, val1: JSValueConst) -> *const c_char {
    unreachable!("unused");
}

#[deprecated(note = "not used")]
pub unsafe fn JS_FreeCString(ctx: *mut JSContext, ptr: *const c_char) {
    unreachable!("JS_FreeCString() is not used any more");

    // if ptr.is_null() {
    //     return;
    // }

    // // ptr是 JS_ToCString() 系列返回返回的指针，它是JSString中 content 字段的地址
    // // 可以通过ptr计算出JSString的地址
    // let off = offset_of!(JSString, content) as isize;
    // let p = ptr.offset(-off).cast::<JSString>() as *mut JSString;
    // // let p = container_of!(ptr, JSString, content) as *mut JSString;
    // println!("ptr={ptr:p}, p={p:p}");
    // debug_assert_eq!(p, container_of!(ptr, JSString, content) as *mut JSString);

    // JS_FreeValue(ctx, JS_MKPTR!(JSTag::String, p.cast()));
}

/// return 0 if OK, < 0 if exception.
#[deprecated(note = "use JSContext::enqueue_job() instead")]
pub unsafe fn JS_EnqueueJob(
    ctx: *mut JSContext,
    job_func: *mut JobCallback,
    argc: i32,
    argv: *const JSValueConst,
) -> i32 {
    unreachable!()
}

/// return < 0 if exception, 0 if no job pending, 1 if a job was
/// executed successfully. the context of the job is stored in '*pctx'
#[deprecated(note = "use JSRuntime::execute_pending_job() instead")]
pub unsafe fn JS_ExecutePendingJob(rt: &mut JSRuntime, pctx: *mut *mut JSContext) -> i32 {
    match rt.execute_pending_job() {
        Ok((Some(ctx), _)) => {
            *pctx = ctx.as_raw().as_ptr();
            1
        }
        Ok((None, _)) => 0,
        Err((Some(ctx), _)) => {
            *pctx = ctx.as_raw().as_ptr();
            -1
        }
        Err((None, _)) => unreachable!(),
    }
}

#[deprecated(note = "use JSContext::create_promise() instead")]
pub unsafe fn JS_NewPromiseCapability(
    ctx: *mut JSContext,
    resolving_funcs: *mut JSValue,
) -> JSValue {
    if let Ok((promise, rr)) = (*ctx).create_promise(JSValue::UNDEFINED) {
        *resolving_funcs.add(0) = rr[0];
        *resolving_funcs.add(1) = rr[1];
        promise
    } else {
        JSValue::EXCEPTION
    }
}

#[deprecated(note = "use PromiseValue::state() instead")]
pub unsafe fn JS_PromiseState(
    ctx: *const JSContext,
    promise: JSValue,
) -> Result<JSPromiseStateEnum, i32> {
    promise.as_promise_unchecked().state()
}

/// to free `fd` and all its children
#[deprecated(note = "use JSFunctionDef::free() instead")]
pub unsafe fn js_free_function_def(ctx: *mut JSContext, fd: *mut JSFunctionDef) {
    Box::from_raw(fd).free(ctx.as_mut().unwrap());
}

/// create a function object from a function definition.
/// All the child functions are also created. It must be done this way to resolve all the variables.
/// The function definition is freed.
#[deprecated(note = "ise JSFunctionDef::make_function() instead")]
pub unsafe fn js_create_function(ctx: &mut JSContext, fd: *mut JSFunctionDef) -> JSValue {
    Box::from_raw(fd).make_function(ctx)
}

#[deprecated(note = "use JSFunctionBytecode::find_line_num() instead")]
pub unsafe fn find_line_num(
    ctx: *const JSContext,
    b: *mut JSFunctionBytecode,
    pc_value: u32,
) -> i32 {
    (*b).find_line_num(pc_value).map_or(-1, |x| x as i32)
}

/// Note: `value` is duplicated inside.
#[deprecated(note = "use PromiseValue::fulfill_or_reject() instead")]
pub unsafe fn fulfill_or_reject_promise(
    ctx: *mut JSContext,
    promise: PromiseValue,
    value: JSValueConst,
    is_reject: bool,
) {
    unreachable!("use PromiseValue::fulfill_or_reject() instead");
}

#[deprecated(note = "use PromiseValue::reject() instead")]
pub unsafe fn reject_promise(ctx: *mut JSContext, promise: PromiseValue, value: JSValueConst) {
    unreachable!("use PromiseValue::reject() instead");
}

#[deprecated(note = "use PromiseValue::then() instead")]
pub unsafe fn perform_promise_then(
    ctx: &mut JSContext,
    promise: PromiseValue,
    resolve_reject: &[JSValue; 2],
    cap_resolving_funcs: &[JSValue; 2],
) -> Result<(), i32> {
    promise.then(ctx, resolve_reject, cap_resolving_funcs)
}
