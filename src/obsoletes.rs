#![allow(clippy::all)]

use std::{
    ffi::{CStr, CString, c_char, c_void},
    ptr::NonNull,
};

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ERROR, JS_CLASS_OBJECT, JS_CLASS_PROXY,
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, JSAtomInfo, JSAtomStruct, JSAtomTypeHint},
    bytecode::arithmatic::JSStrictEqModeEnum,
    class::{JSClassDef, JSClassID},
    context::JSContext,
    error::{JS_ThrowRangeError, JSResult},
    function::{
        Bytecode, JSCFunction, JSCFunctionMagic, JSFunctionDef, JSFunctionKindEnum, JSGlobalVar,
        JSVarDefEnum, JSVarKindEnum, JSVarRef, LabelSlot, NativeCallback, NativeField,
        NativeFunctionType, ObjAsyncState, ObjNativeFunc, RelocEntry, js_function_set_properties,
    },
    gc::{GCObjectHeader, JSGCObjectTypeEnum},
    generator::ObjAsyncGenerator,
    iterator::JSIteratorKindEnum,
    module::ModuleDef,
    number::{JSToNumberHintEnum, MAX_SAFE_INTEGER},
    object::{
        JSProperty, JSPropertyDescriptor, JSShape, JSShapeProperty, ObjectDetail, PropertyEnum,
    },
    parse::{JSParseExportEnum, JSParseFunctionEnum, JSParseState},
    promise::{JSPromiseReactionData, JSPromiseStateEnum, JobCallback, Promise},
    proxy::js_proxy_isArray,
    runtime::JSRuntime,
    stack_frame::StackFrame,
    string::{JSString, JSStringInner},
    utils::UncheckedInto,
    value::{JSValue, JSValueConst},
};

#[deprecated]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSAutoInitIDEnum {
    JS_AUTOINIT_ID_PROTOTYPE,
    JS_AUTOINIT_ID_MODULE_NS,
    JS_AUTOINIT_ID_PROP,
}

/// set new value and free the old value.
/// (freeing the value can reallocate the object data)
#[deprecated(note = "use JSValue::replace_free() instead")]
#[inline(always)]
pub unsafe fn set_value(ctx: &JSContext, pval: *mut JSValue, new_val: JSValue) {
    unreachable!("unused")
}

#[deprecated(note = "use JSContext::create_string() instead")]
pub unsafe fn JS_NewStringLen(ctx: &JSContext, buf: *const u8, buf_len: usize) -> JSValue {
    ctx.create_string(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
        buf, buf_len,
    )))
}

#[deprecated(note = "use JSContext::create_string_utf8() instead")]
pub unsafe fn JS_NewString(ctx: &JSContext, str: &CStr) -> JSValue {
    ctx.create_string(str.to_string_lossy().as_ref())
}

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
    func: NativeCallback,
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
    val.coerce_to_string(ctx).unwrap_or(JSValue::EXCEPTION)
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
    sf: *mut StackFrame,
    var_idx: u16,
    is_arg: bool,
) -> *mut JSVarRef {
    unreachable!("use JSStackFrame::get_var_ref() instead")
}

