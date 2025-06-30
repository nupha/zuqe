use std::{
    ffi::{CStr, CString, c_char, c_void},
    ptr::NonNull,
};

use crate::{
    JS_CLASS_ERROR, JS_CLASS_OBJECT,
    atom::{JS_ATOM_NULL, JSAtom, JSAtomInfo, JSAtomStruct, JSAtomTypeHint},
    bytecode::arithmatic::JSStrictEqModeEnum,
    class::{JSClassDef, JSClassID},
    context::JSContext,
    function::{
        JSAsyncFunctionState, JSCFunction, JSFunctionBytecode, JSFunctionDef, JSFunctionKindEnum,
        JSGlobalVar, JSNativeCallback, JSVarDefEnum, JSVarKindEnum, JSVarRef, LabelSlot,
        RelocEntry,
    },
    gc::{GCObjectHeader, JSGCObjectTypeEnum},
    generator::JSAsyncGeneratorData,
    iterator::JSIteratorKindEnum,
    module::JSModuleDef,
    parse::{JSParseExportEnum, JSParseFunctionEnum, JSParseState},
    promise::{JSPromiseReactionData, JSPromiseStateEnum, JobCallback, PromiseValue},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    utils::UncheckedInto,
    value::{
        JSString, JSStringInner, JSValue, JSValueConst,
        object::{JSAutoInitIDEnum, JSProperty, JSShape, PropertyEnum},
    },
};

#[deprecated(note = "use JSRuntime::finalize() instead")]
unsafe fn JS_FreeRuntime(rt: &mut JSRuntime) {
    unreachable!("unused");
}

#[deprecated(note = "use JSObject::opaque() instead")]
pub unsafe fn JS_GetOpaque(obj: JSValueConst, class_id: JSClassID) -> *mut c_void {
    unreachable!("use JSObject::opaque() instead")
    // if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
    //     return std::ptr::null_mut();
    // }
    // let p = JS_VALUE_GET_OBJ(obj);
    // if (*p).class_id() != class_id {
    //     return std::ptr::null_mut();
    // }
    // (*p).u.opaque
}

#[deprecated(note = "use JSObject::opaque_must() instead")]
pub unsafe fn JS_GetOpaque2(
    ctx: &JSContext,
    obj: JSValueConst,
    class_id: JSClassID,
) -> *mut c_void {
    let p = JS_GetOpaque(obj, class_id);
    if p.is_null() {
        ctx.JS_ThrowTypeErrorInvalidClass(class_id);
    }
    p
}

#[deprecated(note = "use JSObject::set_opaque() instead")]
pub unsafe fn JS_SetOpaque(obj: JSValue, opaque: *mut c_void) {
    unreachable!("use JSObject::set_opaque() instead")

    // if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
    //     let p = JS_VALUE_GET_OBJ(obj);
    //     (*p).u.opaque = opaque;
    // }
}

#[deprecated(note = "unused")]
pub fn JS_ThrowSyntaxErrorAtom(ctx: &JSContext, fmt: &str, atom: JSAtom) {
    unreachable!("unused")
}

#[deprecated(note = "use JSContext::JS_ThrowTypeErr()")]
pub unsafe fn JS_ThrowTypeErrorAtom(ctx: &JSContext, fmt: &CStr, atom: JSAtom) -> JSValue {
    unreachable!("unused")
}

#[inline(always)]
#[deprecated(note = "use Into::<JSValue>::into(bool) or From::<bool>::from(JSValue) instead")]
pub fn JS_NewBool(val: bool) -> JSValue {
    JSValue::Bool(val)
}

/// `val` keeps ref count unchanged after calling this function.
#[deprecated(note = "use JSValue::corece_to_string_value() instead")]
pub unsafe fn JS_ToStringInternal(
    ctx: &JSContext,
    val: JSValueConst,
    is_ToPropertyKey: bool,
) -> JSValue {
    unreachable!("use JSValue::corece_to_string_value() instead");
}

/// `val` keeps ref count unchanged after calling this function.
#[deprecated(note = "use JSValue::coerce_to_string_value() instead")]
pub unsafe fn JS_ToString(ctx: &JSContext, val: JSValueConst) -> JSValue {
    unreachable!("use JSValue::coerce_to_string_value() instead");
}

#[deprecated(note = "use JSValue::coerce_to_propkey_value() instead")]
pub unsafe fn JS_ToPropertyKey(ctx: &JSContext, val: JSValueConst) -> JSValue {
    unreachable!("use JSValue::coerce_to_propkey_value() instead");
}

