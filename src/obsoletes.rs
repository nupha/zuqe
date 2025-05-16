use std::ffi::{c_char, c_void, CStr, CString};

use crate::{
    atom::{
        JSAtom, JSAtomInfo, JSAtomStruct, JSAtomTypeHint, JS_FreeAtom, JS_NewAtom, JS_ATOM_NULL,
    },
    bytecode::arithmatic::JSStrictEqModeEnum,
    class::{JSClassDef, JSClassID},
    context::JSContext,
    function::{
        JSCFunction, JSFunctionBytecode, JSFunctionDef, JSGlobalVar, JSNativeCallback,
        JSVarKindEnum, JSVarRef, LabelInfo, RelocEntry,
    },
    generator::JSAsyncGeneratorData,
    module::JSModuleDef,
    parse::JSParseState,
    promise::{JSPromiseReactionData, JSPromiseStateEnum, JobCallback, PromiseValue},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    utils::UncheckedInto,
    value::{
        object::{JSAutoInitIDEnum, JSObject},
        JSString, JSValue, JSValueConst, JS_FreeValue, JS_TAG_OBJECT, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_TAG,
    },
};

#[deprecated(note = "use JSObject::opaque() instead")]
pub unsafe fn JS_GetOpaque(obj: JSValueConst, class_id: JSClassID) -> *mut c_void {
    if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
        return std::ptr::null_mut();
    }
    let p = JS_VALUE_GET_OBJ(obj);
    if (*p).class_id() != class_id {
        return std::ptr::null_mut();
    }
    (*p).u.opaque
}

#[deprecated(note = "use JSObject::opaque_must() instead")]
pub unsafe fn JS_GetOpaque2(
    ctx: *mut JSContext,
    obj: JSValueConst,
    class_id: JSClassID,
) -> *mut c_void {
    let p = JS_GetOpaque(obj, class_id);
    if p.is_null() {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeErrorInvalidClass(class_id);
    }
    p
}

#[deprecated(note = "use JSObject::set_opaque() instead")]
pub unsafe fn JS_SetOpaque(obj: JSValue, opaque: *mut c_void) {
    if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let p = JS_VALUE_GET_OBJ(obj);
        (*p).u.opaque = opaque;
    }
}

#[inline(always)]
#[deprecated(note = "use Into::<JSValue>::into(bool) or From::<bool>::from(JSValue) instead")]
pub fn JS_NewBool(val: bool) -> JSValue {
    if val {
        JSValue::TRUE
    } else {
        JSValue::FALSE
    }
}

/// `val` keeps ref count unchanged after calling this function.
#[deprecated(note = "use JSValue::corece_to_string_value() instead")]
pub unsafe fn JS_ToStringInternal(
    ctx: *mut JSContext,
    val: JSValueConst,
    is_ToPropertyKey: bool,
) -> JSValue {
    unreachable!("use JSValue::corece_to_string_value() instead");
}

/// `val` keeps ref count unchanged after calling this function.
#[deprecated(note = "use JSValue::coerce_to_string_value() instead")]
pub unsafe fn JS_ToString(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    unreachable!("use JSValue::coerce_to_string_value() instead");
}

#[deprecated(note = "use JSValue::coerce_to_propkey_value() instead")]
pub unsafe fn JS_ToPropertyKey(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    unreachable!("use JSValue::coerce_to_propkey_value() instead");
}