#[deprecated(note = "use ObjAsyncGenerator::dispose() instead")]
pub unsafe fn js_async_generator_free(rt: *mut JSRuntime, s: *mut ObjAsyncGenerator) {
    unreachable!("use ObjAsyncGenerator::dispose() instead");
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
#[deprecated(note = "use Promise::create_resolving_functions() instead")]
pub unsafe fn js_create_resolving_functions(
    ctx: &JSContext,
    promise: JSValueConst,
) -> Result<[JSValue; 2], i32> {
    unreachable!("use Promise::create_resolving_functions() instead");
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

#[deprecated(note = "use Promise::state() instead")]
pub unsafe fn JS_PromiseState(
    ctx: &JSContext,
    promise: JSValue,
) -> Result<JSPromiseStateEnum, i32> {
    UncheckedInto::<Promise>::unchecked_into(promise).state()
}

/// to free `fd` and all its children
#[deprecated(note = "use JSFunctionDef::dispose() instead")]
pub unsafe fn js_free_function_def(ctx: &JSContext, fd: *mut JSFunctionDef) {
    unreachable!("unused");
}

/// create a function object from a function definition.
/// All the child functions are also created. It must be done this way to resolve all the variables.
/// The function definition is freed.
#[deprecated(note = "use JSFunctionDef::compile() instead")]
pub unsafe fn js_create_function(ctx: &JSContext, fd: *mut JSFunctionDef) -> JSValue {
    unreachable!("unused");
    // Box::from_raw(fd).make_function(ctx)
}

#[deprecated(note = "use Bytecode::find_line_num() instead")]
pub unsafe fn find_line_num(ctx: &JSContext, b: *mut Bytecode, pc_value: u32) -> i32 {
    (*b).find_line_num(pc_value).unwrap_or(-1)
}

/// Note: `value` is duplicated inside.
#[deprecated(note = "use Promise::fulfill_or_reject() instead")]
pub unsafe fn fulfill_or_reject_promise(
    ctx: &JSContext,
    promise: Promise,
    value: JSValueConst,
    is_reject: bool,
) {
    unreachable!("use Promise::fulfill_or_reject() instead");
}

#[deprecated(note = "use Promise::reject() instead")]
pub unsafe fn reject_promise(ctx: &JSContext, promise: Promise, value: JSValueConst) {
    unreachable!("use Promise::reject() instead");
}

#[deprecated(note = "use Promise::then() instead")]
pub unsafe fn perform_promise_then(
    ctx: &JSContext,
    promise: Promise,
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
#[deprecated(note = "use ModuleDef::eval() instead")]
pub unsafe fn js_evaluate_module(ctx: &JSContext, m: *mut ModuleDef) -> JSValue {
    (*m).eval(ctx)
}

#[deprecated(note = "use Promise::result() instead")]
pub unsafe fn JS_PromiseResult(ctx: &JSContext, promise: JSValue) -> JSValue {
    UncheckedInto::<Promise>::unchecked_into(promise).result()
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
    unreachable!("unused");
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
#[deprecated(note = "use JSValue::eval_script() instead")]
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
        Ok(str) => ctx.eval_script(
            this_obj,
            CString::new(str.as_bytes()).unwrap().as_c_str(),
            "<input>",
            flags,
            scope_idx,
        ),
        Err(_) => JSValue::EXCEPTION,
    }
}

#[deprecated(note = "use ModuleDef::build_ns() instead")]
pub unsafe fn js_build_module_ns(ctx: &JSContext, m: *mut ModuleDef) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use ModuleDef::ns() instead")]
pub unsafe fn js_get_module_ns(ctx: &JSContext, m: *mut ModuleDef) -> JSValue {
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
pub unsafe fn js_mark_module_def(rt: *mut JSRuntime, m: &mut ModuleDef, mark_func: JS_MarkFunc) {
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
    unreachable!("unused");
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

#[deprecated(note = "use ObjAsyncState::finalize() instead")]
pub unsafe fn __async_func_free(rt: *mut JSRuntime, s: *mut ObjAsyncState) {
    unreachable!("unused");
}

#[deprecated(note = "use ObjAsyncState::free() instead")]
pub unsafe fn async_func_free(rt: *mut JSRuntime, s: *mut ObjAsyncState) {
    unreachable!("unused");
}

/// make new ObjAsyncState (used by generator and async functions)
#[deprecated(note = "use ObjAsyncState::new() instead")]
pub unsafe fn async_func_init(
    ctx: &JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
) -> *mut ObjAsyncState {
    unreachable!("use ObjAsyncState::new() instead");
    // ObjAsyncState::new(
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
    unreachable!("unused");
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
    // UncheckedInto::<TypedArray>::unchecked_into(obj)
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
#[deprecated(note = "use TypedArray::capacity() instead")]
pub unsafe fn typed_array_get_length(ctx: &JSContext, p: *mut JSValue) -> usize {
    unreachable!("use TypedArray::capacity() instead")
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

/// always returns -1
#[deprecated(note = "use JSParseState::throw_reserved_identifier_error() instead")]
pub unsafe fn js_parse_error_reserved_identifier(s: *mut JSParseState) -> i32 {
    (*s).throw_syntax_error(&format!(
        "'{}' is a reserved identifier",
        (*s).token.data.ident_atom().unwrap().to_str(&(*s).ctx.rt()),
    ));
    -1
}

#[deprecated(note = "use JSParseState::parse_delete() instead")]
pub unsafe fn js_parse_delete(s: *mut JSParseState) -> i32 {
    (*s).parse_delete().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_assign_expr() instead")]
pub unsafe fn js_parse_assign_expr(s: *mut JSParseState) -> i32 {
    unreachable!("unused");
    // (*s).parse_assign_expr2(PF_IN_ACCEPTED).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::get_lvalue() instead")]
pub unsafe fn get_lvalue(
    s: *mut JSParseState,
    popcode: *mut i32,
    pscope: *mut i32,
    pname: *mut JSAtom,
    plabel: *mut i32,
    pdepth: *mut i32,
    keep: bool,
    tok: i32,
) -> i32 {
    (*s).get_lvalue(popcode, pscope, pname, plabel, pdepth, keep, tok)
        .map_or(-1, |_| 0)
}

// name has a live reference. 'is_let' is only used with opcode =
// op_scope_get_var which is never generated by get_lvalue().
#[deprecated(note = "use JSParseState::get_lvalue() instead")]
pub unsafe fn put_lvalue(
    s: *mut JSParseState,
    opcode: i32,
    scope: i32,
    name: JSAtom,
    label: i32,
    special: i32, //PutLValueEnum,
    is_let: bool,
) {
    unreachable!("unused");
    // (*s).put_lvalue(opcode, scope, name, label, special, is_let);
}

/// cond? a : b
/// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_cond_expr() instead")]
pub unsafe fn js_parse_cond_expr(s: *mut JSParseState, parse_flags: i32) -> i32 {
    (*s).parse_cond_expr(parse_flags).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_coalesce_expr() instead")]
pub unsafe fn js_parse_coalesce_expr(s: *mut JSParseState, parse_flags: i32) -> i32 {
    (*s).parse_coalesce_expr(parse_flags).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_logical_and_or() instead")]
pub unsafe fn js_parse_logical_and_or(s: *mut JSParseState, op: i32, parse_flags: i32) -> i32 {
    (*s).parse_logical_and_or(op, parse_flags).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_expr_binary() instead")]
pub unsafe fn js_parse_expr_binary(s: *mut JSParseState, level: i32, parse_flags: i32) -> i32 {
    unreachable!("unused");
    // (*s).parse_expr_binary(level, parse_flags).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_ARROW_FUNC, PF_POW_ALLOWED, PF_POW_FORBIDDEN
#[deprecated(note = "use JSParseState::parse_unary() instead")]
pub unsafe fn js_parse_unary(s: *mut JSParseState, mut parse_flags: i32) -> i32 {
    unreachable!("unused");
    // (*s).parse_unary(parse_flags).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_POSTFIX_CALL, PF_ARROW_FUNC
#[deprecated(note = "use JSParseState::parse_postfix_expr() instead")]
pub unsafe fn js_parse_postfix_expr(s: *mut JSParseState, parse_flags: i32) -> i32 {
    (*s).parse_postfix_expr(parse_flags).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::optional_chain_test() instead")]
pub unsafe fn optional_chain_test(
    s: *mut JSParseState,
    poptional_chaining_label: &mut i32,
    drop_count: i32,
) {
    (*s).optional_chain_test(poptional_chaining_label, drop_count);
}

// allowed parse_flags: PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_assign_expr2() instead")]
pub unsafe fn js_parse_assign_expr2(s: *mut JSParseState, parse_flags: i32) -> i32 {
    unreachable!("unused");
    // (*s).parse_assign_expr2(parse_flags).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_expr() instead")]
pub unsafe fn js_parse_expr(s: *mut JSParseState) -> i32 {
    unreachable!("unused");
    // (*s).parse_expr2(PF_IN_ACCEPTED).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_expr2() instead")]
pub unsafe fn js_parse_expr2(s: *mut JSParseState, parse_flags: i32) -> i32 {
    unreachable!("unused");
    // (*s).parse_expr2(parse_flags).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_template() instead")]
pub unsafe fn js_parse_template(s: *mut JSParseState, call: bool, argc: *mut i32) -> i32 {
    (*s).parse_template(call, argc).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_left_hand_side_expr() instead")]
pub unsafe fn js_parse_left_hand_side_expr(s: *mut JSParseState) -> i32 {
    (*s).parse_left_hand_side_expr().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_expr_parens() instead")]
pub unsafe fn js_parse_expr_paren(s: *mut JSParseState) -> i32 {
    (*s).parse_expr_parens().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_object_literal() instead")]
pub unsafe fn js_parse_object_literal(s: *mut JSParseState) -> i32 {
    (*s).parse_object_literal().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::set_object_name() instead")]
pub unsafe fn set_object_name(s: *mut JSParseState, name: JSAtom) {
    (*s).set_object_name(name);
}

#[deprecated(note = "use JSParseState::next_token() instead")]
pub unsafe fn next_token(s: *mut JSParseState) -> i32 {
    if (*s).next_token().is_err() { -1 } else { 0 }
}

// #[deprecated(note = "use JSParseState::free_token() instead")]
// pub unsafe fn free_token(s: *mut JSParseState, token: *mut JSToken) {
//     debug_assert_eq!(&mut (*s).token as *mut JSToken, token);
//     (*s).free_token();
// }

// #[deprecated(note = "use JSParseState::token_is_ident() instead")]
// pub fn token_is_ident(tok: i32) -> bool {
//     /* Accept keywords and reserved words as property names */
//     tok == TOK_IDENT || (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD).contains(&tok)
// }

#[deprecated(note = "use JSParseState::token_is_pseudo_keyword() instead")]
pub unsafe fn token_is_pseudo_keyword(s: *mut JSParseState, atom: JSAtom) -> bool {
    (*s).token_is_pseudo_keyword(atom)
    // (*s).token.val == TOK_IDENT
    //     && (*s).token.data.ident_atom().unwrap() == atom
    //     && !(*s).token.data.ident_has_escape()
}

#[deprecated(note = "use JSAtom::to_value() instead")]
pub unsafe fn __JS_AtomToValue(ctx: &JSContext, atom: JSAtom, force_string: bool) -> JSValue {
    atom.to_value(ctx, force_string)
}

#[deprecated(note = "use JSAtom::to_value() instead")]
pub unsafe fn JS_AtomToValue(ctx: &JSContext, atom: JSAtom) -> JSValue {
    __JS_AtomToValue(ctx, atom, false)
}

#[deprecated(note = "use JSContext::create_atom() instead")]
pub unsafe fn JS_NewAtomLen(ctx: &JSContext, str: *const u8, len: usize) -> JSAtom {
    ctx.create_atom(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
        str, len,
    )))
    .unwrap_or(JS_ATOM_NULL)
}

#[deprecated(note = "use JSContext::create_atom() instead")]
pub unsafe fn JS_NewAtom(ctx: &JSContext, str: &CStr) -> JSAtom {
    ctx.create_atom(str.to_string_lossy().as_ref())
        .unwrap_or(JS_ATOM_NULL)
}

#[deprecated(note = "use JSContext::create_atom_string() instead")]
pub unsafe fn JS_NewAtomString(ctx: &JSContext, str: &str) -> JSValue {
    ctx.create_atom_string(str).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSRuntime::register_atom_u32() instead")]
pub unsafe fn JS_NewAtomUInt32(ctx: &JSContext, n: u32) -> JSAtom {
    ctx.rt_mut().register_atom_u32(n)
}

#[deprecated(note = "use JSRuntime::register_atom_i64() instead")]
pub unsafe fn JS_NewAtomInt64(ctx: &JSContext, n: i64) -> JSAtom {
    ctx.rt_mut().register_atom_i64(n)
}

#[deprecated(note = "use ObjAsyncState::free_frame() instead")]
pub unsafe fn async_func_free_frame(rt: *mut JSRuntime, s: *mut ObjAsyncState) {
    unreachable!("unused");
}

/* return Err() if exception (Proxy object only) or TRUE/FALSE */
#[deprecated(note = "use JSValue::is_extensible() instead")]
pub unsafe fn JS_IsExtensible(ctx: &JSContext, obj: &JSValue) -> Result<bool, i32> {
    obj.is_extensible(ctx)
}

/* Return -1 if exception,
FALSE if the property does not exist, TRUE if it exists. If TRUE is
returned, the property descriptor 'desc' is filled present. */
#[deprecated(note = "use JSObject::get_own_property() instead")]
pub unsafe fn JS_GetOwnPropertyInternal(
    ctx: &JSContext,
    desc: *mut JSPropertyDescriptor,
    p: &JSValue,
    prop: JSAtom,
) -> i32 {
    match p.get_own_property(ctx, prop) {
        Ok(Some(d)) => {
            if !desc.is_null() {
                *desc = d;
            }
            1
        }
        Ok(None) => 0,
        Err(_) => -1,
    }
}

#[deprecated(note = "JSValue::get_own_property() instead")]
pub unsafe fn JS_GetOwnProperty(
    ctx: &JSContext,
    desc: *mut JSPropertyDescriptor,
    obj: JSValue,
    prop: JSAtom,
) -> i32 {
    match obj.get_own_property(ctx, prop) {
        Ok(Some(d)) => {
            if !desc.is_null() {
                *desc = d;
            }
            1
        }
        Ok(None) => 0,
        Err(_) => -1,
    }
}

#[deprecated(note = "use JSValue::copy_properties_from() instead")]
pub unsafe fn JS_CopyDataProperties(
    ctx: &JSContext,
    target: JSValue,
    source: JSValue,
    excluded: JSValue,
    setprop: bool,
) -> Result<(), i32> {
    target.copy_properties_from(ctx, &source, &excluded, setprop)
}

#[deprecated(note = "use JSValue::ordinary_instance_of() instead")]
pub unsafe fn JS_OrdinaryIsInstanceOf(
    ctx: &JSContext,
    val: &JSValueConst,
    obj: &JSValueConst,
) -> Result<bool, i32> {
    obj.ordinary_instance_of(ctx, val)
}

#[deprecated(note = "use JSValue::instance_of() instead")]
pub unsafe fn JS_IsInstanceOf(
    ctx: &JSContext,
    val: &JSValueConst,
    obj: &JSValueConst,
) -> Result<bool, i32> {
    obj.instance_of(ctx, val)
}

#[deprecated(note = "use JSValue::type_of() instead")]
pub fn js_operator_typeof(ctx: &JSContext, op1: &JSValueConst) -> JSAtom {
    op1.type_of()
}

/// Access an Array's internal JSValue array if available
#[deprecated(note = "use JSValue::fast_array_values_mut() instead")]
pub unsafe fn js_get_fast_array(
    ctx: &JSContext,
    mut obj: JSValueConst,
    arrpp: *mut *mut JSValue,
    countp: *mut u32,
) -> bool {
    unreachable!("unused");
    // if let Some(lst) = obj.fast_array_values_mut() {
    //     *countp = lst.len() as _;
    //     *arrpp = lst.as_mut_ptr();
    //     true
    // } else {
    //     false
    // }
}

#[deprecated(note = "use JSValue::is_fast_array() instead")]
pub unsafe fn js_is_fast_array(ctx: &JSContext, obj: JSValueConst) -> bool {
    obj.is_fast_array()
}

/// Preconditions: 'p' must be of class JS_CLASS_ARRAY, p is fast_array and p is extensible.
/// returns Err if fail, true if append successfully, false if can't append due to read-only but flags indicates don't throw exception.
/// Note: `val` is consumed.
#[deprecated(note = "use JSObjectInner::add_fast_array_element() instead")]
pub unsafe fn add_fast_array_element(
    ctx: &JSContext,
    p: *mut JSValue,
    val: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("unused");
}

// return Err if exception (proxy case) or TRUE/FALSE
#[deprecated(note = "use JSValue::is_array() instead")]
pub unsafe fn JS_IsArray(ctx: &JSContext, val: &JSValueConst) -> Result<bool, i32> {
    if let JSValue::Object(p) = val {
        if p.class_id() == JS_CLASS_PROXY {
            js_proxy_isArray(ctx, val)
        } else {
            Ok(p.class_id() == JS_CLASS_ARRAY)
        }
    } else {
        Ok(false)
    }
}

/// set the array length and remove the array elements if necessary.
/// returns 0,1,-1
#[deprecated(note = "use JSObject::set_array_length() instead")]
pub unsafe fn set_array_length(ctx: &JSContext, p: *mut JSValue, val: JSValue, flags: u32) -> i32 {
    (*p).object_unchecked()
        .set_array_length(ctx, val, flags)
        .map_or(-1, |b| b as i32)
}

/// `val` is duplicated inside
#[deprecated(note = "use JSObject::create_property() instead")]
pub unsafe fn JS_CreateProperty(
    ctx: &JSContext,
    p: *mut JSValue,
    prop: JSAtom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    (*p).object_unchecked()
        .create_property(ctx, prop, val, getter, setter, flags)
}

#[deprecated(note = "use JSValue::has_property() instead")]
pub unsafe fn JS_HasProperty(ctx: &JSContext, obj: JSValue, prop: JSAtom) -> Result<bool, i32> {
    obj.has_property(ctx, prop)
}

#[deprecated(note = "use JSProperty::free() instead")]
pub unsafe fn free_property(rt: *mut JSRuntime, pr: *mut JSProperty, prop_flags: u32) {
    unreachable!("use JSProperty::free() instead");
}

/// Finds a property in an object by its atom name
#[deprecated(note = "use JSObject::find_own_property1() instead")]
pub unsafe fn find_own_property1(p: *mut JSValue, atom: JSAtom) -> *mut JSShapeProperty {
    unreachable!("unused");
}

/// Finds a property and its value in an object by atom name
/// Sets ppr to the property value pointer if found, null otherwise
/// Returns the property shape pointer if found, null otherwise
#[inline(always)]
#[deprecated(note = "use JSObject::find_own_property() instead")]
pub unsafe fn find_own_property(
    ppr: *mut *mut JSProperty,
    p: *mut JSValue,
    atom: JSAtom,
) -> *mut JSShapeProperty {
    unreachable!("unused");
}

#[deprecated]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum JSCFunctionEnum {
    Generic = 0,
    GenericMagic,
    Constructor,
    ConstructorMagic,
    ConstructorOrFunc,
    ConstructorOrFuncMagic,
    FF,
    FFF,
    Getter,
    Setter,
    GetterMagic,
    SetterMagic,
    IteratorNext,

    NativeFunc,
    NativeConstructor,
    NativeConstructorOrFunc,
    NativeSetter,
    NativeGetter,
}

// Note: at least 'length' arguments will be readable in 'argv'
#[deprecated(note = "use native callback instead. see JSContext::create_native_function()")]
pub unsafe fn JS_NewCFunction3(
    ctx: &JSContext,
    func: JSCFunction,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
    proto_val: JSValueConst,
) -> JSValue {
    match ctx.create_instance_from_proto(
        &proto_val,
        ObjectDetail::C_FUNCTION(ObjNativeFunc {
            realm: ctx.downgrade(),
            handler: NativeFunctionType::CFunc(func),
            length,
            magic,
        }),
    ) {
        Ok(func_obj) => {
            unsafe {
                (*func_obj.object_unchecked().0.detailed.as_ptr()).set_constructor(matches!(
                    cproto,
                    JSCFunctionEnum::Constructor
                        | JSCFunctionEnum::ConstructorMagic
                        | JSCFunctionEnum::ConstructorOrFunc
                        | JSCFunctionEnum::ConstructorOrFuncMagic
                ));
            }

            // define propperties on func_obj: length and name.
            let name_atom = ctx.create_atom(name).unwrap();

            js_function_set_properties(ctx, &func_obj, name_atom, length as i32);
            JS_FreeAtom(ctx, name_atom);

            func_obj
        }
        Err(_) => JSValue::EXCEPTION,
    }
}

/* Note: at least 'length' arguments will be readable in 'argv' */
#[deprecated(note = "use native callback instead. see JSContext::create_native_function()")]
#[inline(always)]
pub fn JS_NewCFunction2(
    ctx: &JSContext,
    func: JSCFunction,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
) -> JSValue {
    unsafe {
        JS_NewCFunction3(
            ctx,
            func,
            name,
            length,
            cproto,
            magic,
            ctx.0.borrow().function_proto.clone(),
        )
    }
}

#[inline(always)]
#[deprecated(note = "use native callback instead. see JSContext::create_native_function()")]
pub fn JS_NewCFunction(ctx: &JSContext, func: JSCFunction, name: &str, length: u8) -> JSValue {
    JS_NewCFunction2(ctx, func, name, length, JSCFunctionEnum::Generic, 0)
}

#[inline(always)]
#[deprecated(note = "use native callback instead. see JSContext::create_native_function()")]
pub fn JS_NewCFunctionMagic(
    ctx: &JSContext,
    func: JSCFunctionMagic,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
) -> JSValue {
    unsafe { JS_NewCFunction2(ctx, std::mem::transmute(func), name, length, cproto, magic) }
}

/* return NULL without exception if not a function or no bytecode */
#[deprecated(note = "use ObjectDetail::func() to get Bytecode")]
pub fn JS_GetFunctionBytecode(val: &JSValueConst) -> *mut Bytecode {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::ensure_callable() instead")]
pub unsafe fn check_function(ctx: &JSContext, obj: JSValueConst) -> Result<(), i32> {
    obj.ensure_callable(ctx)?;
    Ok(())
}

/// Checks if a JSValue is a specific C function.
/// Verifies both the function pointer and magic value match.
#[deprecated(note = "use JSValue::match_native_callback() instead")]
pub unsafe fn JS_IsCFunction(val: &JSValue, func: JSCFunction, magic: i32) -> bool {
    unreachable!("unused");
}

/// flags is 0, DEFINE_GLOBAL_LEX_VAR or DEFINE_GLOBAL_FUNC_VAR
/// XXX: could support exotic global object.
#[deprecated(note = "use JSContext::define_global_var_checked() instead")]
pub fn JS_CheckDefineGlobalVar(ctx: &JSContext, prop: JSAtom, flags: u32) -> i32 {
    ctx.define_global_var_checked(prop, flags).map_or(-1, |_| 0)
}

// def_flags is (0, DEFINE_GLOBAL_LEX_VAR) |
//   JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE
// XXX: could support exotic global object.
#[deprecated(note = "use JSontext::define_global_var() instead")]
pub fn JS_DefineGlobalVar(ctx: &JSContext, prop: JSAtom, def_flags: u32) -> i32 {
    ctx.define_global_var(prop, def_flags).map_or(-1, |_| 0)
}

/// 'def_flags' is 0 or JS_PROP_CONFIGURABLE.
/// `func` is not consumed.
/// XXX: could support exotic global object.
#[deprecated(note = "use JSContext::define_global_function() instead")]
pub fn JS_DefineGlobalFunction(
    ctx: &JSContext,
    prop: JSAtom,
    func: &JSValueConst,
    def_flags: u32,
) -> i32 {
    ctx.define_global_function(prop, func, def_flags)
        .map_or(-1, |_| 0)
}

#[deprecated(note = "use JSContext::get_global_var() instead")]
pub fn JS_GetGlobalVar(ctx: &JSContext, prop: JSAtom, throw_ref_error: bool) -> JSValue {
    ctx.get_global_var(prop, throw_ref_error)
}

// construct a reference to a global variable
#[deprecated(note = "use JSContextInner::build_global_varref() instead")]
pub unsafe fn JS_GetGlobalVarRef(ctx: &JSContext, prop: JSAtom, sp: *mut JSValue) -> i32 {
    if let Ok(vs) = ctx.build_global_varref(prop) {
        if let Some((v0, v1)) = vs {
            std::ptr::replace(sp, v0);
            std::ptr::replace(sp.add(1), v1);
        }
        0
    } else {
        -1
    }
}

/// used for strict variable access: test if the variable exists
#[deprecated(note = "JSContext::check_global_var() instead")]
pub fn JS_CheckGlobalVar(ctx: &JSContext, prop: JSAtom) -> i32 {
    // no exotic behavior is possible in global_var_obj
    let p = ctx.global_var_obj();
    if p.has_shape_property(prop) {
        1
    } else {
        if let Ok(ret) = ctx.global_obj().has_property(ctx, prop) {
            ret as i32
        } else {
            -1
        }
    }
}

// flag = 0: normal variable write
// flag = 1: initialize lexical variable
// flag = 2: normal variable write, strict check was done before
#[deprecated(note = "use JSContext::set_global_var() instead")]
pub fn JS_SetGlobalVar(ctx: &JSContext, prop: JSAtom, val: JSValue, flag: u32) -> i32 {
    ctx.set_global_var(prop, val, flag).map_or(-1, |_| 0)
}

#[deprecated(note = "use ObjAsyncState::resume() instead")]
pub fn async_func_resume(ctx: &JSContext, af: &ObjAsyncState) -> JSValue {
    af.resume(ctx).unwrap_or(JSValue::EXCEPTION)
}

/// call an async function
#[deprecated(note = "use AsyncFunction::call() instead")]
pub fn js_async_function_call(
    ctx: &JSContext,
    func_obj: &JSValue,
    this_obj: &JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult {
    unreachable!("unused");
}

// Note: the integer value is saturated to 32 bits
#[deprecated(note = "use JSValue::coerce_to_i32_saturating() instead")]
pub unsafe fn JS_ToInt32SatFree(ctx: &JSContext, pres: *mut i32, mut val: JSValue) -> i32 {
    let ret: i32 = loop {
        match val {
            JSValue::NULL | JSValue::UNDEFINED => {
                break 0;
            }
            JSValue::Int(i) => {
                break i;
            }
            JSValue::Bool(b) => {
                break b as i32;
            }
            JSValue::Float64(d) => {
                break if d.is_nan() {
                    0
                } else if d < i32::MIN as f64 {
                    i32::MIN
                } else if d > i32::MAX as f64 {
                    i32::MAX
                } else {
                    d as i32
                };
            }
            JSValue::EXCEPTION => {
                *pres = 0;
                return -1;
            }
            _ => {
                val = JS_ToNumberFree(ctx, val);
                if val.is_exception() {
                    *pres = 0;
                    return -1;
                }
            }
        }
    };

    *pres = ret;
    0
}

/// val is not consumed
#[deprecated(note = "use JSValue::coerce_to_i32_saturating() instead")]
pub unsafe fn JS_ToInt32Sat(ctx: &JSContext, pres: *mut i32, val: &JSValueConst) -> i32 {
    JS_ToInt32SatFree(ctx, pres, val.clone())
}

/// val is not consumed
#[deprecated(note = "use JSValue::coerce_to_i32_clamp() instead")]
pub unsafe fn JS_ToInt32Clamp(
    ctx: &JSContext,
    pres: *mut i32,
    val: &JSValueConst,
    min: i32,
    max: i32,
    min_offset: i32,
) -> i32 {
    if let Ok(mut res) = val.coerce_to_i32_saturating(ctx) {
        if res < min {
            res += min_offset;
            if res < min {
                res = min;
            }
        } else if res > max {
            res = max;
        }
        *pres = res;
        0
    } else {
        -1
    }
}

#[deprecated(note = "use JSValue::coerce_number() instead")]
pub unsafe fn JS_ToNumberFree(ctx: &JSContext, val: JSValue) -> JSValue {
    val.coerce_to_number(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_numeric() instead")]
pub unsafe fn JS_ToNumericFree(ctx: &JSContext, val: JSValue) -> JSValue {
    val.coerce_to_numeric(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_numeric() instead")]
pub unsafe fn JS_ToNumeric(ctx: &JSContext, val: JSValueConst) -> JSValue {
    val.coerce_to_numeric(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_to_i64_saturating() instead")]
pub unsafe fn JS_ToInt64SatFree(ctx: &JSContext, pres: *mut i64, mut val: JSValue) -> i32 {
    unreachable!("unused");
}

/// val is not consumed
#[deprecated(note = "use JSValue::coerce_to_i64_saturating() instead")]
pub unsafe fn JS_ToInt64Sat(ctx: &JSContext, pres: *mut i64, val: &JSValueConst) -> i32 {
    unreachable!("unused");
}

/// val is not consumed
#[deprecated(note = "use JSValue::coerce_to_i64_clamp() instead")]
pub unsafe fn JS_ToInt64Clamp(
    ctx: &JSContext,
    pres: *mut i64,
    val: &JSValueConst,
    min: i64,
    max: i64,
    neg_offset: i64,
) -> i32 {
    unreachable!("unused");
}

/// convert a value to a length between 0 and MAX_SAFE_INTEGER.
#[deprecated(note = "use JSValue::coerce_to_length() instead")]
pub unsafe fn JS_ToLengthFree(ctx: &JSContext, val: JSValue) -> Result<i64, i32> {
    unreachable!("unused");
}

/// returns value of int, float64, nan, exception
#[deprecated(note = "use JSValue::do_coerce_number_free() instead")]
pub unsafe fn JS_ToNumberHintFree(
    ctx: &JSContext,
    val: JSValue,
    flag: JSToNumberHintEnum,
) -> JSValue {
    unreachable!("unused");
    // JSValue::do_coerce_to_number_free(ctx, val, flag).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_to_index() instead")]
pub fn JS_ToIndex(ctx: &JSContext, val: &JSValue) -> Result<u64, i32> {
    let v: i64 = val.coerce_to_i64_saturating(ctx)?;
    if !(0..=MAX_SAFE_INTEGER).contains(&v) {
        JS_ThrowRangeError(ctx, "invalid array index");
        return Err(-1);
    }
    Ok(v as u64)
}

#[deprecated(note = "use JSValue::from_i32() instead")]
pub fn JS_NewInt32(ctx: &JSContext, val: i32) -> JSValue {
    JSValue::Int(val)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_u32() instead")]
pub fn JS_NewUint32(ctx: &JSContext, val: u32) -> JSValue {
    JSValue::from(val)
}

#[deprecated(note = "use JSValue::from_i64() instead")]
#[inline(always)]
pub fn JS_NewInt64(ctx: &JSContext, val: i64) -> JSValue {
    JSValue::from(val)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_f64_as_is() instead")]
pub fn __JS_NewFloat64(ctx: &JSContext, d: f64) -> JSValue {
    JSValue::Float64(d)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_f64() instead")]
pub fn JS_NewFloat64(ctx: &JSContext, d: f64) -> JSValue {
    JSValue::from(d)
}

/* Note: can return an exception (-1) */
#[deprecated(note = "use JSValue::is_integer() instead")]
pub unsafe fn JS_NumberIsInteger(ctx: &JSContext, val: JSValueConst) -> i32 {
    val.is_integral(ctx).map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::coerce_to_i32() instead")]
pub unsafe fn JS_ToInt32Free(ctx: &JSContext, pres: *mut i32, val: JSValue) -> i32 {
    val.coerce_to_i32(ctx).map_or(-1, |v| {
        *pres = v;
        0
    })
}

/// `val` is not consumed
#[deprecated(note = "use JSValue::coerce_to_i32() instead")]
pub unsafe fn JS_ToInt32(ctx: &JSContext, val: &JSValueConst) -> Result<i32, i32> {
    let mut n = 0;
    if JS_ToInt32Free(ctx, &mut n, val.clone()) == -1 {
        Err(-1)
    } else {
        Ok(n)
    }
}

#[deprecated(note = "use JSValue::coerce_to_u32() instead")]
pub unsafe fn JS_ToUint32(ctx: &JSContext, val: &JSValue) -> Result<u32, i32> {
    val.coerce_to_u32(ctx)
}

#[inline(always)]
#[deprecated(note = "use JSValue::coerce_to_u32() instead")]
pub unsafe fn JS_ToUint32Free(ctx: &JSContext, val: JSValue) -> Result<u32, i32> {
    val.coerce_to_u32(ctx)
}

#[deprecated(note = "use JSValue::coerce_to_u8_clamp() instead")]
pub unsafe fn JS_ToUint8ClampFree(ctx: &JSContext, pres: *mut i32, val: JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::coerce_to_u8_clamp() instead")]
pub unsafe fn JS_ToUint8Clamp(ctx: &JSContext, pres: *mut i32, val: &JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::coerce_to_f64() instead")]
pub unsafe fn JS_ToFloat64Free(ctx: &JSContext, pres: *mut f64, val: JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::coerce_to_f64() instead")]
pub unsafe fn JS_ToFloat64(ctx: &JSContext, pres: *mut f64, val: &JSValueConst) -> i32 {
    JS_ToFloat64Free(ctx, pres, val.clone())
}

/// Note: `val` is consumed
#[deprecated(note = "use JSValue::coerce_to_f64() instead")]
pub unsafe fn __JS_ToFloat64Free(ctx: &JSContext, pres: *mut f64, val: JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "unused")]
pub unsafe fn free_function_bytecode(
    rt: *mut JSRuntime,
    mut b: NonNull<Bytecode>,
) -> Option<NonNull<Bytecode>> {
    unreachable!("unused")
}

/// Make function closure with bytecode and captured variables.
/// Returns closure function object.
#[deprecated(note = "use Bytecode::make_closure() instead")]
pub unsafe fn js_closure(
    ctx: &JSContext,
    bc: &Bytecode,
    cur_var_refs: &[JSVarRef],
    parent_frame: Option<&mut StackFrame>,
) -> JSValue {
    bc.make_closure(ctx, cur_var_refs, parent_frame)
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use Bytecode::make_closure_to() instead")]
pub(crate) unsafe fn js_closure2(
    ctx: &JSContext,
    func_obj: JSValue,
    bc: Bytecode,
    cur_var_refs: &[JSVarRef],
    parent_frame: Option<&mut StackFrame>,
) -> JSValue {
    bc.make_closure_to(
        ctx,
        func_obj.object_unchecked().clone(),
        cur_var_refs,
        parent_frame,
    )
    .map_or(JSValue::EXCEPTION, JSValue::Object)
}

#[deprecated(note = "use JSFunctionDef::add_closure_var() instead")]
/// returns added closure var index, or Err for error.
pub unsafe fn add_closure_var(
    ctx: &JSContext,
    s: &mut JSFunctionDef,
    is_local: bool,
    is_arg: bool,
    var_idx: u16,
    var_name: JSAtom,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    s.add_closure_var(
        ctx, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    )
}

/// `fun_obj` is consumed, `this_obj` is not consumed.
/// `fun_obj` could be Bytecode or Module.
#[deprecated(note = "unused")]
pub unsafe fn JS_EvalFunctionInternal(
    ctx: &JSContext,
    fun_obj: &JSValue,
    this_obj: &JSValue,
    var_refs: &[JSVarRef],
    parent_frame: Option<&mut StackFrame>,
) -> JSValue {
    unreachable!("unused");
    // ctx.eval_bytecode_or_module(fun_obj, this_obj, var_refs, parent_frame)
    //     .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "unused")]
pub unsafe fn JS_EvalFunction(ctx: &JSContext, fun_obj: &JSValue) -> JSValue {
    return JS_EvalFunctionInternal(ctx, fun_obj, &ctx.0.borrow().global_obj.clone(), &[], None);
}

#[deprecated(note = "use JSFunctionDef::add_func_var() instead")]
pub unsafe fn add_func_var(ctx: &JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).add_func_var(ctx, name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_var_this() instead")]
pub unsafe fn add_var_this(ctx: &JSContext, fd: *mut JSFunctionDef) -> i32 {
    (*fd).add_var_this(ctx).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_arguments(ctx, false) instead")]
pub unsafe fn add_arguments_var(ctx: &JSContext, fd: *mut JSFunctionDef) -> i32 {
    (*fd).add_arguments(ctx, false).map_or(-1, |x| x as i32)
}

// add an argument definition in the argument scope. Only needed when
// "eval()" may be called in the argument scope. Return 0 if OK.
#[deprecated(note = "use JSFunctionDef::add_arguments() instead")]
pub unsafe fn add_arguments_arg(ctx: &JSContext, fd: &mut JSFunctionDef) -> i32 {
    (*fd).add_arguments(ctx, true).map_or(-1, |x| x as i32)
}

/// find closure var by name, returning index of the closure var
#[deprecated(note = "use JSFunctionDef::find_closure_var() instead")]
pub unsafe fn find_closure_var(
    ctx: &JSContext,
    s: &mut JSFunctionDef,
    var_name: JSAtom,
) -> Option<u16> {
    (*s).find_closure_var(var_name)
}

// 'fd' must be a parent of 's'. Create in 's' a closure referencing a
// local variable (is_local = TRUE) or a closure (is_local = FALSE) in
// 'fd'
#[deprecated(note = "use JSFunctionDef::get_closure_var2() instead")]
pub unsafe fn get_closure_var2(
    ctx: &JSContext,
    s: &mut JSFunctionDef,
    fd: *const JSFunctionDef,
    is_local: bool,
    is_arg: bool,
    var_idx: u16,
    var_name: JSAtom,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    s.get_closure_var2(
        ctx, fd, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    )
}

#[deprecated(note = "use JSFunctionDef::get_closure_var2() instead")]
pub unsafe fn get_closure_var(
    ctx: &JSContext,
    s: &mut JSFunctionDef,
    fd: *const JSFunctionDef,
    is_arg: bool,
    var_idx: u16,
    var_name: JSAtom,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    s.get_closure_var2(
        ctx, fd, true, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    )
}

#[deprecated(note = "use JSParseState::parse_destructuring_element() instead")]
pub unsafe fn js_parse_destructuring_element(
    s: *mut JSParseState,
    tok: i32,
    is_arg: bool,
    hasval: bool,
    has_ellipsis: i32,
    allow_initializer: bool,
) -> i32 {
    (*s).parse_destructuring_element(tok, is_arg, hasval, has_ellipsis, allow_initializer)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSParseState::parse_destructuring_var() instead")]
pub unsafe fn js_parse_destructuring_var(s: *mut JSParseState, tok: i32, is_arg: bool) -> JSAtom {
    (*s).parse_destructuring_var(tok, is_arg)
        .unwrap_or(JS_ATOM_NULL)
}

#[deprecated(note = "use JSValue::install_native_field_entry() instead")]
pub unsafe fn JS_SetPropertyFunctionList(
    ctx: &JSContext,
    obj: &JSValueConst,
    tab: *const NativeField,
    len: usize,
) {
    unreachable!("unused");
    // obj.install_property_entries(ctx, std::slice::from_raw_parts(tab, len))
    //     .unwrap();
}

#[deprecated(note = "use JSValue::install_native_fields() instead")]
pub unsafe fn JS_InstantiateFunctionListItem(
    ctx: &JSContext,
    obj: &JSValueConst,
    atom: JSAtom,
    e: *const NativeField,
) -> i32 {
    unreachable!("unused");
}

/* Note: the property value is not initialized. Return NULL if memory
error. */
#[deprecated(note = "use JSObject::add_property() instead")]
pub unsafe fn add_property(
    ctx: &JSContext,
    p: *mut JSValue,
    prop: JSAtom,
    prop_flags: u32,
) -> *mut JSProperty {
    unreachable!("unused")
}

#[deprecated(note = "use ObjAsyncGenerator::resume_next() instead")]
pub unsafe fn js_async_generator_resume_next(ctx: &JSContext, s: *mut ObjAsyncGenerator) {
    (*s).resume_next(ctx);
}

#[deprecated(note = "use ObjAsyncGenerator::complete() instead")]
pub unsafe fn js_async_generator_complete(ctx: &JSContext, s: *mut ObjAsyncGenerator) {
    (*s).complete();
}

/// `value` is not consumed.
#[deprecated(note = "use ObjAsyncGenerator::resolve() instead")]
pub unsafe fn js_async_generator_resolve(
    ctx: &JSContext,
    s: *mut ObjAsyncGenerator,
    value: JSValueConst,
    done: bool,
) {
    (*s).resolve(ctx, value, done);
}

#[inline(always)]
#[deprecated(note = "use ObjAsyncGenerator::reject() instead")]
pub unsafe fn js_async_generator_reject(
    ctx: &JSContext,
    s: *mut ObjAsyncGenerator,
    exception: JSValueConst,
) {
    (*s).reject(ctx, exception);
}

#[deprecated(note = "use ObjAsyncGenerator::resolve_or_reject() instead")]
pub unsafe fn js_async_generator_resolve_or_reject(
    ctx: &JSContext,
    s: *mut ObjAsyncGenerator,
    result: JSValueConst,
    is_reject: bool,
) {
    unreachable!("unnused")
    // (*s).resolve_or_reject(ctx, &result, is_reject);
}

#[cfg(feature = "dump_bytecode")]
#[deprecated(note = "use Bytecode::dump() instead")]
pub fn js_dump_function_bytecode(ctx: &JSContext, bc: &Bytecode) {
    bc.dump(ctx);
}

#[deprecated(note = "use JSFunctionDef::compute_stack_size() instead")]
pub unsafe fn compute_stack_size(ctx: &JSContext, fd: *mut JSFunctionDef) -> Result<u16, i32> {
    (*fd).compute_stack_size(ctx)
}

#[deprecated(note = "use JSParseState::parse_class() instead")]
pub fn js_parse_class(
    s: &mut JSParseState,
    is_class_expr: bool,
    export_flag: JSParseExportEnum,
) -> i32 {
    s.parse_class(is_class_expr, export_flag).map_or(-1, |_| 0)
}