/// `val` keeps ref count after calling this function.
#[deprecated(note = "use JSValue::coerce_number() instead")]
pub unsafe fn JS_ToNumber(ctx: &JSContext, val: JSValueConst) -> JSValue {
    val.coerce_to_number(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_to_primitive() + JS_FreeValue() instead")]
pub unsafe fn JS_ToPrimitiveFree(ctx: &JSContext, val: JSValue, hint: i32) -> JSValue {
    unreachable!("use JSValue::coerce_to_primitive() + JS_FreeValue() instead");
}

#[deprecated(note = "use JSValue::coerce_to_primitive() instead")]
pub unsafe fn JS_ToPrimitive(ctx: &JSContext, val: JSValue, hint: i32) -> JSValue {
    val.coerce_to_primitive(ctx, hint)
        .unwrap_or(JSValue::EXCEPTION)

    //JS_ToPrimitiveFree(ctx, val.dup(), hint)
}

#[deprecated(note = "use JSValue::is_callable() instead")]
pub unsafe fn JS_IsFunction(ctx: &JSContext, val: JSValueConst) -> bool {
    val.is_callable()
}

#[deprecated(note = "use JSValue::is_constructor() instead")]
#[inline(always)]
pub unsafe fn JS_IsConstructor(ctx: &JSContext, val: JSValueConst) -> bool {
    val.is_constructor()
}

#[deprecated(note = "use JSContext::add_global_constructor() instead")]
pub unsafe fn JS_NewGlobalCConstructor2(
    ctx: &JSContext,
    func_obj: JSValue,
    name: &CStr,
    proto: JSValueConst,
) {
    ctx.add_global_constructor(name.to_string_lossy().as_ref(), &func_obj, &proto);
}

#[deprecated(note = "use JSContext::create_global_constructor() instead")]
pub unsafe fn JS_NewGlobalCConstructor(
    ctx: &JSContext,
    name: &CStr,
    func: JSCFunction,
    length: i32,
    proto: JSValueConst,
) -> JSValueConst {
    unreachable!("unused");
}

#[deprecated(note = "use JSRuntime::is_class_registered() instead")]
pub unsafe fn JS_IsRegisteredClass(rt: &JSRuntime, class_id: JSClassID) -> bool {
    (*rt).is_class_registered(class_id)
}

#[deprecated(note = "not used")]
pub unsafe fn check_exception_free(ctx: &JSContext, obj: JSValue) -> bool {
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
    ctx: &JSContext,
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
pub unsafe fn JS_AtomToString(ctx: &JSContext, atom: JSAtom) -> JSValue {
    unreachable!("use JSAtom::to_value() instead");
    // __JS_AtomToValue(ctx, atom, true)
}

/// Warning: 'p' is freed
#[deprecated(note = "use JSRuntime::add_atom_jsstring() instead")]
pub unsafe fn JS_NewAtomStr(ctx: &JSContext, p: *mut JSString) -> JSAtom {
    unreachable!("use JSRuntime::add_atom_jsstring() instead");
}

#[deprecated(note = "use JSAtom::to_utf8_string() instead")]
pub unsafe fn JS_AtomToCString(ctx: &JSContext, atom: JSAtom) -> *const c_char {
    unreachable!("use JSAtom::to_utf8_string() instead");
}

/// `data` values are not consumed.
#[deprecated(note = "use JSContext::create_native_callable_object() instead")]
pub unsafe fn JS_NewCFunctionData(
    ctx: &JSContext,
    func: JSNativeCallback,
    length: u8,
    magic: i32,
    data_count: u8,
    data: *const JSValueConst,
) -> JSValue {
    ctx.create_native_callable_object(
        func,
        length,
        magic,
        std::slice::from_raw_parts(data, data_count as usize),
    )
    .unwrap_or(JSValue::EXCEPTION)
}

/// op1, op2 ref_count unchanged
#[deprecated(note = "use JSValue::is_same() instead")]
pub unsafe fn js_same_value(ctx: &JSContext, op1: JSValueConst, op2: JSValueConst) -> bool {
    unreachable!("use JSValue::is_same_to() instead");
    // js_strict_eq2(
    //     ctx,
    //     op1.dup(),
    //     op2.dup(),
    //     JSStrictEqModeEnum::JS_EQ_SAME_VALUE,
    // )
}

#[deprecated(note = "unused")]
pub unsafe fn free_arg_list(ctx: &JSContext, tab: *mut JSValue, len: u32) {
    unreachable!("unused");
    // for i in 0..len {
    //     JS_FreeValue(ctx, *tab.add(i as _));
    // }
    // js_free(ctx, tab.cast());
}

#[deprecated(note = "use JSParseState::parse_string() instead")]
pub unsafe fn js_parse_string(
    s: *mut JSParseState,
    sep: char,
    do_throw: bool,
    p: *const u8,
    token: *mut c_void, //JSToken,
    pp: *mut *const u8,
) -> i32 {
    unreachable!("unused");
    // if (*s)
    //     .parse_string(sep, do_throw, p, Some(token.as_mut().unwrap()), pp)
    //     .is_err()
    // {
    //     -1
    // } else {
    //     0
    // }
}

#[deprecated(note = "use StringContent::parse_u32() instead")]
pub unsafe fn is_num_string(pval: &mut u32, p: *const JSStringInner) -> bool {
    match (*p).content().parse_u32() {
        Ok(n) => {
            *pval = n;
            true
        }
        Err(_) => false,
    }
}

/// `val` is consumed.
#[deprecated(note = "use JSValue::coerce_to_string_value() instead")]
pub fn JS_ToStringFree(ctx: &JSContext, val: JSValue) -> JSValue {
    val.coerce_to_string_value(ctx)
        .unwrap_or(JSValue::EXCEPTION)
}

/// `val` is consumed.
#[deprecated(note = "use JSValue::to_locale_string() instead")]
pub unsafe fn JS_ToLocaleStringFree(ctx: &JSContext, val: JSValue) -> JSValue {
    let r = val.to_locale_string(ctx).unwrap_or(JSValue::EXCEPTION);
    r
}

#[deprecated(note = "unused")]
pub unsafe fn JS_ResizeAtomHash(rt: *mut JSRuntime, new_hash_size: i32) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSContext::get_class_proto() instead")]
pub unsafe fn JS_GetClassProto(ctx: &JSContext, class_id: JSClassID) -> JSValue {
    ctx.get_class_proto(class_id)
}

#[deprecated(note = "use JSContext::set_class_proto() instead")]
pub unsafe fn JS_SetClassProto(ctx: &JSContext, class_id: JSClassID, obj: JSValue) {
    ctx.set_class_proto(class_id, obj);
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
pub unsafe fn expand_fast_array(ctx: &JSContext, p: *mut JSValue, new_len: u32) -> i32 {
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
pub unsafe fn js_allocate_fast_array(ctx: &JSContext, len: i64) -> JSValue {
    unreachable!("use JSContext::create_array() instead");
}

#[deprecated(note = "use JSParseState::parse_string_escape() instead")]
pub unsafe fn lre_parse_escape(pp: *mut *const u8, allow_utf16: i32) -> Result<u32, i32> {
    unreachable!("use JSParseState::parse_string_escape() instead")
}

#[deprecated(note = "use JSFunctionDef::find_var_in_child_scope() instead")]
pub unsafe fn find_var_in_child_scope(
    ctx: &JSContext,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    scope_level: i32,
) -> Option<u16> {
    unreachable!("use JSFunctionDef::find_var_in_child_scope() instead");
}

#[deprecated(note = "use JSFunctionDef::add_scope_var() instead")]
pub unsafe fn add_scope_var(
    ctx: &JSContext,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    var_kind: JSVarKindEnum,
) -> i32 {
    unreachable!("use JSFunctionDef::add_scope_var() instead");
}

#[deprecated(note = "use JSStackFrame::get_var_ref() instead")]
pub unsafe fn get_var_ref(
    ctx: &JSContext,
    sf: *mut JSStackFrame,
    var_idx: u16,
    is_arg: bool,
) -> *mut JSVarRef {
    unreachable!("use JSStackFrame::get_var_ref() instead")
}

#[deprecated(note = "use JSAsyncGeneratorData::dispose() instead")]
pub unsafe fn js_async_generator_free(rt: *mut JSRuntime, s: *mut JSAsyncGeneratorData) {
    unreachable!("use JSAsyncGeneratorData::dispose() instead");
}

#[deprecated(note = "use AsyncGeneratorValue::function_call() instead")]
pub unsafe fn js_async_generator_function_call(
    ctx: &JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    unreachable!("use AsyncGeneratorValue::function_call() instead");
}

#[deprecated(note = "unused")]
unsafe fn js_promise_executor_new(ctx: &JSContext) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use JSPromiseReactionData::dispose() instead")]
pub(super) unsafe fn promise_reaction_data_free(
    rt: *mut JSRuntime,
    rd: *mut JSPromiseReactionData,
) {
    unreachable!("use JSPromiseReactionData::dispose() instead");
}

#[deprecated(note = "use JSContext::create_promise() instead")]
pub unsafe fn js_new_promise_capability(
    ctx: &JSContext,
    resolving_funcs: *mut JSValue,
    ctor: JSValueConst,
) -> JSValue {
    unreachable!("use JSContext::create_promise() instead")
}

/// create and return [resolve, reject] functions for `promise` object.
/// Note: `promise` is not consumed.
#[deprecated(note = "use PromiseValue::create_resolving_functions() instead")]
pub unsafe fn js_create_resolving_functions(
    ctx: &JSContext,
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
    ctx: &JSContext,
    max_len: usize,
    is_wide_char: bool,
) -> *mut JSString {
    unreachable!("use JSString::new() instead");

    // match JSString::with_capacity(ctx, max_len, is_wide_char) {
    //     Ok(s) => Box::into_raw(s),
    //     Err(_) => std::ptr::null_mut(),
    // }
}

#[deprecated(note = "use JSString::dispose() instead")]
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
    ctx: &JSContext,
    p1: *const JSString,
    p2: *const JSString,
) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use JSContext::create_string8() instead")]
pub unsafe fn js_new_string8(ctx: &JSContext, buf: *const u8, len: usize) -> JSValue {
    unreachable!("js_new_string8() is obsolete");
}

#[deprecated(note = "use JSContext::create_string16() instead")]
pub unsafe fn js_new_string16(ctx: &JSContext, buf: *const u16, len: usize) -> JSValue {
    unreachable!("js_new_string16() is obsolete");
}

#[deprecated(note = "use JSContext::create_string_char() instead")]
pub unsafe fn js_new_string_char(ctx: &JSContext, c: u16) -> JSValue {
    unreachable!("js_new_string_char() is obsolete");
}

/* return (NULL, 0) if exception. */
/* return pointer into a JSString with a live ref_count */
/* cesu8 determines if non-BMP1 codepoints are encoded as 1 or 2 utf-8 sequences */
#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCStringLen2(
    ctx: &JSContext,
    plen: *mut usize,
    val1: JSValueConst,
    cesu8: bool,
) -> *const c_char {
    unreachable!("use JSValue::to_utf8_string() instead");
}

#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCStringLen(
    ctx: &JSContext,
    plen: *mut usize,
    val1: JSValueConst,
) -> *const c_char {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCString(ctx: &JSContext, val1: JSValueConst) -> *const c_char {
    unreachable!("unused");
}

#[deprecated(note = "not used")]
pub unsafe fn JS_FreeCString(ctx: &JSContext, ptr: *const c_char) {
    unreachable!("JS_FreeCString() is not used any more");
}

/// return 0 if OK, < 0 if exception.
#[deprecated(note = "use JSContext::enqueue_job() instead")]
pub unsafe fn JS_EnqueueJob(
    ctx: &JSContext,
    job_func: *mut JobCallback,
    argc: i32,
    argv: *const JSValueConst,
) -> i32 {
    unreachable!()
}

/// return < 0 if exception, 0 if no job pending, 1 if a job was
/// executed successfully. the context of the job is stored in '*pctx'
#[deprecated(note = "use JSRuntime::execute_pending_job() instead")]
pub unsafe fn JS_ExecutePendingJob(rt: &mut JSRuntime, pctx: *mut &JSContext) -> i32 {
    unreachable!("unused");
    // match rt.execute_pending_job() {
    //     Ok((Some(ctx), _)) => {
    //         *pctx = ctx.as_raw().as_ptr();
    //         1
    //     }
    //     Ok((None, _)) => 0,
    //     Err((Some(ctx), _)) => {
    //         *pctx = ctx.as_raw().as_ptr();
    //         -1
    //     }
    //     Err((None, _)) => unreachable!(),
    // }
}

#[deprecated(note = "use JSContext::create_promise() instead")]
pub unsafe fn JS_NewPromiseCapability(ctx: &JSContext, resolving_funcs: *mut JSValue) -> JSValue {
    unreachable!("unused");
    // if let Ok((promise, rr)) = ctx.create_promise(&JSValue::UNDEFINED) {
    //     *resolving_funcs.add(0) = rr[0];
    //     *resolving_funcs.add(1) = rr[1];
    //     promise.into()
    // } else {
    //     JSValue::EXCEPTION
    // }
}

#[deprecated(note = "use PromiseValue::state() instead")]
pub unsafe fn JS_PromiseState(
    ctx: &JSContext,
    promise: JSValue,
) -> Result<JSPromiseStateEnum, i32> {
    UncheckedInto::<PromiseValue>::unchecked_into(promise).state()
}

/// to free `fd` and all its children
#[deprecated(note = "use JSFunctionDef::dispose() instead")]
pub unsafe fn js_free_function_def(ctx: &JSContext, fd: *mut JSFunctionDef) {
    unreachable!("unused");
}

/// create a function object from a function definition.
/// All the child functions are also created. It must be done this way to resolve all the variables.
/// The function definition is freed.
#[deprecated(note = "ise JSFunctionDef::make_function() instead")]
pub unsafe fn js_create_function(ctx: &JSContext, fd: *mut JSFunctionDef) -> JSValue {
    unreachable!("unused");
    // Box::from_raw(fd).make_function(ctx)
}

#[deprecated(note = "use JSFunctionBytecode::find_line_num() instead")]
pub unsafe fn find_line_num(ctx: &JSContext, b: *mut JSFunctionBytecode, pc_value: u32) -> i32 {
    (*b).find_line_num(pc_value).unwrap_or(-1)
}

/// Note: `value` is duplicated inside.
#[deprecated(note = "use PromiseValue::fulfill_or_reject() instead")]
pub unsafe fn fulfill_or_reject_promise(
    ctx: &JSContext,
    promise: PromiseValue,
    value: JSValueConst,
    is_reject: bool,
) {
    unreachable!("use PromiseValue::fulfill_or_reject() instead");
}

#[deprecated(note = "use PromiseValue::reject() instead")]
pub unsafe fn reject_promise(ctx: &JSContext, promise: PromiseValue, value: JSValueConst) {
    unreachable!("use PromiseValue::reject() instead");
}

#[deprecated(note = "use PromiseValue::then() instead")]
pub unsafe fn perform_promise_then(
    ctx: &JSContext,
    promise: PromiseValue,
    resolve_reject: &[JSValue; 2],
    cap_resolving_funcs: &[JSValue; 2],
) -> Result<(), i32> {
    promise.reaction(ctx, resolve_reject, cap_resolving_funcs)
}

/// parse import statement
#[deprecated(note = "use JSParseState::parse_import() instead")]
pub unsafe fn js_parse_import(s: *mut JSParseState) -> i32 {
    (*s).parse_import().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_from_clause() instead")]
pub unsafe fn js_parse_from_clause(s: *mut JSParseState) -> JSAtom {
    unreachable!("use JSParseState::parse_from_clause() instead");
}

/// Run the <eval> function of the module and of all its requested modules.
/// Return a promise or an exception.
#[deprecated(note = "use JSModuleDef::eval() instead")]
pub unsafe fn js_evaluate_module(ctx: &JSContext, m: *mut JSModuleDef) -> JSValue {
    (*m).eval(ctx)
}

#[deprecated(note = "use PromiseValue::result() instead")]
pub unsafe fn JS_PromiseResult(ctx: &JSContext, promise: JSValue) -> JSValue {
    UncheckedInto::<PromiseValue>::unchecked_into(promise).result()
}

/* allowed flags:
   JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JS_PROP_ENUMERABLE
   JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
   JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_HAS_ENUMERABLE,
   JS_PROP_THROW, JS_PROP_NO_EXOTIC.
   If JS_PROP_THROW is set, return an exception instead of FALSE.
   if JS_PROP_NO_EXOTIC is set, do not call the exotic
   define_own_property callback.
   return -1 (exception), FALSE or TRUE.
*/
/// Note: property `val` is duplicated inside.
#[deprecated(note = "use JSValue::define_property() instead")]
pub unsafe fn JS_DefineProperty(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValueConst,
    getter: JSValueConst,
    setter: JSValueConst,
    flags: u32,
) -> i32 {
    this_obj
        .define_property(ctx, prop, &val, &getter, &setter, flags)
        .map_or(-1, |b| b as i32)
}

/// shortcut to add or redefine a new property value.
/// Note: property value is consumed.
#[deprecated(note = "use JSValue::defined_property_value() instead")]
pub unsafe fn JS_DefinePropertyValue(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value(ctx, prop, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_str() instead")]
pub unsafe fn JS_DefinePropertyValueStr(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: &CStr,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_str(ctx, prop.to_string_lossy().as_ref(), val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_value() instead")]
pub unsafe fn JS_DefinePropertyValueValue(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: JSValue,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_value(ctx, prop, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::create_data_property_u32() instead")]
pub unsafe fn JS_CreateDataPropertyUint32(
    ctx: &JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .create_data_property_u32(ctx, idx, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_u32() instead")]
pub unsafe fn JS_DefinePropertyValueUint32(
    ctx: &JSContext,
    this_obj: JSValueConst,
    idx: u32,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_u32(ctx, idx, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_i64() instead")]
pub unsafe fn JS_DefinePropertyValueInt64(
    ctx: &JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_i64(ctx, idx, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_desc() instead")]
pub unsafe fn JS_DefinePropertyDesc(
    ctx: &JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    desc: JSValueConst,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("unused");
}

/// `getter` and `setter` are consumed.
#[deprecated(note = "use JSValue::define_property_getset() instead")]
pub unsafe fn JS_DefinePropertyGetSet(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_getset(ctx, prop, getter, setter, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSObject::define_auto_init_property() instead")]
pub unsafe fn JS_DefineAutoInitProperty(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    id: JSAutoInitIDEnum,
    opaque: *mut std::ffi::c_void,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("use JSObject::define_auto_init_property() instead");
    // this_obj.define_auto_init_property(ctx, prop, id, opaque, flags)
}

/// returned value has its ref_count increased by 1.
#[deprecated(note = "use JSValue::get_property_internal() instead")]
pub unsafe fn JS_GetPropertyInternal(
    ctx: &JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    this_obj: JSValueConst,
    throw_ref_error: bool,
) -> JSValue {
    obj.get_property_internal(ctx, prop, &this_obj, throw_ref_error)
}

#[deprecated(note = "use JSValue::get_property() instead")]
#[inline(always)]
pub unsafe fn JS_GetProperty(ctx: &JSContext, this_obj: JSValueConst, prop: JSAtom) -> JSValue {
    this_obj.get_property(ctx, prop)
}

#[deprecated(note = "use JSValue::get_property_str() instead")]
pub unsafe fn JS_GetPropertyStr(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: *const c_char,
) -> JSValue {
    this_obj.get_property_str(ctx, CStr::from_ptr(prop).to_string_lossy().as_ref())
    // let atom = JS_NewAtom(ctx, CStr::from_ptr(prop));
    // let ret = JS_GetProperty(ctx, this_obj, atom);
    // JS_FreeAtom(ctx, atom);
    // return ret;
}

/// NOTE: returned property value has increased ref_count by 1
#[deprecated(note = "use JSValue::get_property_value() instead")]
pub unsafe fn JS_GetPropertyValue(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: JSValue,
) -> JSValue {
    this_obj.get_property_value(ctx, &prop)
}

#[deprecated(note = "use JSValue::get_property_u32() instead")]
pub unsafe fn JS_GetPropertyUint32(ctx: &JSContext, this_obj: JSValueConst, idx: u32) -> JSValue {
    this_obj.get_property_u32(ctx, idx)
}

#[deprecated(note = "use JSValue::get_property_i64() instead")]
pub unsafe fn JS_GetPropertyInt64(ctx: &JSContext, obj: JSValueConst, idx: i64) -> JSValue {
    obj.get_property_i64(ctx, idx)
}

/// Check if an object has a generalized numeric property `idx`. Returns:
/// Err for exception,
/// Ok(Some(_)) if property exists,
/// Ok(None) if proprty does not exist.
#[deprecated(note = "use JSValue::try_get_property_i64() instead")]
pub unsafe fn JS_TryGetPropertyInt64(
    ctx: &JSContext,
    obj: JSValueConst,
    idx: i64,
) -> Result<Option<JSValue>, i32> {
    obj.try_get_property_i64(ctx, idx)
}

/* 返回 -1、FALSE 或 TRUE。如果不可配置或无效对象则返回 FALSE。出现异常时返回 -1。
flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT */
#[deprecated(note = "use JSValue::delete_property() instead")]
pub unsafe fn JS_DeleteProperty(
    ctx: &JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::delete_property_i64() instead")]
pub unsafe fn JS_DeletePropertyInt64(
    ctx: &JSContext,
    obj: JSValueConst,
    idx: i64,
    flags: u32,
) -> Result<bool, i32> {
    obj.delete_property_i64(ctx, idx, flags)
}

/// the indirection is needed to make 'eval' optional.
#[deprecated(note = "use JSContext::eval_internal() instead")]
pub unsafe fn JS_EvalInternal(
    ctx: &JSContext,
    this_obj: JSValueConst,
    input: &CStr,
    filename: &str,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    ctx.eval_internal(this_obj, input, filename, flags, scope_idx)
}

#[deprecated(note = "use JSContext::eval_this() instead")]
pub unsafe fn JS_EvalThis(
    ctx: &JSContext,
    this_obj: JSValueConst,
    input: &CStr,
    filename: &str,
    flags: u32,
) -> JSValue {
    ctx.eval_this(this_obj, input, filename, flags)
}

#[deprecated(note = "use JSContext::eval() instead")]
pub unsafe fn JS_Eval(ctx: &JSContext, input: &CStr, filename: &str, eval_flags: u32) -> JSValue {
    ctx.eval(input, filename, eval_flags)
}

/// both `this_obj` and `val` are not consumed.
#[deprecated(note = "use JSValue::eval() instead")]
pub unsafe fn JS_EvalObject(
    ctx: &JSContext,
    this_obj: JSValueConst,
    val: JSValueConst,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    if !val.is_string() {
        return val.clone(); // dup and return val
    }

    match val.coerce_to_utf8(ctx) {
        Ok(str) => ctx.eval_internal(
            this_obj,
            CString::new(str.as_bytes()).unwrap().as_c_str(),
            "<input>",
            flags,
            scope_idx,
        ),
        Err(_) => JSValue::EXCEPTION,
    }
}

#[deprecated(note = "use JSModuleDef::build_ns() instead")]
pub unsafe fn js_build_module_ns(ctx: &JSContext, m: *mut JSModuleDef) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use JSModuleDef::ns() instead")]
pub unsafe fn js_get_module_ns(ctx: &JSContext, m: *mut JSModuleDef) -> JSValue {
    (*m).ns(ctx).unwrap_or(JSValue::EXCEPTION)
}

/// build traceback, set it as property of `error_obj`.
/// if filename/line number is provided, they are defined as property of `error_obj` too.
/// `error_obj` ref count is unchanged.
#[deprecated(note = "use JSContext::build_backtrace_to() instead")]
pub unsafe fn build_backtrace(
    ctx: &JSContext,
    error_obj: JSValueConst,
    file_line: Option<(&str, i32)>, // filename and line number
    backtrace_flags: i32,
) {
    ctx.build_backtrace_to(&error_obj, file_line, backtrace_flags);
}

/// op1, op2 are consumed
#[deprecated(note = "use JSValue::strict_eq_with_mode() instead")]
pub unsafe fn js_strict_eq2(
    ctx: &JSContext,
    op1: JSValue,
    op2: JSValue,
    eq_mode: JSStrictEqModeEnum,
) -> bool {
    unreachable!("use JSValue::strict_eq_with_mode() instead");
    // let res = op1.strict_eq_with_mode(op2, eq_mode);
    // JS_FreeValue(ctx, op1);
    // JS_FreeValue(ctx, op2);
    // res
}

/// op1, op2 are consumed
#[deprecated(note = "use JSValue::strict_eq() instead")]
pub unsafe fn js_strict_eq(ctx: &JSContext, op1: JSValue, op2: JSValue) -> bool {
    unreachable!("use JSValue::strict_eq() instead");
    // let res = op1.strict_eq(op2);
    // JS_FreeValue(ctx, op1);
    // JS_FreeValue(ctx, op2);
    // res
}

#[deprecated(note = "use JSValue::is_same0() instead")]
pub unsafe fn js_same_value_zero(ctx: &JSContext, op1: JSValueConst, op2: JSValueConst) -> bool {
    op1.is_same0(&op2)
}

/// always returns -1
#[deprecated(note = "use JSParseState::throw_syntax_error() instead")]
pub unsafe fn js_parse_error(s: *mut JSParseState, message: &CStr) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSFunctionDef::find_global_var() instead")]
pub unsafe fn find_global_var(fd: *mut JSFunctionDef, name: JSAtom) -> *mut JSGlobalVar {
    (*fd)
        .find_global_var(name)
        .map_or(std::ptr::null_mut(), |v| v as *const JSGlobalVar as *mut _)
}

/* add a global variable definition */
#[deprecated(note = "use JSFunctionDef::add_global_var() instead")]
pub unsafe fn add_global_var(
    ctx: &JSContext,
    s: *mut JSFunctionDef,
    name: JSAtom,
) -> *mut JSGlobalVar {
    (*s).add_global_var(ctx, name)
        .map_or(std::ptr::null_mut(), |p| p as *mut JSGlobalVar)
}

#[deprecated(note = "use JSFunctionDef::add_arg() instead")]
pub unsafe fn add_arg(ctx: &JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).add_arg(ctx, name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::find_arg() instead")]
/* return the variable index or -1 if not found,
add ARGUMENT_VAR_OFFSET for argument variables */
pub unsafe fn find_arg(ctx: &JSContext, fd: *const JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).find_arg(name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::find_var_or_arg() instead")]
pub unsafe fn find_var(ctx: &JSContext, fd: *const JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).find_var(name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_var() instead")]
/* return the variable index or -1 if error */
pub unsafe fn add_var(ctx: &JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).add_var(ctx, name).map_or(-1, |x| x as i32)
}

// return the constant pool index. 'val' is not duplicated.
#[deprecated(note = "use JSParseState::cpool_add() instead")]
pub unsafe fn cpool_add(s: *mut JSParseState, ctx: &JSContext, val: JSValue) -> i32 {
    let fd = (*s).cur_func_mut();
    fd.cpool_add(val).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use LabelInfo::add_reloc() instead")]
pub unsafe fn add_reloc(
    ctx: &JSContext,
    ls: *mut LabelSlot,
    addr: usize,
    size: i32,
) -> *mut RelocEntry {
    unreachable!("use LabelInfo::add_reloc() instead");
}

#[deprecated(note = "use JSFunctionDef::add_label() instead")]
pub unsafe fn new_label_fd(fd: *mut JSFunctionDef, label: i32) -> i32 {
    if label < 0 {
        (*fd).add_label().map_or(-1, |x| x as i32)
    } else {
        label
    }
}

#[deprecated(note = "use JSFunctionDef::add_label() instead")]
pub unsafe fn new_label(s: *mut JSParseState) -> i32 {
    (*s).cur_func_mut().add_label().map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSParseState::parse_template_part() instead")]
pub unsafe fn js_parse_template_part(s: *mut JSParseState, p: *const u8) -> i32 {
    (*s).parse_template_part(p).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::push_state() instead")]
pub unsafe fn push_scope(s: *mut JSParseState) -> i32 {
    (*s).push_scope().map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSParseState::pop_state() instead")]
pub unsafe fn pop_scope(s: *mut JSParseState) {
    (*s).pop_scope();
}

#[deprecated(note = "use JSParseState::close_scopes() instead")]
pub unsafe fn close_scopes(s: *mut JSParseState, scope: i32, scope_stop: i32) {
    (*s).close_scopes(scope, scope_stop);
}

// 'c' is the first character. Return JS_ATOM_NULL in case of error
#[deprecated(note = "use JSParseState::parse_ident() instead")]
pub unsafe fn parse_ident(
    s: *mut JSParseState,
    pp: *mut *const u8,
    pident_has_escape: *mut bool,
    c: char,
    is_private: bool,
) -> JSAtom {
    let (a, esc) = (*s)
        .parse_ident(pp, c, is_private)
        .unwrap_or((JS_ATOM_NULL, false));
    *pident_has_escape = esc;
    a
}

/// find a variable declaration in a given scope
#[deprecated(note = "use JSFunctionDef::find_var_in_scope() instead")]
pub fn find_var_in_scope(
    ctx: &JSContext,
    fd: &mut JSFunctionDef,
    name: JSAtom,
    scope_level: i32,
) -> Option<u16> {
    debug_assert!(scope_level >= 0);
    fd.find_var_in_scope(name, scope_level as _)
}

#[deprecated(note = "use JSFunctionDef::find_lexical_global_var_mut() instead")]
pub unsafe fn find_lexical_global_var(fd: *mut JSFunctionDef, name: JSAtom) -> *mut JSGlobalVar {
    (*fd)
        .find_lexical_global_var_mut(name)
        .map_or(std::ptr::null_mut(), |v| v as *mut _)
}

#[deprecated(note = "use JSFSunctionDef::find_lexical_decl() instead")]
pub unsafe fn find_lexical_decl(
    ctx: &JSContext,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    scope_idx: i32,
    check_catch_var: bool,
) -> i32 {
    debug_assert!(scope_idx >= 0);
    (*fd)
        .find_lexical_decl(name, scope_idx as _, check_catch_var)
        .map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::get_first_lexical_var() instead")]
pub unsafe fn get_first_lexical_var(fd: *mut JSFunctionDef, scope: i32) -> i32 {
    (*fd).get_first_lexical_var(scope).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSParseState::parse_program() instead")]
pub unsafe fn js_parse_program(s: &mut JSParseState) -> i32 {
    if s.parse_program().is_err() { -1 } else { 0 }
}

#[deprecated(note = "use JSParseState::parse_directives() instead")]
pub unsafe fn js_parse_directives(s: *mut JSParseState) -> i32 {
    if (*s).parse_directives().is_err() {
        -1
    } else {
        0
    }
}

#[deprecated(note = "use JSParseState::parse_source_element() instead")]
pub unsafe fn js_parse_source_element(s: *mut JSParseState) -> i32 {
    if (*s).parse_source_element().is_err() {
        -1
    } else {
        0
    }
}

// allowed parse_flags: PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_var() instead")]
pub unsafe fn js_parse_var(
    s: *mut JSParseState,
    parse_flags: i32,
    tok: i32,
    export_flag: bool,
) -> i32 {
    (*s).parse_var(parse_flags, tok, export_flag)
        .map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_expect_semi() instead")]
pub unsafe fn js_parse_expect_semi(s: *mut JSParseState) -> i32 {
    (*s).parse_expect_semi().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_expect() instead")]
pub unsafe fn js_parse_expect(s: *mut JSParseState, tok: i32) -> i32 {
    (*s).parse_expect(tok).map_or(-1, |_| 0)
}

/* return true if scope == parent_scope or if scope is a child of parent_scope */
pub unsafe fn is_child_scope(fd: *const JSFunctionDef, scope: i32, parent_scope: i32) -> bool {
    (*fd).is_child_scope(scope, parent_scope)
}

/// return the position of the next opcode
#[deprecated(note = "use JSFUnctionDef::resolve_scope_var() instead")]
pub unsafe fn resolve_scope_var(
    ctx: &JSContext,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
    scope_level: i32,
    op: u8,
    bc: &[u8],
    bc_buf: *mut u8, // JSFunctionDef::byte_code buffer ptr
    label: Option<&LabelSlot>,
    pos_next: usize,
) -> usize {
    unreachable!("use JSFUnctionDef::resolve_scope_var() instead");
}

/// Resolves variable references in the function definition
/// Converts global variable accesses to local/closure variables when needed
/// This is the second pass of bytecode generation
/// Handles scope resolution and variable binding
#[deprecated(note = "use JSFunctionDef::resolve_variables() instead")]
pub unsafe fn resolve_variables(ctx: &JSContext, s: *mut JSFunctionDef) -> Result<(), i32> {
    (*s).resolve_variables(ctx)
}

#[deprecated(note = "use JSParseState::parse_function_decl() instead")]
pub unsafe fn js_parse_function_decl(
    s: *mut JSParseState,
    func_type: JSParseFunctionEnum,
    func_kind: JSFunctionKindEnum,
    func_name: JSAtom,
    ptr: *const u8,
    function_line_num: i32,
) -> i32 {
    unreachable!("use JSParseState::parse_function_decl() instead")
}

/// func_name must be JS_ATOM_NULL for JS_PARSE_FUNC_STATEMENT and
/// JS_PARSE_FUNC_EXPR, JS_PARSE_FUNC_ARROW and JS_PARSE_FUNC_VAR.
/// Returns -1 for ERROR, 0 for OK
#[deprecated(note = "use JSParseState::parse_function_decl2() instead")]
pub unsafe fn js_parse_function_decl2(
    s: *mut JSParseState,
    func_type: JSParseFunctionEnum,
    func_kind: JSFunctionKindEnum,
    func_name: JSAtom,
    ptr: *const u8,
    function_line_num: i32,
    export_flag: JSParseExportEnum,
    pfd: *mut *mut JSFunctionDef,
) -> i32 {
    unreachable!("use JSParseState::parse_function_decl2() instead")
}

/// the pc2line table gives a line number for each PC value
#[deprecated(note = "use JSFunctionDef::add_pc2line_info() instead")]
pub unsafe fn add_pc2line_info(s: *mut JSFunctionDef, pc: u32, line_num: i32) {
    (*s).add_pc2line_info(pc, line_num);
}

#[deprecated(note = "use JSContext::create_array() instead")]
pub unsafe fn JS_NewArray(ctx: &JSContext) -> JSValue {
    ctx.create_array().unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSContext::create_object_from_shape() instead")]
pub unsafe fn JS_NewObjectFromShape(
    ctx: &JSContext,
    sh: *mut JSShape,
    class_id: JSClassID,
) -> JSValue {
    unreachable!("unused");
}

/* create a new empty shape with prototype 'proto' */
#[deprecated(note = "use JSShape::with_capacity instead")]
pub unsafe fn js_new_shape2(
    ctx: &JSContext,
    proto: *mut JSValue,
    hash_size: usize,
    prop_size: i32,
) -> *mut JSShape {
    unreachable!("unused");
    // JSShape::with_capacity(ctx, proto.as_mut(), hash_size, prop_size)
    //     .map_or(std::ptr::null_mut(), |p| p.as_ptr())
}

#[deprecated(note = "use JSShape::new() instead")]
pub unsafe fn js_new_shape(ctx: &JSContext, proto: *mut JSValue) -> *mut JSShape {
    unreachable!("unused");
}

#[deprecated(note = "use JSShape::dup() instead")]
pub unsafe fn js_dup_shape(sh: *mut JSShape) -> *mut JSShape {
    unreachable!("unused");
}

/* The shape is cloned. The new shape is not inserted in the shape hash table */
#[deprecated(note = "use JSShape::clone() instead")]
pub unsafe fn js_clone_shape(ctx: &JSContext, sh1: *mut JSShape) -> Result<*mut JSShape, i32> {
    unreachable!("unused");
}

#[deprecated(note = "use JSShape::dispose() instead")]
pub unsafe fn js_free_shape0(rt: *mut JSRuntime, sh: *mut JSShape) {
    unreachable!("unused");
    // debug_assert_eq!((*sh).header.ref_count, 0);
    // Box::from_raw(sh).dispose(rt.as_mut().unwrap());
}

#[deprecated(note = "use JSShape::free() instead")]
pub unsafe fn js_free_shape(rt: *mut JSRuntime, sh: *mut JSShape) {
    unreachable!("unused");
}

#[deprecated(note = "use is_null() check plus JSShape::free()")]
pub unsafe fn js_free_shape_null(rt: *mut JSRuntime, sh: *mut JSShape) {
    unreachable!("unused");
}

#[deprecated(note = "use JSObject::free() instead")]
pub unsafe fn free_object(rt: *mut JSRuntime, p: NonNull<JSValue>) {
    unreachable!("unused");
    // JSObject::dispose(rt.as_mut().unwrap(), p);
}

#[deprecated(note = "not used")]
pub unsafe fn init_shape_hash(rt: &mut JSRuntime) -> i32 {
    unreachable!();
}
#[deprecated(note = "not used")]
pub unsafe fn resize_shape_hash(rt: &mut JSRuntime, new_shape_hash_bits: i32) -> i32 {
    unreachable!("not used");
}

#[deprecated(note = "use JSRuntime::register_shape() instead")]
pub fn js_shape_hash_link(rt: &mut JSRuntime, sh: JSShape) {
    unreachable!("unused")
}

#[deprecated(note = "use JSRuntime::unregister_shape() instead")]
pub fn js_shape_hash_unlink(rt: &mut JSRuntime, sh: JSShape) {
    unreachable!("unused");
}

/// find a hashed empty shape matching the prototype. Return None if not found
#[deprecated(note = "use JSRuntime::find_shape_by_proto() instead")]
pub unsafe fn find_hashed_shape_proto(rt: &mut JSRuntime, proto: *mut JSValue) -> Option<JSShape> {
    unreachable!("unused");
}

/// find a hashed shape matching sh + (prop, prop_flags). Return None if not found
#[deprecated(note = "use JSRuntime::find_shape_with_prop() instead")]
pub unsafe fn find_hashed_shape_prop(
    rt: &mut JSRuntime,
    sh: *mut JSShape,
    atom: JSAtom,
    prop_flags: u32,
) -> Option<JSShape> {
    unreachable!("unused");
}

#[deprecated(note = "use JSRuntime::gc() instead")]
pub fn JS_RunGC(rt: &mut JSRuntime) {
    rt.gc();
}

#[deprecated(note = "use JSMapState::hook_finalizer() instead")]
pub unsafe fn js_map_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unreachable!("unused");
}

#[deprecated(note = "use JSMapState::hook_mark() instead")]
pub unsafe fn js_map_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    unreachable!("unused");
    // JSMapState::hook_mark(rt, val, mark_func);
}

// for debug only: dump an object without side effect
#[deprecated(note = "use JSObject::dump() instead")]
pub unsafe fn JS_DumpObject(rt: &JSRuntime, p: *const JSValue) {
    unreachable!("unused");
}

/// Adds a new GC object to the garbage collector's tracking list
/// Initializes the object's mark and type, then adds it to the GC object list
#[deprecated]
pub unsafe fn add_gc_object(rt: *mut JSRuntime, h: *mut GCObjectHeader, tp: JSGCObjectTypeEnum) {
    unreachable!("unused");
}

/// Function type for marking objects during garbage collection
/// Parameters:
/// - rt: Pointer to the JavaScript runtime
/// - gp: Pointer to the garbage collected object header
#[deprecated]
pub type JS_MarkFunc = unsafe fn(rt: *mut crate::runtime::JSRuntime, gp: *mut GCObjectHeader);

/// Marks a JavaScript value during garbage collection
/// Only marks values that have reference counts (objects and function bytecode)
#[deprecated]
pub unsafe fn JS_MarkValue(
    rt: *mut crate::runtime::JSRuntime,
    val: crate::value::JSValue,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

/// Marks the realm associated with an auto-initialized property
/// This ensures the realm is preserved during garbage collection
#[deprecated]
pub unsafe fn js_autoinit_mark(rt: *mut JSRuntime, pr: *mut JSProperty, mark_func: JS_MarkFunc) {
    unreachable!("unused");
}

/// Recursively marks all child objects of a GC object
/// This ensures all referenced objects are preserved during garbage collection
#[deprecated]
pub(super) unsafe fn mark_children(
    rt: *mut crate::runtime::JSRuntime,
    gp: *mut GCObjectHeader,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

#[deprecated]
pub unsafe fn JS_MarkContext(
    rt: &mut crate::runtime::JSRuntime,
    ctx: &mut crate::context::JSContext,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

#[deprecated]
pub unsafe fn js_mark_module_def(rt: *mut JSRuntime, m: &mut JSModuleDef, mark_func: JS_MarkFunc) {
    unreachable!("unused");
}

/// Preserves objects with positive reference counts and their child objects.
/// Identifies objects with zero reference count in the GC object list
/// and moves them to a temporary list for removal.
#[deprecated]
pub unsafe fn gc_scan(rt: &mut JSRuntime) {
    unreachable!("unused");
}

/// Frees objects in the temporary list, handling cyclic references
#[deprecated]
pub unsafe fn gc_free_cycles(rt: &mut JSRuntime) {
    unreachable!("unused");
}

/// Frees a GC object based on its type
/// Handles different types of GC objects with their specific cleanup logic
#[deprecated(note = "use gc_free() instead")]
pub unsafe fn free_gc_object(rt: *mut JSRuntime, gp: *mut GCObjectHeader) {
    unreachable!("unused");
}

/// Removes an object from either the main GC object list or temporary list
#[deprecated(note = "use set status to Zeroed instead")]
pub unsafe fn remove_gc_object(h: *mut GCObjectHeader) {
    unreachable!("unused");
}

/// Frees objects with zero reference count (in gc_zero_ref_count_list chain).
#[deprecated(note = "use JSRuntime::gc_free_zeroed() instead")]
pub unsafe fn free_zero_refcount(rt: *mut JSRuntime) {
    (*rt).gc_free_zeroed();
}

/// Decrements the reference count of a child GC object
/// Moves the object to the temporary list if its refcount reaches zero
#[deprecated]
pub unsafe fn gc_decref_child(rt: *mut JSRuntime, p: *mut GCObjectHeader) {
    unreachable!("unsed");
}

/// Decreases reference counts for all child objects in the GC object list.
/// Moves objects with zero reference count to a temporary list for cleanup.
#[deprecated(note = "use JSRuntime::gc_deref() instead")]
pub unsafe fn gc_decref(rt: &mut JSRuntime) {
    unreachable!("unsed");
}

#[deprecated(note = "use GCObject::gc_dump() instead")]
pub unsafe fn JS_DumpGCObject(rt: *mut JSRuntime, p: *mut GCObjectHeader) {
    unreachable!("unused");
}

#[deprecated(note = "use JSRuntime::gc_dump_objects() instead")]
pub unsafe fn JS_DumpGCObjects(rt: *mut JSRuntime) {
    unreachable!("unused");
}

#[deprecated(note = "use JSAsyncFunctionState::finalize() instead")]
pub unsafe fn __async_func_free(rt: *mut JSRuntime, s: *mut JSAsyncFunctionState) {
    unreachable!("unused");
}

#[deprecated(note = "use JSAsyncFunctionState::free() instead")]
pub unsafe fn async_func_free(rt: *mut JSRuntime, s: *mut JSAsyncFunctionState) {
    unreachable!("unused");
}

/// make new JSAsyncFunctionState (used by generator and async functions)
#[deprecated(note = "use JSAsyncFunctionState::new() instead")]
pub unsafe fn async_func_init(
    ctx: &JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
) -> *mut JSAsyncFunctionState {
    unreachable!("use JSAsyncFunctionState::new() instead");
    // JSAsyncFunctionState::new(
    //     ctx,
    //     func_obj,
    //     this_obj,
    //     if argc == 0 {
    //         &[]
    //     } else {
    //         std::slice::from_raw_parts(argv, argc as usize)
    //     },
    // )
    // .map_or(std::ptr::null_mut(), |p| p.as_ptr())
}

/// 'input' must be zero terminated i.e. input[input_len] = '\0'.
#[deprecated(note = "use JSContext::eval_internal() instead")]
pub fn __JS_EvalInternal(
    ctx: &JSContext,
    this_obj: JSValueConst,
    input: &CStr,
    filename: &str,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    unsafe { ctx.eval_internal(this_obj, input, filename, flags, scope_idx) }
}

#[deprecated(note = "use JSValue::is_error() instead")]
pub fn JS_IsError(val: &JSValueConst) -> bool {
    if let JSValue::Object(o) = val {
        o.class_id() == JS_CLASS_ERROR
    } else {
        false
    }
}

/// Note: `val` is consumed
#[deprecated(note = "use JSValue::set_property_internal() instead")]
pub unsafe fn JS_SetPropertyInternal(
    ctx: &JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
    this_obj: JSValueConst,
    flags: u32,
) -> i32 {
    unreachable!("unused")
}

/// returns 0,1,-1
/// Note: `val` is consumed
#[deprecated(note = "use JSValue::set_property_value() instead")]
pub unsafe fn JS_SetPropertyValue(
    ctx: &JSContext,
    this_obj: JSValue,
    prop: JSValue,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .set_property_value(ctx, &prop, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property() instead")]
#[inline(always)]
pub unsafe fn JS_SetProperty(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property(ctx, prop, val)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_u32() instead")]
pub unsafe fn JS_SetPropertyUint32(
    ctx: &JSContext,
    this_obj: JSValueConst,
    idx: u32,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property_u32(ctx, idx, val)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_i64() instead")]
pub unsafe fn JS_SetPropertyInt64(
    ctx: &JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property_i64(ctx, idx, val)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_str() instead")]
pub unsafe fn JS_SetPropertyStr(
    ctx: &JSContext,
    this_obj: JSValueConst,
    prop: &CStr,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property_str(ctx, prop.to_string_lossy().as_ref(), val)
        .map_or(-1, |b| b as i32)

    // let atom = JS_NewAtom(ctx, prop);
    // let ret = JS_SetPropertyInternal(ctx, this_obj, atom, val, this_obj, JS_PROP_THROW);
    // JS_FreeAtom(ctx, atom);
    // ret
}

// 'obj' must be an allocated typed array object
#[deprecated(note = "use TypedArrayObj::init() instead")]
pub unsafe fn typed_array_init(
    ctx: &JSContext,
    obj: JSValueConst,
    buffer: JSValue,
    offset: u64,
    count: u64,
) -> i32 {
    unreachable!("unused");
    // UncheckedInto::<TypedArrayValue>::unchecked_into(obj)
    //     .init(ctx, buffer, offset as _, count as _)
    //     .map_or(-1, |_| 0)
}

#[deprecated(note = "use JSValue::typed_array() instead")]
pub unsafe fn get_typed_array(
    ctx: &JSContext,
    this_val: JSValueConst,
    is_dataview: bool,
) -> *mut JSValue {
    unreachable!("unused");
    // this_val
    //     .typed_array_must(ctx, is_dataview)
    //     .map_or(std::ptr::null_mut(), |p| {
    //         p.0.object_unchecked() as *mut JSObject
    //     })
}

/* WARNING: 'p' must be a typed array. Works even if the array buffer
is detached */
#[deprecated(note = "use TypedArrayValue::capacity() instead")]
pub unsafe fn typed_array_get_length(ctx: &JSContext, p: *mut JSValue) -> usize {
    unreachable!("use TypedArrayValue::capacity() instead")
}

/// returns Err if val is an exception.
/// `val` is consumed
#[deprecated(note = "use JSValue::coerce_to_bool() instead")]
pub unsafe fn JS_ToBoolFree(ctx: &JSContext, val: JSValue) -> Result<bool, i32> {
    val.coerce_to_bool(ctx)
}
#[deprecated(note = "use JSValue::coerce_to_bool() instead")]
pub unsafe fn JS_ToBool(ctx: &JSContext, val: &JSValueConst) -> Result<bool, i32> {
    val.coerce_to_bool(ctx)
}

#[deprecated(note = "use JSValue::get_own_property_names2() instead")]
pub unsafe fn JS_GetOwnPropertyNames2(
    ctx: &JSContext,
    target: JSValueConst,
    flags: i32,
    kind: JSIteratorKindEnum,
) -> JSValue {
    target
        .get_own_property_names2(ctx, flags, kind)
        .unwrap_or(JSValue::EXCEPTION)
}

// #[deprecated]
// pub unsafe fn js_free_prop_enum(ctx: &JSContext, tab: *mut PropertyEnum, len: usize) {
//     if !tab.is_null() {
//         for i in 0..len {
//             JS_FreeAtom(ctx, (*tab.add(i)).atom);
//         }
//         js_free(ctx, tab.cast());
//     }
// }

// ptab and its atoms must be freed by the user.
#[deprecated(note = "use JSObject::get_own_property_names_internal instead")]
pub unsafe fn JS_GetOwnPropertyNamesInternal(
    ctx: &JSContext,
    ptab: *mut *mut PropertyEnum,
    plen: *mut u32,
    p: *mut JSValue,
    flags: i32,
) -> Result<(), i32> {
    unreachable!("unused");

    // // clear pointer for consistency in case of failure
    // *ptab = std::ptr::null_mut();
    // *plen = 0;

    // let res = (*p).get_own_property_names_internal(ctx, flags)?;
    // let mut lst = res;
    // *plen = lst.len() as u32;
    // *ptab = lst.as_mut_ptr();
    // Box::leak(lst);
    // Ok(())
}

#[deprecated(note = "use JSContext::create_object_with_proto_and_classid() instead")]
pub unsafe fn JS_NewObjectProtoClass(
    ctx: &JSContext,
    proto_val: JSValueConst,
    class_id: JSClassID,
) -> JSValue {
    unreachable!("unused")
}

/// Instantiate object of specific class
///
///  Args:
/// - ctx: Runtime context
/// - class_id: JS_CLASS_* identifier
///
/// Returns New object or Exception
#[deprecated(note = "use JSContext::create_object_with_classid() instead")]
pub unsafe fn JS_NewObjectClass(ctx: &JSContext, class_id: JSClassID) -> JSValue {
    unreachable!("unused");

    // ctx
    //     .create_object_with_classid(class_id)
    //     .unwrap_or(JSValue::EXCEPTION)
}

/// Create plain object with specified prototype
/// Parameters:
/// - ctx: JavaScript context
/// - proto: Prototype object (can be null)
#[deprecated(note = "use JSContext::create_base_object_with_proto() instead")]
pub unsafe fn JS_NewObjectProto(ctx: &JSContext, proto: JSValueConst) -> JSValue {
    JS_NewObjectProtoClass(ctx, proto, JS_CLASS_OBJECT)
}

/// Create basic object using default Object prototype
#[deprecated(note = "use JSContext::create_base_object() instead")]
pub unsafe fn JS_NewObject(ctx: &JSContext) -> JSValue {
    ctx.create_base_object().unwrap_or(JSValue::EXCEPTION)
    // JS_NewObjectProtoClass(
    //     ctx,
    //     ctx.class_proto[JS_CLASS_OBJECT as usize],
    //     JS_CLASS_OBJECT,
    // )
}

/// Convert JS value to object equivalent
/// Handles primitive wrapping according to ES6 ToObject conversion rules
/// Returns exception for null/undefined inputs.
/// `val` is consumed
#[deprecated(note = "use JSValue::coerce_to_object() instead")]
pub fn JS_ToObject(ctx: &JSContext, val: JSValue) -> JSValue {
    val.coerce_to_object(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_to_object() instead")]
pub unsafe fn JS_ToObjectFree(ctx: &JSContext, val: JSValue) -> JSValue {
    val.coerce_to_object(ctx).unwrap_or(JSValue::EXCEPTION)
}

/// Return an Object, JS_NULL or JS_EXCEPTION in case of Proxy object.
/// `obj` keep ref count unchanged after calling.
#[deprecated(note = "use JSValue::prototype() instead")]
pub fn JS_GetPrototype(ctx: &JSContext, obj: &JSValueConst) -> JSValue {
    obj.prototype(ctx).unwrap_or(JSValue::EXCEPTION)
}

/// `obj` is consumed
#[deprecated(note = "use JSValue::prototype() instead")]
pub fn JS_GetPrototypeFree(ctx: &JSContext, obj: JSValue) -> JSValue {
    JS_GetPrototype(ctx, &obj)
}

// Private fields can be added even on non extensible objects or Proxies
#[deprecated(note = "use JSValue::define_private_field() instead")]
pub unsafe fn JS_DefinePrivateField(
    ctx: &JSContext,
    obj: &JSValueConst,
    name: JSValueConst,
    val: JSValue,
) -> i32 {
    obj.define_private_field(ctx, &name, val).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSValue::get_private_field() instead")]
pub unsafe fn JS_GetPrivateField(
    ctx: &JSContext,
    obj: JSValueConst,
    name: JSValueConst,
) -> JSValue {
    obj.get_private_field(ctx, &name)
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::set_private_field() instead")]
pub unsafe fn JS_SetPrivateField(
    ctx: &JSContext,
    obj: &JSValue,
    name: &JSValue,
    val: JSValue,
) -> i32 {
    obj.set_private_field(ctx, name, val).map_or(-1, |_| 0)
}

// XXX: remove
#[deprecated(note = "use JSFunctionDef::has_with_scope() instead")]
pub unsafe fn has_with_scope(s: *mut JSFunctionDef, mut scope_level: i32) -> bool {
    unreachable!("unused");
}

#[deprecated(note = "use JSParseState::define_var_for_func() instead")]
pub unsafe fn define_var(
    s: *mut JSParseState,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    var_def_type: JSVarDefEnum,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSParseState::define_var() instead")]
pub unsafe fn js_define_var(s: *mut JSParseState, name: JSAtom, tok: i32) -> i32 {
    if (*s).define_var(name, tok).is_err() {
        -1
    } else {
        0
    }
}

#[deprecated(note = "use JSParseState::peek_token() instead")]
pub fn peek_token(s: *const JSParseState, no_line_terminator: bool) -> i32 {
    unsafe { (*s).peek_token(no_line_terminator) }
}