/// `val` keeps ref count after calling this function.
#[deprecated(note = "use JSValue::coerce_number() instead")]
pub unsafe fn JS_ToNumber(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    val.coerce_to_number(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_to_primitive() + JS_FreeValue() instead")]
pub unsafe fn JS_ToPrimitiveFree(ctx: *mut JSContext, val: JSValue, hint: i32) -> JSValue {
    unreachable!("use JSValue::coerce_to_primitive() + JS_FreeValue() instead");
}

#[deprecated(note = "use JSValue::coerce_to_primitive() instead")]
pub unsafe fn JS_ToPrimitive(ctx: *mut JSContext, val: JSValue, hint: i32) -> JSValue {
    val.coerce_to_primitive(ctx.as_mut().unwrap(), hint)
        .unwrap_or(JSValue::EXCEPTION)

    //JS_ToPrimitiveFree(ctx, val.dup(), hint)
}

#[deprecated(note = "use JSValue::is_callable() instead")]
pub unsafe fn JS_IsFunction(ctx: *mut JSContext, val: JSValueConst) -> bool {
    val.is_callable(ctx.as_ref().unwrap())
}

#[deprecated(note = "use JSValue::is_constructor() instead")]
#[inline(always)]
pub unsafe fn JS_IsConstructor(ctx: *const JSContext, val: JSValueConst) -> bool {
    val.is_constructor()
}

#[deprecated(note = "use JSContext::add_global_constructor() instead")]
pub unsafe fn JS_NewGlobalCConstructor2(
    ctx: *mut JSContext,
    func_obj: JSValue,
    name: &CStr,
    proto: JSValueConst,
) {
    (*ctx).add_global_constructor(name.to_string_lossy().as_ref(), func_obj, proto);
}

#[deprecated(note = "use JSContext::create_global_constructor() instead")]
pub unsafe fn JS_NewGlobalCConstructor(
    ctx: *mut JSContext,
    name: &CStr,
    func: JSCFunction,
    length: i32,
    proto: JSValueConst,
) -> JSValueConst {
    (*ctx).create_global_constructor(name, func, length, proto)
}

#[deprecated(note = "use JSRuntime::is_class_registered() instead")]
pub unsafe fn JS_IsRegisteredClass(rt: *const JSRuntime, class_id: JSClassID) -> bool {
    (*rt).is_class_registered(class_id)
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
#[deprecated(note = "use JSContext::create_native_callable_object() instead")]
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
        promise.into()
    } else {
        JSValue::EXCEPTION
    }
}

#[deprecated(note = "use PromiseValue::state() instead")]
pub unsafe fn JS_PromiseState(
    ctx: *const JSContext,
    promise: JSValue,
) -> Result<JSPromiseStateEnum, i32> {
    UncheckedInto::<PromiseValue>::unchecked_into(promise).state()
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
pub unsafe fn js_evaluate_module(ctx: *mut JSContext, m: *mut JSModuleDef) -> JSValue {
    (*m).eval(ctx.as_mut().unwrap())
}

#[deprecated(note = "use PromiseValue::result() instead")]
pub unsafe fn JS_PromiseResult(ctx: *mut JSContext, promise: JSValue) -> JSValue {
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
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValueConst,
    getter: JSValueConst,
    setter: JSValueConst,
    flags: u32,
) -> i32 {
    this_obj
        .define_property(ctx.as_mut().unwrap(), prop, val, getter, setter, flags)
        .map_or(-1, |b| b as i32)
}

/// shortcut to add or redefine a new property value.
/// Note: property value is consumed.
#[deprecated(note = "use JSValue::defined_property_value() instead")]
pub unsafe fn JS_DefinePropertyValue(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value(ctx.as_mut().unwrap(), prop, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_str() instead")]
pub unsafe fn JS_DefinePropertyValueStr(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: &CStr,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_str(
            ctx.as_mut().unwrap(),
            prop.to_string_lossy().as_ref(),
            val,
            flags,
        )
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_value() instead")]
pub unsafe fn JS_DefinePropertyValueValue(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSValue,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_value(ctx.as_mut().unwrap(), prop, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::create_data_property_u32() instead")]
pub unsafe fn JS_CreateDataPropertyUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .create_data_property_u32(ctx.as_mut().unwrap(), idx, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_u32() instead")]
pub unsafe fn JS_DefinePropertyValueUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: u32,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_u32(ctx.as_mut().unwrap(), idx, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_i64() instead")]
pub unsafe fn JS_DefinePropertyValueInt64(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_i64(ctx.as_mut().unwrap(), idx, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_desc() instead")]
pub unsafe fn JS_DefinePropertyDesc(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    desc: JSValueConst,
    flags: u32,
) -> Result<bool, i32> {
    obj.define_property_desc(ctx.as_mut().unwrap(), prop, desc, flags)

    // let mut d: JSPropertyDescriptor = std::mem::zeroed();
    // js_obj_to_desc(ctx, &mut d, desc)?;

    // let ret = JS_DefineProperty(ctx, obj, prop, d.value, d.getter, d.setter, d.flags | flags);
    // js_free_desc(ctx, &mut d);

    // Ok(ret != 0)
}

/// `getter` and `setter` are consumed.
#[deprecated(note = "use JSValue::define_property_getset() instead")]
pub unsafe fn JS_DefinePropertyGetSet(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_getset(ctx.as_mut().unwrap(), prop, getter, setter, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::define_auto_init_property() instead")]
pub unsafe fn JS_DefineAutoInitProperty(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    id: JSAutoInitIDEnum,
    opaque: *mut std::ffi::c_void,
    flags: u32,
) -> Result<bool, i32> {
    this_obj.define_auto_init_property(ctx.as_mut().unwrap(), prop, id, opaque, flags)
}

/// returned value has its ref_count increased by 1.
#[deprecated(note = "use JSValue::get_property_internal() instead")]
pub unsafe fn JS_GetPropertyInternal(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    this_obj: JSValueConst,
    throw_ref_error: bool,
) -> JSValue {
    obj.get_property_internal(ctx.as_mut().unwrap(), prop, this_obj, throw_ref_error)
}

#[deprecated(note = "use JSValue::get_property() instead")]
#[inline(always)]
pub unsafe fn JS_GetProperty(ctx: *mut JSContext, this_obj: JSValueConst, prop: JSAtom) -> JSValue {
    this_obj.get_property(ctx.as_mut().unwrap(), prop)
}

#[deprecated(note = "use JSValue::get_property_str() instead")]
pub unsafe fn JS_GetPropertyStr(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: *const c_char,
) -> JSValue {
    this_obj.get_property_str(
        ctx.as_mut().unwrap(),
        CStr::from_ptr(prop).to_string_lossy().as_ref(),
    )
    // let atom = JS_NewAtom(ctx, CStr::from_ptr(prop));
    // let ret = JS_GetProperty(ctx, this_obj, atom);
    // JS_FreeAtom(ctx, atom);
    // return ret;
}

/// NOTE: returned property value has increased ref_count by 1
#[deprecated(note = "use JSValue::get_property_value() instead")]
pub unsafe fn JS_GetPropertyValue(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSValue,
) -> JSValue {
    this_obj.get_property_value(ctx.as_mut().unwrap(), prop)
}

#[deprecated(note = "use JSValue::get_property_u32() instead")]
pub unsafe fn JS_GetPropertyUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: u32,
) -> JSValue {
    this_obj.get_property_u32(ctx.as_mut().unwrap(), idx)
}

#[deprecated(note = "use JSValue::get_property_i64() instead")]
pub unsafe fn JS_GetPropertyInt64(ctx: *mut JSContext, obj: JSValueConst, idx: i64) -> JSValue {
    obj.get_property_i64(ctx.as_mut().unwrap(), idx)
}

/// Check if an object has a generalized numeric property `idx`. Returns:
/// Err for exception,
/// Ok(Some(_)) if property exists,
/// Ok(None) if proprty does not exist.
#[deprecated(note = "use JSValue::try_get_property_i64() instead")]
pub unsafe fn JS_TryGetPropertyInt64(
    ctx: *mut JSContext,
    obj: JSValueConst,
    idx: i64,
) -> Result<Option<JSValue>, i32> {
    obj.try_get_property_i64(ctx.as_mut().unwrap(), idx)
}

/* 返回 -1、FALSE 或 TRUE。如果不可配置或无效对象则返回 FALSE。出现异常时返回 -1。
flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT */
#[deprecated(note = "use JSValue::delete_property() instead")]
pub unsafe fn JS_DeleteProperty(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    flags: u32,
) -> Result<bool, i32> {
    obj.delete_property(ctx.as_mut().unwrap(), prop, flags)
}

#[deprecated(note = "use JSValue::delete_property_i64() instead")]
pub unsafe fn JS_DeletePropertyInt64(
    ctx: *mut JSContext,
    obj: JSValueConst,
    idx: i64,
    flags: u32,
) -> Result<bool, i32> {
    obj.delete_property_i64(ctx.as_mut().unwrap(), idx, flags)
}

/// the indirection is needed to make 'eval' optional.
#[deprecated(note = "use JSContext::eval_internal() instead")]
pub unsafe fn JS_EvalInternal(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    input: &CStr,
    filename: &str,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    (*ctx).eval_internal(this_obj, input, filename, flags, scope_idx)
}

#[deprecated(note = "use JSContext::eval_this() instead")]
pub unsafe fn JS_EvalThis(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    input: &CStr,
    filename: &str,
    flags: u32,
) -> JSValue {
    (*ctx).eval_this(this_obj, input, filename, flags)
}

#[deprecated(note = "use JSContext::eval() instead")]
pub unsafe fn JS_Eval(
    ctx: *mut JSContext,
    input: &CStr,
    filename: &str,
    eval_flags: u32,
) -> JSValue {
    (*ctx).eval(input, filename, eval_flags)
}

/// both `this_obj` and `val` are not consumed.
#[deprecated(note = "use JSValue::eval() instead")]
pub unsafe fn JS_EvalObject(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    val: JSValueConst,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    if !val.is_string() {
        return val.dup(); // dup and return val
    }

    match val.coerce_to_utf8(ctx.into()) {
        Ok(str) => (*ctx).eval_internal(
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
pub unsafe fn js_build_module_ns(ctx: *mut JSContext, m: *mut JSModuleDef) -> JSValue {
    (*m).build_ns(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSModuleDef::ns() instead")]
pub unsafe fn js_get_module_ns(ctx: *mut JSContext, m: *mut JSModuleDef) -> JSValue {
    (*m).ns(ctx.as_mut().unwrap()).unwrap_or(JSValue::EXCEPTION)
}

/// build traceback, set it as property of `error_obj`.
/// if filename/line number is provided, they are defined as property of `error_obj` too.
/// `error_obj` ref count is unchanged.
#[deprecated(note = "use JSContext::build_backtrace_to() instead")]
pub unsafe fn build_backtrace(
    ctx: *mut JSContext,
    error_obj: JSValueConst,
    file_line: Option<(&str, u32)>, // filename and line number
    backtrace_flags: i32,
) {
    (*ctx).build_backtrace_to(error_obj, file_line, backtrace_flags);
}

/// op1, op2 are consumed
#[deprecated(note = "use JSValue::strict_eq_with_mode() instead")]
pub unsafe fn js_strict_eq2(
    ctx: *mut JSContext,
    op1: JSValue,
    op2: JSValue,
    eq_mode: JSStrictEqModeEnum,
) -> bool {
    let res = op1.strict_eq_with_mode(op2, eq_mode);
    JS_FreeValue(ctx, op1);
    JS_FreeValue(ctx, op2);
    res
}

/// op1, op2 are consumed
#[deprecated(note = "use JSValue::strict_eq() instead")]
pub unsafe fn js_strict_eq(ctx: *mut JSContext, op1: JSValue, op2: JSValue) -> bool {
    let res = op1.strict_eq(op2);
    JS_FreeValue(ctx, op1);
    JS_FreeValue(ctx, op2);
    res
}

#[deprecated(note = "use JSValue::is_same0() instead")]
pub unsafe fn js_same_value_zero(
    ctx: *mut JSContext,
    op1: JSValueConst,
    op2: JSValueConst,
) -> bool {
    op1.is_same0(op2)
}

/// always returns -1
#[deprecated(note = "use JSParseState::throw_syntax_error() instead")]
pub unsafe fn js_parse_error(s: *mut JSParseState, message: &CStr) -> i32 {
    (*s).throw_syntax_error(message);
    -1
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
    ctx: *mut JSContext,
    s: *mut JSFunctionDef,
    name: JSAtom,
) -> *mut JSGlobalVar {
    (*s).add_global_var(ctx.as_mut().unwrap(), name)
        .map_or(std::ptr::null_mut(), |p| p as *mut JSGlobalVar)
}

#[deprecated(note = "use JSFunctionDef::add_arg() instead")]
pub unsafe fn add_arg(ctx: *mut JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd)
        .add_arg(ctx.as_mut().unwrap(), name)
        .map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::find_arg() instead")]
/* return the variable index or -1 if not found,
add ARGUMENT_VAR_OFFSET for argument variables */
pub unsafe fn find_arg(ctx: *const JSContext, fd: *const JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).find_arg(name).unwrap_or(-1)
}

#[deprecated(note = "use JSFunctionDef::find_var_or_arg() instead")]
pub unsafe fn find_var(ctx: *const JSContext, fd: *const JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).find_var(name).unwrap_or(-1)
}

#[deprecated(note = "use JSFunctionDef::add_var() instead")]
/* return the variable index or -1 if error */
pub unsafe fn add_var(ctx: &mut JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).add_var(ctx, name).map_or(-1, |x| x as i32)
}

// return the constant pool index. 'val' is not duplicated.
#[deprecated(note = "use JSParseState::cpool_add() instead")]
pub unsafe fn cpool_add(s: *mut JSParseState, ctx: *mut JSContext, val: JSValue) -> i32 {
    let fd = (*s).cur_func_mut();
    fd.cpool_add(ctx.into(), val).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use LabelInfo::add_reloc() instead")]
pub unsafe fn add_reloc(
    ctx: *mut JSContext,
    ls: *mut LabelInfo,
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
