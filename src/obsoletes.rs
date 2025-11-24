#![allow(clippy::all)]

use std::{
    ffi::{CStr, c_char, c_void},
    ptr::NonNull,
};

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_BUFFER, JS_CLASS_ASYNC_FUNCTION,
    JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_ERROR,
    JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_OBJECT, JS_CLASS_PROXY,
    array::Array,
    array_buffer::JSFreeArrayBufferDataFunc,
    atom::{
        Atom, AtomDesc, AtomDescKind, AtomHintAndId, AtomKind, AtomRef, JS_ATOM_NULL,
        JS_ATOM_TAG_INT,
    },
    bytecode::{OpCode, OpCodeTemp, arithmatic::StrictEqMode},
    class::{ClassDef, ClassFieldsDef, ClassID, StdClassDef},
    context::ContextRef,
    error::{ErrorKind, JSResult},
    function::{
        AsyncFuncState, BlockEnv, Bytecode, CallContext, FieldDef, JSCFunction, JSCFunctionMagic,
        JSFunctionDef, JSFunctionKindEnum, JSGlobalVar, JSVarDefEnum, JSVarKindEnum, JSVarRef,
        Label, NativeCallback, RelocEntry,
    },
    gc::{GCObjectHeader, JSGCObjectTypeEnum},
    generator::{AGenPayload, GeneratorPayload},
    iterator::{ForInIterPayload, JSIteratorKindEnum, ObjectIter},
    mapset::{JSMapState, MapPair},
    module::{ExportEntry, ExportType, JSResolveResultEnum, ModuleDef, ModuleDefImpl},
    number::{IntOrFloat, JSToNumberHintEnum},
    object::{
        GCObject, JSProperty, JSPropertyDescriptor, ObjectPayload, ObjectRef, PropertyKey,
        ShapeHandle, ShapeProperty,
    },
    parse::{JSParseExportEnum, JSParseFunctionEnum, JSParseState},
    promise::{Promise, PromiseState},
    proxy::ProxyPayload,
    runtime::RuntimeImpl,
    stack_frame::StackFrame,
    string::{StringImpl, StringImpl, UTF8_CHAR_LEN_MAX, char_is_space, unicode_from_utf8_v0},
    typed_array::TypedArray,
    value::JSValue,
};

#[deprecated]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSAutoInitIDEnum {
    JS_AUTOINIT_ID_PROTOTYPE,
    JS_AUTOINIT_ID_MODULE_NS,
    JS_AUTOINIT_ID_PROP,
}

#[deprecated(note = "unused")]
pub fn JS_FreeValue(ctx: ContextRef, v: &JSValue) {
    JS_FreeValueRT(ctx.rt_mut(), v)
}

#[deprecated(note = "unused")]
pub fn JS_FreeValueRT(rt: *mut RuntimeImpl, v: &JSValue) {
    // if let Some(p) = v.payload() {
    //     if p.strong_count() == 1 {
    //         unsafe {
    //             __JS_FreeValueRT(rt, p);
    //         }
    //     }
    // }

    // if JS_VALUE_HAS_REF_COUNT(v) {
    //     let mut gcobj = match v.tag() {
    //         JS_TAG_OBJECT => v.object_unchecked().gc_node_ptr(),
    //         JS_TAG_FUNCTION_BYTECODE => {
    //             (*JS_VALUE_GET_PTR(v).cast::<Bytecode>()).gc_node_ptr()
    //         }
    //         JS_TAG_STRING | JS_TAG_SYMBOL => {
    //             let s = Rc::from_raw(v.string());
    //             if Rc::strong_count(&s) == 1 {
    //                 // rc is about to drop, drop atom description too
    //                 if let Some(at) = s.atom_info() {
    //                     JS_FreeAtomStruct(rt, at.atom());
    //                 }
    //             }
    //             return;
    //         }
    //         _ => return,
    //     };

    //     debug_assert!(gcobj.as_ref().ref_count() > 0);
    //     if gcobj.as_mut().dec_ref() == 0 {
    //         __JS_FreeValueRT(rt, v);
    //     }
    // }
}

/* called with the ref_count of 'v' reaches zero. */
#[deprecated(note = "unused")]
pub unsafe fn __JS_FreeValueRT(rt: *mut RuntimeImpl, payload: &GCObject) {
    //debug_assert_eq!(payload.strong_count(), 1);
    //payload.finalize(rt.as_mut().unwrap());

    // let tag = v.tag();

    // #[cfg(feature = "dump_free")]
    // {
    //     print!("Freeing ({tag:2}) ");
    //     if tag == JS_TAG_OBJECT {
    //         v.object_unchecked().dump(rt.as_ref().unwrap());
    //     } else {
    //         print!("{}", v.repr(rt.as_ref().unwrap()));
    //         println!();
    //     }
    // }

    // match tag {
    //     JS_TAG_OBJECT => {
    //         if (*rt).gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
    //             let node = v.object_unchecked().gc_node_ptr();
    //             debug_assert_eq!(node.as_ref().ref_count(), 0);

    //             Node::unlink(node);
    //             (*rt).gc_zero_ref_count_list.push_raw(node);

    //             if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
    //                 (*rt).gc_free_zeroed();
    //             }
    //         }
    //     }
    //     JS_TAG_FUNCTION_BYTECODE => {
    //         if (*rt).gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
    //             let node = (*JS_VALUE_GET_PTR(v).cast::<Bytecode>()).gc_node_ptr();
    //             debug_assert_eq!(node.as_ref().ref_count(), 0);

    //             Node::unlink(node);
    //             (*rt).gc_zero_ref_count_list.push_raw(node);

    //             if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
    //                 (*rt).gc_free_zeroed();
    //             }
    //         }
    //     }
    //     JS_TAG_STRING | JS_TAG_SYMBOL => {
    //         unreachable!("never freed here");
    //     }
    //     JS_TAG_MODULE => {
    //         unreachable!("never freed here");
    //     }
    //     JS_TAG_BIG_INT | JS_TAG_BIG_FLOAT | JS_TAG_BIG_DECIMAL => {
    //         unimplemented!()
    //     }
    //     _ => {
    //         panic!("__JS_FreeValue: unknown tag={}", tag);
    //     }
    // }
}

/// set new value and free the old value.
/// (freeing the value can reallocate the object data)
#[deprecated(note = "use JSValue::replace_free() instead")]
#[inline(always)]
pub unsafe fn set_value(ctx: ContextRef, pval: *mut JSValue, new_val: JSValue) {
    unreachable!("unused")
}

#[deprecated(note = "use ContextRef::create_string() instead")]
pub unsafe fn JS_NewStringLen(ctx: ContextRef, buf: *const u8, buf_len: usize) -> JSValue {
    unreachable!("unused");
    // ctx.create_string(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
    //     buf, buf_len,
    // )))
}

#[deprecated(note = "use ContextRef::create_string_utf8() instead")]
pub unsafe fn JS_NewString(ctx: ContextRef, str: &CStr) -> JSValue {
    unreachable!("unused");
    // ctx.create_string(str.to_string_lossy().as_ref())
}

#[deprecated(note = "use RuntimeImpl::finalize() instead")]
unsafe fn JS_FreeRuntime(rt: &mut RuntimeImpl) {
    unreachable!("unused");
}

#[deprecated(note = "use ObjectRef::opaque() instead")]
pub unsafe fn JS_GetOpaque(obj: JSValue, class_id: ClassID) -> *mut c_void {
    unreachable!("use ObjectRef::opaque() instead")
    // if JS_VALUE_GET_TAG(obj) != JS_TAG_OBJECT {
    //     return std::ptr::null_mut();
    // }
    // let p = JS_VALUE_GET_OBJ(obj);
    // if (*p).class_id() != class_id {
    //     return std::ptr::null_mut();
    // }
    // (*p).u.opaque
}

#[deprecated(note = "use ObjectRef::opaque_must() instead")]
pub unsafe fn JS_GetOpaque2(ctx: ContextRef, obj: JSValue, class_id: ClassID) -> *mut c_void {
    let p = JS_GetOpaque(obj, class_id);
    if p.is_null() {
        ctx.throw_type_error_invalid_class(class_id);
    }
    p
}

#[deprecated(note = "use ObjectRef::set_opaque() instead")]
pub unsafe fn JS_SetOpaque(obj: JSValue, opaque: *mut c_void) {
    unreachable!("use ObjectRef::set_opaque() instead")

    // if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
    //     let p = JS_VALUE_GET_OBJ(obj);
    //     (*p).u.opaque = opaque;
    // }
}

#[deprecated(note = "unused")]
pub fn JS_ThrowSyntaxErrorAtom(ctx: ContextRef, fmt: &str, atom: AtomRef) {
    unreachable!("unused")
}

#[deprecated(note = "use ContextRef::JS_ThrowTypeErr()")]
pub unsafe fn JS_ThrowTypeErrorAtom(ctx: ContextRef, fmt: &CStr, atom: AtomRef) -> JSValue {
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
    ctx: ContextRef,
    val: JSValue,
    is_ToPropertyKey: bool,
) -> JSValue {
    unreachable!("use JSValue::corece_to_string_value() instead");
}

/// `val` keeps ref count unchanged after calling this function.
#[deprecated(note = "use JSValue::coerce_to_string_value() instead")]
pub unsafe fn JS_ToString(ctx: ContextRef, val: JSValue) -> JSValue {
    unreachable!("use JSValue::coerce_to_string_value() instead");
}

#[deprecated(note = "use JSValue::coerce_to_propkey_value() instead")]
pub unsafe fn JS_ToPropertyKey(ctx: ContextRef, val: JSValue) -> JSValue {
    unreachable!("use JSValue::coerce_to_propkey_value() instead");
}

/// `val` keeps ref count after calling this function.
#[deprecated(note = "use JSValue::coerce_number() instead")]
pub unsafe fn JS_ToNumber(ctx: ContextRef, val: JSValue) -> JSValue {
    val.to_number(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::coerce_to_primitive() + JS_FreeValue() instead")]
pub unsafe fn JS_ToPrimitiveFree(ctx: ContextRef, val: JSValue, hint: i32) -> JSValue {
    unreachable!("use JSValue::coerce_to_primitive() + JS_FreeValue() instead");
}

#[deprecated(note = "use JSValue::coerce_to_primitive() instead")]
pub unsafe fn JS_ToPrimitive(ctx: ContextRef, val: JSValue, hint: i32) -> JSValue {
    val.to_primitive(ctx, hint).unwrap_or(JSValue::EXCEPTION)

    //JS_ToPrimitiveFree(ctx, val.dup(), hint)
}

#[deprecated(note = "use JSValue::is_function() instead")]
pub unsafe fn JS_IsFunction(ctx: ContextRef, val: JSValue) -> bool {
    val.is_function()
}

#[deprecated(note = "use JSValue::is_constructor() instead")]
#[inline(always)]
pub unsafe fn JS_IsConstructor(ctx: ContextRef, val: JSValue) -> bool {
    val.is_constructor()
}

#[deprecated(note = "use ContextRef::add_global_constructor() instead")]
pub unsafe fn JS_NewGlobalCConstructor2(
    ctx: ContextRef,
    func_obj: JSValue,
    name: &CStr,
    proto: JSValue,
) {
    ctx.add_global_constructor(name.to_string_lossy().as_ref(), &func_obj, &proto);
}

#[deprecated(note = "use ContextRef::create_global_constructor() instead")]
pub unsafe fn JS_NewGlobalCConstructor(
    ctx: ContextRef,
    name: &CStr,
    func: JSCFunction,
    length: i32,
    proto: JSValue,
) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use RuntimeImpl::is_class_registered() instead")]
pub unsafe fn JS_IsRegisteredClass(rt: &RuntimeImpl, class_id: ClassID) -> bool {
    (*rt).is_class_registered(class_id)
}

#[deprecated(note = "use RuntimeImpl::register_std_classes() instead")]
pub unsafe fn init_class_range(
    rt: &mut RuntimeImpl,
    tab: &[StdClassDef],
    start: ClassID,
) -> Result<(), i32> {
    (*rt).register_std_classes(tab, start)
}

#[deprecated(note = "not used")]
pub unsafe fn check_exception_free(ctx: ContextRef, obj: JSValue) -> bool {
    unreachable!("unused");
}

#[deprecated(note = "use RuntimeImpl::atom_to_str() instead")]
pub unsafe fn JS_AtomGetStrRT(
    rt: *mut RuntimeImpl,
    buf: *mut c_char,
    buf_size: usize,
    atom: AtomRef,
) -> *const c_char {
    unreachable!("use RuntimeImpl::atom_to_str() instead")
}

#[deprecated(note = "use AtomRef::to_str() instead")]
pub unsafe fn JS_AtomGetStr(
    ctx: ContextRef,
    buf: *mut c_char,
    buf_size: usize,
    atom: AtomRef,
) -> *const c_char {
    unreachable!("unused");
}

#[deprecated(note = "use RuntimeImpl::add_atom() instead")]
/* string case (internal). Return Err if error. 'str' is freed. */
pub unsafe fn __JS_NewAtom(
    rt: *mut RuntimeImpl,
    str: *mut StringImpl,
    atom_kind: AtomKind,
) -> Result<AtomRef, i32> {
    unreachable!("use RuntimeImpl::add_atom() instead");
}

#[deprecated(note = "use RuntimeImpl::add_atom_str() instead")]
pub unsafe fn __JS_NewAtomInit(
    rt: &mut RuntimeImpl,
    str: &str,
    atom_kind: AtomHintAndId,
) -> Result<AtomRef, i32> {
    unreachable!("use RuntimeImpl::add_atom_str() instead");
}

#[deprecated(note = "use StringImpl::atom_kind() instead")]
pub unsafe fn js_get_atom_index(rt: &RuntimeImpl, p: *const AtomDesc) -> AtomRef {
    unreachable!("use StringImpl::atom_kind() instead");

    // let mut i: u32 = (*p).hash_next; /* atom_index */
    // if (*p).atom_kind() != JSAtomType::Symbol {
    //     i = *rt
    //         .atom_hash
    //         .add(((*p).hash() & (rt.atom_hash_size as u32 - 1)) as usize);
    //     let mut p1 = *rt.atom_array.add(i as usize) as *const StringImpl;

    //     while p1 != p {
    //         debug_assert!(i != 0);
    //         i = (*p1).hash_next;
    //         p1 = *rt.atom_array.add(i as usize);
    //     }
    // }
}

#[deprecated(note = "use AtomRef::to_value() instead")]
pub unsafe fn JS_AtomToString(ctx: ContextRef, atom: AtomRef) -> JSValue {
    unreachable!("use AtomRef::to_value() instead");
    // __JS_AtomToValue(ctx, atom, true)
}

/// Warning: 'p' is freed
#[deprecated(note = "use RuntimeImpl::add_atom_jsstring() instead")]
pub unsafe fn JS_NewAtomStr(ctx: ContextRef, p: *mut StringImpl) -> AtomRef {
    unreachable!("use RuntimeImpl::add_atom_jsstring() instead");
}

#[deprecated(note = "use AtomRef::to_utf8_string() instead")]
pub unsafe fn JS_AtomToCString(ctx: ContextRef, atom: AtomRef) -> *const c_char {
    unreachable!("use AtomRef::to_utf8_string() instead");
}

/// `data` values are not consumed.
#[deprecated(note = "use ContextRef::create_native_function_with_data() instead")]
pub unsafe fn JS_NewCFunctionData(
    ctx: ContextRef,
    func: NativeCallback,
    length: u8,
    magic: i32,
    data_count: u8,
    data: *const JSValue,
) -> JSValue {
    unreachable!("unused");
    // ctx.create_native_function_with_data(
    //     func,
    //     length,
    //     magic,
    //     std::slice::from_raw_parts(data, data_count as usize),
    // )
    // .map_or(JSValue::EXCEPTION, JSValue::Object)
}

/// op1, op2 ref_count unchanged
#[deprecated(note = "use JSValue::is_same() instead")]
pub unsafe fn js_same_value(ctx: ContextRef, op1: JSValue, op2: JSValue) -> bool {
    unreachable!("use JSValue::is_same_to() instead");
    // js_strict_eq2(
    //     ctx,
    //     op1.dup(),
    //     op2.dup(),
    //     JSStrictEqModeEnum::JS_EQ_SAME_VALUE,
    // )
}

#[deprecated(note = "unused")]
pub unsafe fn free_arg_list(ctx: ContextRef, tab: *mut JSValue, len: u32) {
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
    token: *mut c_void, //Token,
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

#[deprecated(note = "use StringBuf::parse_u32() instead")]
pub unsafe fn is_num_string(pval: &mut u32, p: *const StringImpl) -> bool {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::is_number() instead")]
pub fn JS_IsNumber(v: JSValue) -> bool {
    v.is_number()
}

/// `val` is consumed.
#[deprecated(note = "use JSValue::coerce_to_string() instead")]
pub fn JS_ToStringFree(ctx: ContextRef, val: JSValue) -> JSValue {
    unreachable!("unused")
}

/// `val` is consumed.
#[deprecated(note = "use JSValue::to_locale_string() instead")]
pub unsafe fn JS_ToLocaleStringFree(ctx: ContextRef, val: JSValue) -> JSValue {
    unreachable!("unused")
    // let r = val.to_locale_string(ctx).unwrap_or(JSValue::EXCEPTION);
    // r
}

#[deprecated(note = "unused")]
pub unsafe fn JS_ResizeAtomHash(rt: *mut RuntimeImpl, new_hash_size: i32) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::class_proto() instead")]
pub unsafe fn JS_GetClassProto(ctx: ContextRef, class_id: ClassID) -> JSValue {
    todo!();
    // ctx.get_class_proto(class_id)
}

#[deprecated(note = "unused")]
pub unsafe fn JS_SetClassProto(ctx: ContextRef, class_id: ClassID, obj: JSValue) {
    todo!()
    // ctx.set_class_proto(class_id, obj);
}

#[deprecated(note = "use RuntimeImpl::register_class() instead")]
pub unsafe fn JS_NewClass(
    rt: *mut RuntimeImpl,
    class_id: ClassID,
    class_def: *const ClassDef,
) -> Result<(), i32> {
    unreachable!("unused")
}

/* create a new object internal class. Return -1 if error, 0 if OK. The finalize can be NULL if none is needed. */
#[deprecated(note = "use RuntimeImpl::add_class_internal() instead")]
pub unsafe fn JS_NewClass1(
    rt: *mut RuntimeImpl,
    class_id: ClassID,
    class_def: *const ClassDef,
    name: AtomRef,
) -> Result<(), i32> {
    unreachable!("use RuntimeImpl::add_class_internal() instead");
}

/// return -1 if exception
#[deprecated(note = "not used")]
pub unsafe fn expand_fast_array(ctx: ContextRef, p: *mut JSValue, new_len: u32) -> i32 {
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

#[deprecated(note = "use ContextRef::create_array_with_capacity() instead")]
pub unsafe fn js_allocate_fast_array(ctx: ContextRef, len: i64) -> JSValue {
    unreachable!("use ContextRef::create_array_with_capacity() instead");
}

#[deprecated(note = "use JSParseState::parse_string_escape() instead")]
pub unsafe fn lre_parse_escape(pp: *mut *const u8, allow_utf16: i32) -> Result<u32, i32> {
    unreachable!("use JSParseState::parse_string_escape() instead")
}

#[deprecated(note = "use JSFunctionDef::find_var_in_child_scope() instead")]
pub unsafe fn find_var_in_child_scope(
    ctx: ContextRef,
    fd: *mut JSFunctionDef,
    name: AtomRef,
    scope_level: i32,
) -> Option<u16> {
    unreachable!("use JSFunctionDef::find_var_in_child_scope() instead");
}

#[deprecated(note = "use JSFunctionDef::add_scope_var() instead")]
pub unsafe fn add_scope_var(
    ctx: ContextRef,
    fd: *mut JSFunctionDef,
    name: AtomRef,
    var_kind: JSVarKindEnum,
) -> i32 {
    unreachable!("use JSFunctionDef::add_scope_var() instead");
}

#[deprecated(note = "use JSStackFrame::get_var_ref() instead")]
pub unsafe fn get_var_ref(
    ctx: ContextRef,
    sf: *mut StackFrame,
    var_idx: u16,
    is_arg: bool,
) -> *mut JSVarRef {
    unreachable!("use JSStackFrame::get_var_ref() instead")
}

#[deprecated(note = "use AGenPayload::dispose() instead")]
pub unsafe fn js_async_generator_free(rt: *mut RuntimeImpl, s: *mut AGenPayload) {
    unreachable!("use AGenPayload::dispose() instead");
}

#[deprecated(note = "use AsyncGeneratorFunction::function_call() instead")]
pub unsafe fn js_async_generator_function_call(
    ctx: ContextRef,
    func_obj: JSValue,
    this_obj: JSValue,
    argc: i32,
    argv: *const JSValue,
    flags: i32,
) -> JSValue {
    unreachable!("use AsyncGeneratorFunction::function_call() instead");
}

#[deprecated(note = "unused")]
unsafe fn js_promise_executor_new(ctx: ContextRef) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use JSPromiseReactionData::dispose() instead")]
pub(super) unsafe fn promise_reaction_data_free(
    rt: *mut RuntimeImpl,
    rd: *mut c_void, // JSPromiseReactionData,
) {
    unreachable!("use JSPromiseReactionData::dispose() instead");
}

#[deprecated(note = "use ContextRef::create_promise() instead")]
pub unsafe fn js_new_promise_capability(
    ctx: ContextRef,
    resolving_funcs: *mut JSValue,
    ctor: JSValue,
) -> JSValue {
    unreachable!("use ContextRef::create_promise() instead")
}

/// create and return [resolve, reject] functions for `promise` object.
/// Note: `promise` is not consumed.
#[deprecated(note = "use Promise::create_resolving_functions() instead")]
pub unsafe fn js_create_resolving_functions(
    ctx: ContextRef,
    promise: JSValue,
) -> Result<[JSValue; 2], i32> {
    unreachable!("use Promise::create_resolving_functions() instead");
}

#[deprecated(note = "use StringImpl::new() instead")]
pub unsafe fn js_alloc_string_rt(
    rt: *mut RuntimeImpl,
    max_len: usize,
    is_wide_char: bool,
) -> *mut StringImpl {
    unreachable!("use StringImpl::new() instead");
    // match StringImpl::new_rt(rt.as_mut().unwrap(), max_len, is_wide_char) {
    //     Ok(s) => Box::into_raw(s),
    //     Err(_) => std::ptr::null_mut(),
    // }
}

#[deprecated(note = "use StringImpl::new() instead")]
pub unsafe fn js_alloc_string(
    ctx: ContextRef,
    max_len: usize,
    is_wide_char: bool,
) -> *mut StringImpl {
    unreachable!("use StringImpl::new() instead");

    // match StringImpl::with_capacity(ctx, max_len, is_wide_char) {
    //     Ok(s) => Box::into_raw(s),
    //     Err(_) => std::ptr::null_mut(),
    // }
}

#[deprecated(note = "use StringImpl::dispose() instead")]
pub unsafe fn js_free_string(rt: &mut RuntimeImpl, str: *mut StringImpl) {
    unreachable!("unused");
}

#[deprecated(note = "use StringBuf::cmp() instead")]
pub unsafe fn js_string_memcmp(p1: *const StringImpl, p2: *const StringImpl, len: usize) -> i32 {
    unreachable!("use StringBuf::cmp() instead");

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

#[deprecated(note = "use StringBuf::hash() instead")]
pub unsafe fn hash_string(str: *const StringImpl, h: u32) -> u32 {
    unreachable!("use StringBuf::hash() instead");
}

pub unsafe fn copy_str16(dst: *mut u16, p: *const StringImpl, offset: usize, len: usize) {
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
    ctx: ContextRef,
    p1: *const StringImpl,
    p2: *const StringImpl,
) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::create_string8() instead")]
pub unsafe fn js_new_string8(ctx: ContextRef, buf: *const u8, len: usize) -> JSValue {
    unreachable!("js_new_string8() is obsolete");
}

#[deprecated(note = "use ContextRef::create_string16() instead")]
pub unsafe fn js_new_string16(ctx: ContextRef, buf: *const u16, len: usize) -> JSValue {
    unreachable!("js_new_string16() is obsolete");
}

#[deprecated(note = "use ContextRef::create_string_char() instead")]
pub unsafe fn js_new_string_char(ctx: ContextRef, c: u16) -> JSValue {
    unreachable!("js_new_string_char() is obsolete");
}

/* return (NULL, 0) if exception. */
/* return pointer into a StringImpl with a live ref_count */
/* cesu8 determines if non-BMP1 codepoints are encoded as 1 or 2 utf-8 sequences */
#[deprecated(note = "use JSValue::to_utf8() instead")]
pub unsafe fn JS_ToCStringLen2(
    ctx: ContextRef,
    plen: *mut usize,
    val1: JSValue,
    cesu8: bool,
) -> *const c_char {
    unreachable!("use JSValue::to_utf8() instead");
}

#[deprecated(note = "use JSValue::to_utf8() instead")]
pub unsafe fn JS_ToCStringLen(ctx: ContextRef, plen: *mut usize, val1: JSValue) -> *const c_char {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::to_string().content().to_utf8() instead")]
pub unsafe fn JS_ToCString(ctx: ContextRef, val1: JSValue) -> *const c_char {
    unreachable!("unused");
}

#[deprecated(note = "not used")]
pub unsafe fn JS_FreeCString(ctx: ContextRef, ptr: *const c_char) {
    unreachable!("JS_FreeCString() is not used any more");
}

/// return 0 if OK, < 0 if exception.
#[deprecated(note = "use ContextRef::enqueue_job() instead")]
pub unsafe fn JS_EnqueueJob(
    ctx: ContextRef,
    job_func: *mut c_void,
    argc: i32,
    argv: *const JSValue,
) -> i32 {
    unreachable!()
}

/// return < 0 if exception, 0 if no job pending, 1 if a job was
/// executed successfully. the context of the job is stored in '*pctx'
#[deprecated(note = "use RuntimeImpl::execute_pending_job() instead")]
pub unsafe fn JS_ExecutePendingJob(rt: &mut RuntimeImpl, pctx: *mut ContextRef) -> i32 {
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

#[deprecated(note = "use ContextRef::create_promise() instead")]
pub unsafe fn JS_NewPromiseCapability(ctx: ContextRef, resolving_funcs: *mut JSValue) -> JSValue {
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
pub unsafe fn JS_PromiseState(ctx: ContextRef, promise: JSValue) -> Result<PromiseState, i32> {
    unreachable!("unused");
}

/// to free `fd` and all its children
#[deprecated(note = "use JSFunctionDef::dispose() instead")]
pub unsafe fn js_free_function_def(ctx: ContextRef, fd: *mut JSFunctionDef) {
    unreachable!("unused");
}

/// create a function object from a function definition.
/// All the child functions are also created. It must be done this way to resolve all the variables.
/// The function definition is freed.
#[deprecated(note = "use JSFunctionDef::compile() instead")]
pub unsafe fn js_create_function(ctx: ContextRef, fd: *mut JSFunctionDef) -> JSValue {
    unreachable!("unused");
    // Box::from_raw(fd).make_function(ctx)
}

#[deprecated(note = "use Bytecode::find_line_num() instead")]
pub unsafe fn find_line_num(ctx: ContextRef, b: *mut Bytecode, pc_value: u32) -> i32 {
    (*b).find_line_num(pc_value).unwrap_or(-1)
}

/// Note: `value` is duplicated inside.
#[deprecated(note = "use Promise::fulfill_or_reject() instead")]
pub unsafe fn fulfill_or_reject_promise(
    ctx: ContextRef,
    promise: Promise,
    value: JSValue,
    is_reject: bool,
) {
    unreachable!("use Promise::fulfill_or_reject() instead");
}

#[deprecated(note = "use Promise::reject() instead")]
pub unsafe fn reject_promise(ctx: ContextRef, promise: Promise, value: JSValue) {
    unreachable!("use Promise::reject() instead");
}

#[deprecated(note = "use Promise::reaction() instead")]
pub unsafe fn perform_promise_then(
    ctx: ContextRef,
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
pub unsafe fn js_parse_from_clause(s: *mut JSParseState) -> AtomRef {
    unreachable!("use JSParseState::parse_from_clause() instead");
}

/// Run the <eval> function of the module and of all its requested modules.
/// Return a promise or an exception.
#[deprecated(note = "use ModuleDef::eval() instead")]
pub unsafe fn js_evaluate_module(ctx: ContextRef, m: *mut ModuleDef) -> JSValue {
    unreachable!("");
    // (*m).eval(ctx).map_or(JSValue::EXCEPTION, |p| p.into())
}

#[deprecated(note = "use Promise::result() instead")]
pub unsafe fn JS_PromiseResult(ctx: ContextRef, promise: JSValue) -> JSValue {
    Promise::try_from(promise).unwrap().result()
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
    ctx: ContextRef,
    this_obj: JSValue,
    prop: AtomRef,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> i32 {
    unreachable!("ununsed");
    // this_obj
    //     .define_property(ctx, prop, val, getter, setter, flags)
    //     .map_or(-1, |b| b as i32)
}

/// shortcut to add or redefine a new property value.
/// Note: property value is consumed.
#[deprecated(note = "use JSValue::defined_property_value() instead")]
pub unsafe fn JS_DefinePropertyValue(
    ctx: ContextRef,
    this_obj: JSValue,
    prop: AtomRef,
    val: JSValue,
    flags: u32,
) -> i32 {
    unreachable!("ununsed");
    // this_obj
    //     .define_property_value(ctx, prop, val, flags)
    //     .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::defined_property_value_str() instead")]
pub unsafe fn JS_DefinePropertyValueStr(
    ctx: ContextRef,
    this_obj: JSValue,
    prop: &CStr,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .define_property_value_str(ctx, prop.to_string_lossy().as_ref(), val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "unused")]
pub unsafe fn JS_DefinePropertyValueValue(
    ctx: ContextRef,
    this_obj: JSValue,
    prop: JSValue,
    val: JSValue,
    flags: u32,
) -> i32 {
    unreachable!("unused")
}

#[deprecated(note = "use JSValue::create_data_property_u32() instead")]
pub unsafe fn JS_CreateDataPropertyUint32(
    ctx: ContextRef,
    this_obj: JSValue,
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
    ctx: ContextRef,
    this_obj: JSValue,
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
    ctx: ContextRef,
    this_obj: JSValue,
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
    ctx: ContextRef,
    obj: JSValue,
    prop: AtomRef,
    desc: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("unused");
}

/// `getter` and `setter` are consumed.
#[deprecated(note = "use JSValue::define_property_getset() instead")]
pub unsafe fn JS_DefinePropertyGetSet(
    ctx: ContextRef,
    this_obj: JSValue,
    prop: AtomRef,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> i32 {
    unreachable!("ununsed");
    // this_obj
    //     .define_property_getset(ctx, prop, getter, setter, flags)
    //     .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use ObjectRef::define_auto_init_property() instead")]
pub unsafe fn JS_DefineAutoInitProperty(
    ctx: ContextRef,
    this_obj: JSValue,
    prop: AtomRef,
    id: JSAutoInitIDEnum,
    opaque: *mut std::ffi::c_void,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("use ObjectRef::define_auto_init_property() instead");
    // this_obj.define_auto_init_property(ctx, prop, id, opaque, flags)
}

/// returned value has its ref_count increased by 1.
#[deprecated(note = "use JSValue::get_property_internal() instead")]
pub unsafe fn JS_GetPropertyInternal(
    ctx: ContextRef,
    obj: JSValue,
    prop: AtomRef,
    this_obj: JSValue,
    throw_ref_error: bool,
) -> JSValue {
    unreachable!("unused");
    // obj.get_property_internal(ctx, prop, &this_obj, throw_ref_error)
}

#[deprecated(note = "use JSValue::get_property() instead")]
#[inline(always)]
pub unsafe fn JS_GetProperty(ctx: ContextRef, this_obj: JSValue, prop: AtomRef) -> JSValue {
    unreachable!("unused");
    // this_obj.get_property(ctx, prop)
}

#[deprecated(note = "use JSValue::get_property_str() instead")]
pub unsafe fn JS_GetPropertyStr(
    ctx: ContextRef,
    this_obj: JSValue,
    prop: *const c_char,
) -> JSValue {
    unreachable!("unused");
    // this_obj.get_property_str(ctx, CStr::from_ptr(prop).to_string_lossy().as_ref())
}

/// NOTE: returned property value has increased ref_count by 1
#[deprecated(note = "use JSValue::get_property_value() instead")]
pub unsafe fn JS_GetPropertyValue(ctx: ContextRef, this_obj: JSValue, prop: JSValue) -> JSValue {
    unreachable!("unused");
    // this_obj.get_property_value(ctx, &prop)
}

#[deprecated(note = "use JSValue::get_property_u32() instead")]
pub unsafe fn JS_GetPropertyUint32(ctx: ContextRef, this_obj: JSValue, idx: u32) -> JSValue {
    unreachable!("unused");
    // this_obj.get_property_u32(ctx, idx)
}

#[deprecated(note = "use JSValue::get_property_i64() instead")]
pub unsafe fn JS_GetPropertyInt64(ctx: ContextRef, obj: JSValue, idx: i64) -> JSValue {
    unreachable!("unused");
    // obj.get_property_i64(ctx, idx)
}

/// Check if an object has a generalized numeric property `idx`. Returns:
/// Err for exception,
/// Ok(Some(_)) if property exists,
/// Ok(None) if proprty does not exist.
#[deprecated(note = "use JSValue::try_get_property_i64() instead")]
pub unsafe fn JS_TryGetPropertyInt64(
    ctx: ContextRef,
    obj: JSValue,
    idx: i64,
) -> Result<Option<JSValue>, i32> {
    obj.try_get_property_i64(ctx, idx)
}

/* 返回 -1、FALSE 或 TRUE。如果不可配置或无效对象则返回 FALSE。出现异常时返回 -1。
flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT */
#[deprecated(note = "use JSValue::delete_property() instead")]
pub unsafe fn JS_DeleteProperty(
    ctx: ContextRef,
    obj: JSValue,
    prop: AtomRef,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::delete_property_i64() instead")]
pub unsafe fn JS_DeletePropertyInt64(
    ctx: ContextRef,
    obj: JSValue,
    idx: i64,
    flags: u32,
) -> Result<bool, i32> {
    obj.delete_property_i64(ctx, idx, flags)
}

/// the indirection is needed to make 'eval' optional.
#[deprecated(note = "use ContextRef::eval_internal() instead")]
pub unsafe fn JS_EvalInternal(
    ctx: ContextRef,
    this_obj: JSValue,
    input: &CStr,
    filename: &str,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::eval_this() instead")]
pub unsafe fn JS_EvalThis(
    ctx: ContextRef,
    this_obj: JSValue,
    input: &CStr,
    filename: &str,
    flags: u32,
) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::eval() instead")]
pub unsafe fn JS_Eval(ctx: ContextRef, input: &CStr, filename: &str, eval_flags: u32) -> JSValue {
    unreachable!("unused");
}

/// both `this_obj` and `val` are not consumed.
pub unsafe fn JS_EvalObject(
    ctx: ContextRef,
    this_obj: JSValue,
    val: JSValue,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    unreachable!("unued");
}

#[deprecated(note = "use ModuleDef::build_ns() instead")]
pub unsafe fn js_build_module_ns(ctx: ContextRef, m: *mut ModuleDef) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use ModuleDef::ns() instead")]
pub unsafe fn js_get_module_ns(ctx: ContextRef, m: *mut ModuleDef) -> JSValue {
    (*m).ns(ctx).map_or(JSValue::EXCEPTION, JSValue::Object)
}

/// build traceback, set it as property of `error_obj`.
/// if filename/line number is provided, they are defined as property of `error_obj` too.
/// `error_obj` ref count is unchanged.
#[deprecated(note = "use ContextRef::build_backtrace_to() instead")]
pub unsafe fn build_backtrace(
    ctx: ContextRef,
    error_obj: JSValue,
    file_line: Option<(&str, i32)>, // filename and line number
    backtrace_flags: i32,
) {
    ctx.build_backtrace_to(&error_obj, file_line, backtrace_flags);
}

/// op1, op2 are consumed
#[deprecated(note = "use JSValue::strict_eq_with_mode() instead")]
pub unsafe fn js_strict_eq2(
    ctx: ContextRef,
    op1: JSValue,
    op2: JSValue,
    eq_mode: StrictEqMode,
) -> bool {
    unreachable!("use JSValue::strict_eq_with_mode() instead");
    // let res = op1.strict_eq_with_mode(op2, eq_mode);
    // JS_FreeValue(ctx, op1);
    // JS_FreeValue(ctx, op2);
    // res
}

/// op1, op2 are consumed
#[deprecated(note = "use JSValue::strict_eq() instead")]
pub unsafe fn js_strict_eq(ctx: ContextRef, op1: JSValue, op2: JSValue) -> bool {
    unreachable!("use JSValue::strict_eq() instead");
    // let res = op1.strict_eq(op2);
    // JS_FreeValue(ctx, op1);
    // JS_FreeValue(ctx, op2);
    // res
}

#[deprecated(note = "use JSValue::is_same0() instead")]
pub unsafe fn js_same_value_zero(ctx: ContextRef, op1: JSValue, op2: JSValue) -> bool {
    op1.is_same0(&op2)
}

/// always returns -1
#[deprecated(note = "use JSParseState::throw_syntax_error() instead")]
pub unsafe fn js_parse_error(s: *mut JSParseState, message: &CStr) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSFunctionDef::find_global_var() instead")]
pub unsafe fn find_global_var(fd: *mut JSFunctionDef, name: &Atom) -> *mut JSGlobalVar {
    (*fd)
        .find_global_var(name)
        .map_or(std::ptr::null_mut(), |v| v as *const JSGlobalVar as *mut _)
}

/* add a global variable definition */
#[deprecated(note = "use JSFunctionDef::add_global_var() instead")]
pub unsafe fn add_global_var(
    ctx: ContextRef,
    s: *mut JSFunctionDef,
    name: &Atom,
) -> *mut JSGlobalVar {
    (*s).add_global_var(ctx, name)
        .map_or(std::ptr::null_mut(), |p| p as *mut JSGlobalVar)
}

#[deprecated(note = "use JSFunctionDef::add_arg() instead")]
pub unsafe fn add_arg(ctx: ContextRef, fd: *mut JSFunctionDef, name: AtomRef) -> i32 {
    unreachable!("unused");
    // (*fd).add_arg(ctx, name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::find_arg() instead")]
/* return the variable index or -1 if not found,
add ARGUMENT_VAR_OFFSET for argument variables */
pub unsafe fn find_arg(ctx: ContextRef, fd: *const JSFunctionDef, name: AtomRef) -> i32 {
    unreachable!("unused");
    // (*fd).find_arg(name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::find_var_or_arg() instead")]
pub unsafe fn find_var(ctx: ContextRef, fd: *const JSFunctionDef, name: AtomRef) -> i32 {
    unreachable!("unused");
    // (*fd).find_var(name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_var() instead")]
/* return the variable index or -1 if error */
pub unsafe fn add_var(ctx: ContextRef, fd: *mut JSFunctionDef, name: AtomRef) -> i32 {
    unreachable!("unused");
    // (*fd).add_var(ctx, name).map_or(-1, |x| x as i32)
}

// return the constant pool index. 'val' is not duplicated.
#[deprecated(note = "use JSParseState::cpool_add() instead")]
pub unsafe fn cpool_add(s: *mut JSParseState, ctx: ContextRef, val: JSValue) -> i32 {
    unreachable!("unused");
    // let fd = (*s).cur_func_mut();
    // fd.cpool_add(val).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use LabelInfo::add_reloc() instead")]
pub unsafe fn add_reloc(
    ctx: ContextRef,
    ls: *mut Label,
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
    unreachable!("unused");
    // (*s).cur_func_mut().add_label().map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSParseState::parse_template_part() instead")]
pub unsafe fn js_parse_template_part(s: *mut JSParseState, p: *const u8) -> i32 {
    unreachable!("unused");
    // (*s).parse_template_part(p).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::push_state() instead")]
pub unsafe fn push_scope(s: *mut JSParseState) -> i32 {
    unreachable!("unused");
    // (*s).push_scope().map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSParseState::pop_state() instead")]
pub unsafe fn pop_scope(s: *mut JSParseState) {
    unreachable!("unused");
    // (*s).pop_scope();
}

#[deprecated(note = "use JSParseState::close_scopes() instead")]
pub unsafe fn close_scopes(s: *mut JSParseState, scope: i32, scope_stop: i32) {
    unreachable!("unused");
    // (*s).close_scopes(scope, scope_stop);
}

// 'c' is the first character. Return JS_ATOM_NULL in case of error
#[deprecated(note = "use JSParseState::parse_ident() instead")]
pub unsafe fn parse_ident(
    s: *mut JSParseState,
    pp: *mut *const u8,
    pident_has_escape: *mut bool,
    c: char,
    is_private: bool,
) -> AtomRef {
    unreachable!("unused");
}

/// find a variable declaration in a given scope
#[deprecated(note = "use JSFunctionDef::find_var_in_scope() instead")]
pub fn find_var_in_scope(
    ctx: ContextRef,
    fd: &mut JSFunctionDef,
    name: AtomRef,
    scope_level: i32,
) -> Option<u16> {
    unreachable!("unused");
}

#[deprecated(note = "use JSFunctionDef::find_lexical_global_var_mut() instead")]
pub unsafe fn find_lexical_global_var(fd: *mut JSFunctionDef, name: AtomRef) -> *mut JSGlobalVar {
    unreachable!("unused");

    // (*fd)
    //     .find_lexical_global_var_mut(name)
    //     .map_or(std::ptr::null_mut(), |v| v as *mut _)
}

#[deprecated(note = "use JSFSunctionDef::find_lexical_decl() instead")]
pub unsafe fn find_lexical_decl(
    ctx: ContextRef,
    fd: *mut JSFunctionDef,
    name: AtomRef,
    scope_idx: i32,
    check_catch_var: bool,
) -> i32 {
    unreachable!("unused");
    // debug_assert!(scope_idx >= 0);
    // (*fd)
    //     .find_lexical_decl(name, scope_idx as _, check_catch_var)
    //     .map_or(-1, |x| x as i32)
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
    unreachable!("unused");
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
    ctx: ContextRef,
    s: *mut JSFunctionDef,
    var_name: AtomRef,
    scope_level: i32,
    op: u8,
    bc: &[u8],
    bc_buf: *mut u8, // JSFunctionDef::byte_code buffer ptr
    label: Option<&Label>,
    pos_next: usize,
) -> usize {
    unreachable!("use JSFUnctionDef::resolve_scope_var() instead");
}

/// Resolves variable references in the function definition
/// Converts global variable accesses to local/closure variables when needed
/// This is the second pass of bytecode generation
/// Handles scope resolution and variable binding
#[deprecated(note = "use JSFunctionDef::resolve_variables() instead")]
pub unsafe fn resolve_variables(ctx: ContextRef, s: *mut JSFunctionDef) -> Result<(), i32> {
    (*s).resolve_variables(ctx)
}

#[deprecated(note = "use JSParseState::parse_function_decl() instead")]
pub unsafe fn js_parse_function_decl(
    s: *mut JSParseState,
    func_type: JSParseFunctionEnum,
    func_kind: JSFunctionKindEnum,
    func_name: AtomRef,
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
    func_name: AtomRef,
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

#[deprecated(note = "use ContextRef::create_array() instead")]
pub unsafe fn JS_NewArray(ctx: ContextRef) -> JSValue {
    unreachable!("unused")
    // ctx.create_array().unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use ContextRef::create_object_from_shape() instead")]
pub unsafe fn JS_NewObjectFromShape(
    ctx: ContextRef,
    sh: *mut ShapeHandle,
    class_id: ClassID,
) -> JSValue {
    unreachable!("unused");
}

/* create a new empty shape with prototype 'proto' */
#[deprecated(note = "use ShapeHandle::with_capacity instead")]
pub unsafe fn js_new_shape2(
    ctx: ContextRef,
    proto: *mut JSValue,
    hash_size: usize,
    prop_size: i32,
) -> *mut ShapeHandle {
    unreachable!("unused");
    // ShapeHandle::with_capacity(ctx, proto.as_mut(), hash_size, prop_size)
    //     .map_or(std::ptr::null_mut(), |p| p.as_ptr())
}

#[deprecated(note = "use ShapeHandle::new() instead")]
pub unsafe fn js_new_shape(ctx: ContextRef, proto: *mut JSValue) -> *mut ShapeHandle {
    unreachable!("unused");
}

#[deprecated(note = "use ShapeHandle::dup() instead")]
pub unsafe fn js_dup_shape(sh: *mut ShapeHandle) -> *mut ShapeHandle {
    unreachable!("unused");
}

/* The shape is cloned. The new shape is not inserted in the shape hash table */
#[deprecated(note = "use ShapeHandle::clone() instead")]
pub unsafe fn js_clone_shape(
    ctx: ContextRef,
    sh1: *mut ShapeHandle,
) -> Result<*mut ShapeHandle, i32> {
    unreachable!("unused");
}

#[deprecated(note = "use ShapeHandle::dispose() instead")]
pub unsafe fn js_free_shape0(rt: *mut RuntimeImpl, sh: *mut ShapeHandle) {
    unreachable!("unused");
    // debug_assert_eq!((*sh).header.ref_count, 0);
    // Box::from_raw(sh).dispose(rt.as_mut().unwrap());
}

#[deprecated(note = "use ShapeHandle::free() instead")]
pub unsafe fn js_free_shape(rt: *mut RuntimeImpl, sh: *mut ShapeHandle) {
    unreachable!("unused");
}

#[deprecated(note = "use is_null() check plus ShapeHandle::free()")]
pub unsafe fn js_free_shape_null(rt: *mut RuntimeImpl, sh: *mut ShapeHandle) {
    unreachable!("unused");
}

#[deprecated(note = "use ObjectRef::free() instead")]
pub unsafe fn free_object(rt: *mut RuntimeImpl, p: NonNull<JSValue>) {
    unreachable!("unused");
    // ObjectRef::dispose(rt.as_mut().unwrap(), p);
}

#[deprecated(note = "not used")]
pub unsafe fn init_shape_hash(rt: &mut RuntimeImpl) -> i32 {
    unreachable!();
}
#[deprecated(note = "not used")]
pub unsafe fn resize_shape_hash(rt: &mut RuntimeImpl, new_shape_hash_bits: i32) -> i32 {
    unreachable!("not used");
}

#[deprecated(note = "use RuntimeImpl::register_shape() instead")]
pub fn js_shape_hash_link(rt: &mut RuntimeImpl, sh: ShapeHandle) {
    unreachable!("unused")
}

#[deprecated(note = "use RuntimeImpl::unregister_shape() instead")]
pub fn js_shape_hash_unlink(rt: &mut RuntimeImpl, sh: ShapeHandle) {
    unreachable!("unused");
}

/// find a hashed empty shape matching the prototype. Return None if not found
#[deprecated(note = "use RuntimeImpl::find_shape_by_proto() instead")]
pub unsafe fn find_hashed_shape_proto(
    rt: &mut RuntimeImpl,
    proto: *mut JSValue,
) -> Option<ShapeHandle> {
    unreachable!("unused");
}

/// find a hashed shape matching sh + (prop, prop_flags). Return None if not found
#[deprecated(note = "use RuntimeImpl::find_shape_with_prop() instead")]
pub unsafe fn find_hashed_shape_prop(
    rt: &mut RuntimeImpl,
    sh: *mut ShapeHandle,
    atom: AtomRef,
    prop_flags: u32,
) -> Option<ShapeHandle> {
    unreachable!("unused");
}

#[deprecated(note = "use RuntimeImpl::gc() instead")]
pub fn JS_RunGC(rt: &mut RuntimeImpl) {
    rt.gc();
}

#[deprecated(note = "use JSMapState::class_finalize() instead")]
pub unsafe fn js_map_finalizer(rt: *mut RuntimeImpl, val: JSValue) {
    unreachable!("unused");
}

#[deprecated(note = "use JSMapState::class_mark() instead")]
pub unsafe fn js_map_mark(rt: *mut RuntimeImpl, val: JSValue, mark_func: JS_MarkFunc) {
    unreachable!("unused");
    // JSMapState::class_mark(rt, val, mark_func);
}

// for debug only: dump an object without side effect
#[deprecated(note = "use ObjectRef::dump() instead")]
pub unsafe fn JS_DumpObject(rt: &RuntimeImpl, p: *const JSValue) {
    unreachable!("unused");
}

/// Adds a new GC object to the garbage collector's tracking list
/// Initializes the object's mark and type, then adds it to the GC object list
#[deprecated]
pub unsafe fn add_gc_object(rt: *mut RuntimeImpl, h: *mut GCObjectHeader, tp: JSGCObjectTypeEnum) {
    unreachable!("unused");
}

/// Function type for marking objects during garbage collection
/// Parameters:
/// - rt: Pointer to the JavaScript runtime
/// - gp: Pointer to the garbage collected object header
#[deprecated]
pub type JS_MarkFunc = unsafe fn(rt: *mut crate::runtime::RuntimeImpl, gp: *mut GCObjectHeader);

/// Marks a JavaScript value during garbage collection
/// Only marks values that have reference counts (objects and function bytecode)
#[deprecated]
pub unsafe fn JS_MarkValue(
    rt: *mut crate::runtime::RuntimeImpl,
    val: crate::value::JSValue,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

/// Marks the realm associated with an auto-initialized property
/// This ensures the realm is preserved during garbage collection
#[deprecated]
pub unsafe fn js_autoinit_mark(rt: *mut RuntimeImpl, pr: *mut JSProperty, mark_func: JS_MarkFunc) {
    unreachable!("unused");
}

/// Recursively marks all child objects of a GC object
/// This ensures all referenced objects are preserved during garbage collection
#[deprecated]
pub(super) unsafe fn mark_children(
    rt: *mut crate::runtime::RuntimeImpl,
    gp: *mut GCObjectHeader,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

#[deprecated]
pub unsafe fn JS_MarkContext(
    rt: &mut crate::runtime::RuntimeImpl,
    ctx: &mut crate::context::ContextRef,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

#[deprecated]
pub unsafe fn js_mark_module_def(rt: *mut RuntimeImpl, m: &mut ModuleDef, mark_func: JS_MarkFunc) {
    unreachable!("unused");
}

/// Preserves objects with positive reference counts and their child objects.
/// Identifies objects with zero reference count in the GC object list
/// and moves them to a temporary list for removal.
#[deprecated]
pub unsafe fn gc_scan(rt: &mut RuntimeImpl) {
    unreachable!("unused");
}

/// Frees objects in the temporary list, handling cyclic references
#[deprecated]
pub unsafe fn gc_free_cycles(rt: &mut RuntimeImpl) {
    unreachable!("unused");
}

/// Frees a GC object based on its type
/// Handles different types of GC objects with their specific cleanup logic
#[deprecated(note = "use gc_free() instead")]
pub unsafe fn free_gc_object(rt: *mut RuntimeImpl, gp: *mut GCObjectHeader) {
    unreachable!("unused");
}

/// Removes an object from either the main GC object list or temporary list
#[deprecated(note = "use set status to Zeroed instead")]
pub unsafe fn remove_gc_object(h: *mut GCObjectHeader) {
    unreachable!("unused");
}

/// Frees objects with zero reference count (in gc_zero_ref_count_list chain).
#[deprecated(note = "use RuntimeImpl::gc_free_zeroed() instead")]
pub unsafe fn free_zero_refcount(rt: *mut RuntimeImpl) {
    unreachable!("unused");
}

/// Decrements the reference count of a child GC object
/// Moves the object to the temporary list if its refcount reaches zero
#[deprecated]
pub unsafe fn gc_decref_child(rt: *mut RuntimeImpl, p: *mut GCObjectHeader) {
    unreachable!("unsed");
}

/// Decreases reference counts for all child objects in the GC object list.
/// Moves objects with zero reference count to a temporary list for cleanup.
#[deprecated(note = "use RuntimeImpl::gc_deref() instead")]
pub unsafe fn gc_decref(rt: &mut RuntimeImpl) {
    unreachable!("unsed");
}

#[deprecated(note = "use GCObject::gc_dump() instead")]
pub unsafe fn JS_DumpGCObject(rt: *mut RuntimeImpl, p: *mut GCObjectHeader) {
    unreachable!("unused");
}

#[deprecated(note = "use RuntimeImpl::gc_dump_objects() instead")]
pub unsafe fn JS_DumpGCObjects(rt: *mut RuntimeImpl) {
    unreachable!("unused");
}

#[deprecated(note = "use AsyncFuncState::finalize() instead")]
pub unsafe fn __async_func_free(rt: *mut RuntimeImpl, s: *mut AsyncFuncState) {
    unreachable!("unused");
}

#[deprecated(note = "use AsyncFuncState::free() instead")]
pub unsafe fn async_func_free(rt: *mut RuntimeImpl, s: *mut AsyncFuncState) {
    unreachable!("unused");
}

/// make new AsyncFuncState (used by generator and async functions)
#[deprecated(note = "use AsyncFuncState::new() instead")]
pub unsafe fn async_func_init(
    ctx: ContextRef,
    func_obj: JSValue,
    this_obj: JSValue,
    argc: i32,
    argv: *const JSValue,
) -> *mut AsyncFuncState {
    unreachable!("use AsyncFuncState::new() instead");
    // AsyncFuncState::new(
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
#[deprecated(note = "use ContextRef::eval_internal() instead")]
pub fn __JS_EvalInternal(
    ctx: ContextRef,
    this_obj: JSValue,
    input: &CStr,
    filename: &str,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::is_error() instead")]
pub fn JS_IsError(val: &JSValue) -> bool {
    if let JSValue::Object(o) = val {
        o.class_id() == JS_CLASS_ERROR
    } else {
        false
    }
}

/// Note: `val` is consumed
#[deprecated(
    note = "use JSValue::set_property_internal() or ObjectRef::set_property_internal() instead"
)]
pub unsafe fn JS_SetPropertyInternal(
    ctx: ContextRef,
    obj: JSValue,
    prop: AtomRef,
    val: JSValue,
    this_obj: JSValue,
    flags: u32,
) -> i32 {
    unreachable!("unused")
}

/// returns 0,1,-1
/// Note: `val` is consumed
#[deprecated(note = "use JSValue::set_property_value() instead")]
pub unsafe fn JS_SetPropertyValue(
    ctx: ContextRef,
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
    ctx: ContextRef,
    this_obj: JSValue,
    prop: AtomRef,
    val: JSValue,
) -> i32 {
    unreachable!("ununsed");
    // this_obj
    //     .set_property(ctx, prop, val)
    //     .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_u32() instead")]
pub unsafe fn JS_SetPropertyUint32(
    ctx: ContextRef,
    this_obj: JSValue,
    idx: u32,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property_u32(ctx, idx, val)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_i64() instead")]
pub unsafe fn JS_SetPropertyInt64(
    ctx: ContextRef,
    this_obj: JSValue,
    idx: i64,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property_i64(ctx, idx, val)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_str() instead")]
pub unsafe fn JS_SetPropertyStr(
    ctx: ContextRef,
    this_obj: JSValue,
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
    ctx: ContextRef,
    obj: JSValue,
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
    ctx: ContextRef,
    this_val: JSValue,
    is_dataview: bool,
) -> *mut JSValue {
    unreachable!("unused");
    // this_val
    //     .typed_array_must(ctx, is_dataview)
    //     .map_or(std::ptr::null_mut(), |p| {
    //         p.0.object_unchecked() as *mut ObjectRef
    //     })
}

/* WARNING: 'p' must be a typed array. Works even if the array buffer
is detached */
#[deprecated(note = "use TypedArray::capacity() instead")]
pub unsafe fn typed_array_get_length(ctx: ContextRef, p: *mut JSValue) -> usize {
    unreachable!("use TypedArray::capacity() instead")
}

/// returns Err if val is an exception.
/// `val` is consumed
#[deprecated(note = "use JSValue::to_bool() instead")]
pub unsafe fn JS_ToBoolFree(ctx: ContextRef, val: JSValue) -> Result<bool, i32> {
    val.to_bool()
}
#[deprecated(note = "use JSValue::to_bool() instead")]
pub unsafe fn JS_ToBool(ctx: ContextRef, val: &JSValue) -> Result<bool, i32> {
    val.to_bool()
}

#[deprecated(note = "use JSValue::get_own_property_names2() instead")]
pub unsafe fn JS_GetOwnPropertyNames2(
    ctx: ContextRef,
    target: JSValue,
    flags: i32,
    kind: JSIteratorKindEnum,
) -> JSValue {
    target
        .build_own_property_kvs_array(ctx, flags, kind)
        .unwrap_or(JSValue::EXCEPTION)
}

// #[deprecated]
// pub unsafe fn js_free_prop_enum(ctx: ContextRef, tab: *mut PropertyKey, len: usize) {
//     if !tab.is_null() {
//         for i in 0..len {
//             JS_FreeAtom(ctx, (*tab.add(i)).atom);
//         }
//         js_free(ctx, tab.cast());
//     }
// }

// ptab and its atoms must be freed by the user.
#[deprecated(note = "use ObjectRef::get_own_property_names() instead")]
pub unsafe fn JS_GetOwnPropertyNamesInternal(
    ctx: ContextRef,
    ptab: *mut *mut PropertyKey,
    plen: *mut u32,
    p: *mut JSValue,
    flags: i32,
) -> Result<(), i32> {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::create_object_with_proto() instead")]
pub unsafe fn JS_NewObjectProtoClass(
    ctx: ContextRef,
    proto_val: JSValue,
    class_id: ClassID,
) -> JSValue {
    unreachable!("unused")
}

/// Instantiate object of specific class
/// Returns New object or Exception
#[deprecated(note = "use ContextRef::create_object() instead")]
pub unsafe fn JS_NewObjectClass(ctx: ContextRef, class_id: ClassID) -> JSValue {
    unreachable!("unused");

    // ctx
    //     .create_object_with_classid(class_id)
    //     .unwrap_or(JSValue::EXCEPTION)
}

/// Create plain object with specified prototype
/// Parameters:
/// - ctx: JavaScript context
/// - proto: Prototype object (can be null)
#[deprecated(note = "use ContextRef::create_base_object_with_proto() instead")]
pub unsafe fn JS_NewObjectProto(ctx: ContextRef, proto: JSValue) -> JSValue {
    JS_NewObjectProtoClass(ctx, proto, JS_CLASS_OBJECT)
}

/// Create basic object using default Object prototype
#[deprecated(note = "use ContextRef::create_base_object() instead")]
pub unsafe fn JS_NewObject(ctx: ContextRef) -> JSValue {
    ctx.create_base_object()
        .map_or(JSValue::EXCEPTION, JSValue::Object)
}

/// Convert JS value to object equivalent
/// Handles primitive wrapping according to ES6 ToObject conversion rules
/// Returns exception for null/undefined inputs.
/// `val` is consumed
#[deprecated(note = "use JSValue::coerce_to_object() instead")]
pub fn JS_ToObject(ctx: ContextRef, val: JSValue) -> JSValue {
    val.objectify(ctx)
        .map_or(JSValue::EXCEPTION, JSValue::Object)
}

#[deprecated(note = "use JSValue::coerce_to_object() instead")]
pub unsafe fn JS_ToObjectFree(ctx: ContextRef, val: JSValue) -> JSValue {
    val.objectify(ctx)
        .map_or(JSValue::EXCEPTION, JSValue::Object)
}

/// Return an Object, JS_NULL or JS_EXCEPTION in case of Proxy object.
/// `obj` keep ref count unchanged after calling.
#[deprecated(note = "use JSValue::prototype() instead")]
pub fn JS_GetPrototype(ctx: ContextRef, obj: &JSValue) -> JSValue {
    obj.prototype(ctx).unwrap_or(JSValue::EXCEPTION)
}

/// `obj` is consumed
#[deprecated(note = "use JSValue::prototype() instead")]
pub fn JS_GetPrototypeFree(ctx: ContextRef, obj: JSValue) -> JSValue {
    JS_GetPrototype(ctx, &obj)
}

// Private fields can be added even on non extensible objects or Proxies
#[deprecated(note = "use JSValue::define_private_field() instead")]
pub unsafe fn JS_DefinePrivateField(
    ctx: ContextRef,
    obj: &JSValue,
    name: JSValue,
    val: JSValue,
) -> i32 {
    obj.define_private_field(ctx, &name, val).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSValue::get_private_field() instead")]
pub unsafe fn JS_GetPrivateField(ctx: ContextRef, obj: JSValue, name: JSValue) -> JSValue {
    obj.get_private_field(ctx, &name)
        .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::set_private_field() instead")]
pub unsafe fn JS_SetPrivateField(
    ctx: ContextRef,
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
    name: AtomRef,
    var_def_type: JSVarDefEnum,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSParseState::define_var() instead")]
pub unsafe fn js_define_var(s: *mut JSParseState, name: AtomRef, tok: i32) -> i32 {
    unreachable!("unused");
    // if (*s).define_var(name, tok, false).is_err() {
    //     -1
    // } else {
    //     0
    // }
}

#[deprecated(note = "use JSParseState::peek_token() instead")]
pub fn peek_token(s: *const JSParseState, no_line_terminator: bool) -> i32 {
    unsafe { (*s).peek_token(no_line_terminator) }
}

/// always returns -1
#[deprecated(note = "use JSParseState::throw_reserved_identifier_error() instead")]
pub unsafe fn js_parse_error_reserved_identifier(s: *mut JSParseState) -> i32 {
    unreachable!("unused");
    // (*s).throw_syntax_error(&format!(
    //     "'{}' is a reserved identifier",
    //     (*s).token.atom().unwrap().to_str(&(*s).ctx.rt()),
    // ));
    // -1
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
    pname: *mut AtomRef,
    plabel: *mut i32,
    pdepth: *mut i32,
    keep: bool,
    tok: i32,
) -> i32 {
    unreachable!("unused")
    // (*s).get_lvalue(popcode, pscope, pname, plabel, pdepth, keep, tok)
    //     .map_or(-1, |_| 0)
}

// name has a live reference. 'is_let' is only used with opcode =
// op_scope_get_var which is never generated by get_lvalue().
#[deprecated(note = "use JSParseState::get_lvalue() instead")]
pub unsafe fn put_lvalue(
    s: *mut JSParseState,
    opcode: i32,
    scope: i32,
    name: AtomRef,
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
    unreachable!("unused");
    // (*s).parse_template(call, argc).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_left_hand_side_expr() instead")]
pub unsafe fn js_parse_left_hand_side_expr(s: *mut JSParseState) -> i32 {
    unreachable!("unused");
    // (*s).parse_left_hand_side_expr().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_expr_parens() instead")]
pub unsafe fn js_parse_expr_paren(s: *mut JSParseState) -> i32 {
    unreachable!("unused");
    // (*s).parse_expr_parens().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_object_literal() instead")]
pub unsafe fn js_parse_object_literal(s: *mut JSParseState) -> i32 {
    unreachable!("unused");
    // (*s).parse_object_literal().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::set_object_name() instead")]
pub unsafe fn set_object_name(s: *mut JSParseState, name: Atom) {
    unreachable!("unused");
    // (*s).set_object_name(name);
}

#[deprecated(note = "use JSParseState::next_token() instead")]
pub unsafe fn next_token(s: *mut JSParseState) -> i32 {
    unreachable!("unused");
    // if (*s).next_token().is_err() { -1 } else { 0 }
}

// #[deprecated(note = "use JSParseState::free_token() instead")]
// pub unsafe fn free_token(s: *mut JSParseState, token: *mut Token) {
//     debug_assert_eq!(&mut (*s).token as *mut Token, token);
//     (*s).free_token();
// }

// #[deprecated(note = "use JSParseState::token_is_ident() instead")]
// pub fn token_is_ident(tok: i32) -> bool {
//     /* Accept keywords and reserved words as property names */
//     tok == TOK_IDENT || (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD).contains(&tok)
// }

#[deprecated(note = "use JSParseState::token_is_pseudo_keyword() instead")]
pub unsafe fn token_is_pseudo_keyword(s: *mut JSParseState, atom: AtomRef) -> bool {
    unreachable!("unused")
    // (*s).token_is_pseudo_keyword(atom)
}

#[deprecated(note = "use AtomRef::to_string_or_symbol() instead")]
pub unsafe fn __JS_AtomToValue(ctx: ContextRef, atom: AtomRef, force_string: bool) -> JSValue {
    unreachable!("unused")
    // atom.to_string_or_symbol(ctx, force_string)
    //     .unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use AtomRef::to_string_or_symbol() instead")]
pub unsafe fn JS_AtomToValue(ctx: ContextRef, atom: AtomRef) -> JSValue {
    __JS_AtomToValue(ctx, atom, false)
}

#[deprecated(note = "use AtomRef::new() instead")]
pub unsafe fn JS_NewAtomLen(ctx: ContextRef, str: *const u8, len: usize) -> AtomRef {
    unreachable!("unused");
}

#[deprecated(note = "use AtomRef::new() instead")]
pub unsafe fn JS_NewAtom(ctx: ContextRef, str: &CStr) -> AtomRef {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::create_atom_string() instead")]
pub unsafe fn JS_NewAtomString(ctx: ContextRef, str: &str) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use AtomRef::from_u32() instead")]
pub unsafe fn JS_NewAtomUInt32(ctx: ContextRef, n: u32) -> AtomRef {
    unreachable!("unused");
}

#[deprecated(note = "use AtomRef::from_i64() instead")]
pub unsafe fn JS_NewAtomInt64(ctx: ContextRef, n: i64) -> AtomRef {
    unreachable!("unused");
}

#[deprecated(note = "use AsyncFuncState::free_frame() instead")]
pub unsafe fn async_func_free_frame(rt: *mut RuntimeImpl, s: *mut AsyncFuncState) {
    unreachable!("unused");
}

/* return Err() if exception (Proxy object only) or TRUE/FALSE */
#[deprecated(note = "use JSValue::is_extensible() instead")]
pub unsafe fn JS_IsExtensible(ctx: ContextRef, obj: &JSValue) -> Result<bool, i32> {
    obj.is_extensible(ctx)
}

/* Return -1 if exception,
FALSE if the property does not exist, TRUE if it exists. If TRUE is
returned, the property descriptor 'desc' is filled present. */
#[deprecated(note = "use JSValue::get_own_property() or ObjectRef::get_own_property() instead")]
pub unsafe fn JS_GetOwnPropertyInternal(
    ctx: ContextRef,
    desc: *mut JSPropertyDescriptor,
    p: &JSValue,
    prop: AtomRef,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "JSValue::get_own_property() instead")]
pub unsafe fn JS_GetOwnProperty(
    ctx: ContextRef,
    desc: *mut JSPropertyDescriptor,
    obj: JSValue,
    prop: AtomRef,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::copy_data_properties_from() instead")]
pub unsafe fn JS_CopyDataProperties(
    ctx: ContextRef,
    target: JSValue,
    source: JSValue,
    excluded: JSValue,
    setprop: bool,
) -> Result<(), i32> {
    target.copy_data_properties_from(ctx, &source, &excluded, setprop)
}

#[deprecated(note = "use JSValue::ordinary_instance_of() instead")]
pub unsafe fn JS_OrdinaryIsInstanceOf(
    ctx: ContextRef,
    val: &JSValue,
    obj: &JSValue,
) -> Result<bool, i32> {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::instance_of() instead")]
pub unsafe fn JS_IsInstanceOf(ctx: ContextRef, val: &JSValue, obj: &JSValue) -> Result<bool, i32> {
    obj.instance_of(ctx, val)
}

#[deprecated(note = "use JSValue::type_of() instead")]
pub fn js_operator_typeof(ctx: ContextRef, op1: &JSValue) -> AtomRef {
    op1.type_of()
}

/// Access an Array's internal JSValue array if available
#[deprecated(note = "use JSValue::fast_array_values_mut() instead")]
pub unsafe fn js_get_fast_array(
    ctx: ContextRef,
    mut obj: JSValue,
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
pub unsafe fn js_is_fast_array(ctx: ContextRef, obj: JSValue) -> bool {
    obj.is_fast_array()
}

/// Preconditions: 'p' must be of class JS_CLASS_ARRAY, p is fast_array and p is extensible.
/// returns Err if fail, true if append successfully, false if can't append due to read-only but flags indicates don't throw exception.
/// Note: `val` is consumed.
#[deprecated(note = "use JSObjectInner::add_fast_array_element() instead")]
pub unsafe fn add_fast_array_element(
    ctx: ContextRef,
    p: *mut JSValue,
    val: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("unused");
}

// return Err if exception (proxy case) or TRUE/FALSE
#[deprecated(note = "use JSValue::is_array() instead")]
pub unsafe fn JS_IsArray(ctx: ContextRef, val: &JSValue) -> Result<bool, i32> {
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
#[deprecated(note = "use ObjectRef::set_array_length() instead")]
pub unsafe fn set_array_length(ctx: ContextRef, p: *mut JSValue, val: JSValue, flags: u32) -> i32 {
    unreachable!("unused")
    // (*p).object_unchecked()
    //     .set_array_length2(ctx, val, flags)
    //     .map_or(-1, |b| b as i32)
}

/// `val` is duplicated inside
#[deprecated(note = "use ObjectRef::create_property() instead")]
pub unsafe fn JS_CreateProperty(
    ctx: ContextRef,
    p: *mut JSValue,
    prop: AtomRef,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::has_property() instead")]
pub unsafe fn JS_HasProperty(ctx: ContextRef, obj: JSValue, prop: AtomRef) -> Result<bool, i32> {
    unreachable!("ununsed");
    // obj.has_property(ctx, prop)
}

#[deprecated(note = "use JSProperty::free() instead")]
pub unsafe fn free_property(rt: *mut RuntimeImpl, pr: *mut JSProperty, prop_flags: u32) {
    unreachable!("use JSProperty::free() instead");
}

/// Finds a property in an object by its atom name
#[deprecated(note = "use ObjectRef::find_own_property1() instead")]
pub unsafe fn find_own_property1(p: *mut JSValue, atom: AtomRef) -> *mut ShapeProperty {
    unreachable!("unused");
}

/// Finds a property and its value in an object by atom name
/// Sets ppr to the property value pointer if found, null otherwise
/// Returns the property shape pointer if found, null otherwise
#[inline(always)]
#[deprecated(note = "use ObjectRef::find_shape_property() instead")]
pub unsafe fn find_own_property(
    ppr: *mut *mut JSProperty,
    p: *mut JSValue,
    atom: AtomRef,
) -> *mut ShapeProperty {
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
#[deprecated(note = "use native callback instead. see ContextRef::create_native_function()")]
pub unsafe fn JS_NewCFunction3(
    ctx: ContextRef,
    func: JSCFunction,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
    proto_val: JSValue,
) -> JSValue {
    unreachable!("unused");

    // match ctx.create_object_with_proto(
    //     proto_val,
    //     ObjectPayload::C_FUNCTION(NFuncPayload {
    //         realm: ctx.downgrade(),
    //         handler: NativeFunctionType::CFunc(func),
    //         length,
    //         magic,
    //     }),
    // ) {
    //     Ok(func_obj) => {
    //         func_obj.set_flag_constructor(matches!(
    //             cproto,
    //             JSCFunctionEnum::Constructor
    //                 | JSCFunctionEnum::ConstructorMagic
    //                 | JSCFunctionEnum::ConstructorOrFunc
    //                 | JSCFunctionEnum::ConstructorOrFuncMagic
    //         ));

    //         // define properties on func_obj: length and name.
    //         let name_atom = Atom::new(ctx.rt_mut(), name).unwrap();

    //         js_function_set_properties(ctx, &func_obj, name_atom, length as i32);
    //         JS_FreeAtom(ctx, name_atom);

    //         func_obj.into()
    //     }
    //     Err(_) => JSValue::EXCEPTION,
    // }
}

/* Note: at least 'length' arguments will be readable in 'argv' */
#[deprecated(note = "use native callback instead. see ContextRef::create_native_function()")]
#[inline(always)]
pub fn JS_NewCFunction2(
    ctx: ContextRef,
    func: JSCFunction,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
) -> JSValue {
    unreachable!("unused");
    // unsafe {
    //     JS_NewCFunction3(
    //         ctx,
    //         func,
    //         name,
    //         length,
    //         cproto,
    //         magic,
    //         ctx.function_proto.clone(),
    //     )
    // }
}

#[inline(always)]
#[deprecated(note = "use native callback instead. see ContextRef::create_native_function()")]
pub fn JS_NewCFunction(ctx: ContextRef, func: JSCFunction, name: &str, length: u8) -> JSValue {
    JS_NewCFunction2(ctx, func, name, length, JSCFunctionEnum::Generic, 0)
}

#[inline(always)]
#[deprecated(note = "use native callback instead. see ContextRef::create_native_function()")]
pub fn JS_NewCFunctionMagic(
    ctx: ContextRef,
    func: JSCFunctionMagic,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
) -> JSValue {
    unsafe { JS_NewCFunction2(ctx, std::mem::transmute(func), name, length, cproto, magic) }
}

/* return NULL without exception if not a function or no bytecode */
#[deprecated(note = "use ObjectPayload::func() to get Bytecode")]
pub fn JS_GetFunctionBytecode(val: &JSValue) -> *mut Bytecode {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::ensure_function() instead")]
pub unsafe fn check_function(ctx: ContextRef, obj: JSValue) -> Result<(), i32> {
    obj.ensure_function(ctx)?;
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
#[deprecated(note = "use ContextRef::define_global_var_checked() instead")]
pub fn JS_CheckDefineGlobalVar(ctx: ContextRef, prop: AtomRef, flags: u32) -> i32 {
    unreachable!("unused");
}

// def_flags is (0, DEFINE_GLOBAL_LEX_VAR) |
//   JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE
// XXX: could support exotic global object.
#[deprecated(note = "use JSontext::define_global_var() instead")]
pub fn JS_DefineGlobalVar(ctx: ContextRef, prop: AtomRef, def_flags: u32) -> i32 {
    unreachable!("unused");
}

/// 'def_flags' is 0 or JS_PROP_CONFIGURABLE.
/// `func` is not consumed.
/// XXX: could support exotic global object.
#[deprecated(note = "use ContextRef::define_global_function() instead")]
pub fn JS_DefineGlobalFunction(
    ctx: ContextRef,
    prop: AtomRef,
    func: &JSValue,
    def_flags: u32,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::get_global_var() instead")]
pub fn JS_GetGlobalVar(ctx: ContextRef, prop: AtomRef, throw_ref_error: bool) -> JSValue {
    unreachable!("unused");
}

// construct a reference to a global variable
#[deprecated(note = "use ContextImpl::build_global_varref() instead")]
pub unsafe fn JS_GetGlobalVarRef(ctx: ContextRef, prop: AtomRef, sp: *mut JSValue) -> i32 {
    unreachable!("unused");
}

/// used for strict variable access: test if the variable exists
#[deprecated(note = "ContextRef::check_global_var() instead")]
pub fn JS_CheckGlobalVar(ctx: ContextRef, prop: AtomRef) -> i32 {
    unreachable!("unused");
}

// flag = 0: normal variable write
// flag = 1: initialize lexical variable
// flag = 2: normal variable write, strict check was done before
#[deprecated(note = "use ContextRef::set_global_var() instead")]
pub fn JS_SetGlobalVar(ctx: ContextRef, prop: AtomRef, val: JSValue, flag: u32) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use AsyncFuncState::resume() instead")]
pub fn async_func_resume(ctx: ContextRef, af: &AsyncFuncState) -> JSValue {
    af.resume(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use AsyncFunction::class_call() instead")]
pub fn js_async_function_call(
    ctx: ContextRef,
    func_obj: &JSValue,
    this_obj: &JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult {
    unreachable!("unused");
}

// Note: the integer value is saturated to 32 bits
#[deprecated(note = "use JSValue::to_i32_saturating() instead")]
pub unsafe fn JS_ToInt32SatFree(ctx: ContextRef, pres: *mut i32, mut val: JSValue) -> i32 {
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
#[deprecated(note = "use JSValue::to_i32_saturating() instead")]
pub unsafe fn JS_ToInt32Sat(ctx: ContextRef, pres: *mut i32, val: &JSValue) -> i32 {
    JS_ToInt32SatFree(ctx, pres, val.clone())
}

/// val is not consumed
#[deprecated(note = "use JSValue::to_i32_clamp() instead")]
pub unsafe fn JS_ToInt32Clamp(
    ctx: ContextRef,
    pres: *mut i32,
    val: &JSValue,
    min: i32,
    max: i32,
    min_offset: i32,
) -> i32 {
    if let Ok(mut res) = val.to_i32_saturating(ctx) {
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

#[deprecated(note = "use JSValue::to_number() instead")]
pub unsafe fn JS_ToNumberFree(ctx: ContextRef, val: JSValue) -> JSValue {
    val.to_number(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::to_numeric() instead")]
pub unsafe fn JS_ToNumericFree(ctx: ContextRef, val: JSValue) -> JSValue {
    val.to_numeric(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::to_numeric() instead")]
pub unsafe fn JS_ToNumeric(ctx: ContextRef, val: JSValue) -> JSValue {
    val.to_numeric(ctx).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::to_i64_saturating() instead")]
pub unsafe fn JS_ToInt64SatFree(ctx: ContextRef, pres: *mut i64, mut val: JSValue) -> i32 {
    unreachable!("unused");
}

/// val is not consumed
#[deprecated(note = "use JSValue::to_i64_saturating() instead")]
pub unsafe fn JS_ToInt64Sat(ctx: ContextRef, pres: *mut i64, val: &JSValue) -> i32 {
    unreachable!("unused");
}

/// val is not consumed
#[deprecated(note = "use JSValue::to_i64_clamp() instead")]
pub unsafe fn JS_ToInt64Clamp(
    ctx: ContextRef,
    pres: *mut i64,
    val: &JSValue,
    min: i64,
    max: i64,
    neg_offset: i64,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::throw_range_error() instead")]
pub fn JS_ThrowRangeError(ctx: ContextRef, message: &str) {
    ctx.throw_error(ErrorKind::Range, message);
}

/// convert a value to a length between 0 and MAX_SAFE_INTEGER.
#[deprecated(note = "use JSValue::to_length() instead")]
pub unsafe fn JS_ToLengthFree(ctx: ContextRef, val: JSValue) -> Result<i64, i32> {
    unreachable!("unused");
}

/// returns value of int, float64, nan, exception
#[deprecated(note = "use JSValue::do_coerce_number_free() instead")]
pub unsafe fn JS_ToNumberHintFree(
    ctx: ContextRef,
    val: JSValue,
    flag: JSToNumberHintEnum,
) -> JSValue {
    unreachable!("unused");
    // JSValue::do_coerce_to_number_free(ctx, val, flag).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSValue::to_array_index() instead")]
pub fn JS_ToIndex(ctx: ContextRef, val: &JSValue) -> Result<i64, i32> {
    val.to_array_index(ctx)
}

#[deprecated(note = "use JSValue::from_i32() instead")]
pub fn JS_NewInt32(ctx: ContextRef, val: i32) -> JSValue {
    JSValue::Int(val)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_u32() instead")]
pub fn JS_NewUint32(ctx: ContextRef, val: u32) -> JSValue {
    JSValue::from(val)
}

#[deprecated(note = "use JSValue::from_i64() instead")]
#[inline(always)]
pub fn JS_NewInt64(ctx: ContextRef, val: i64) -> JSValue {
    JSValue::from(val)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_f64_as_is() instead")]
pub fn __JS_NewFloat64(ctx: ContextRef, d: f64) -> JSValue {
    JSValue::Float64(d)
}

#[inline(always)]
#[deprecated(note = "use JSValue::from_f64() instead")]
pub fn JS_NewFloat64(ctx: ContextRef, d: f64) -> JSValue {
    JSValue::from(d)
}

/* Note: can return an exception (-1) */
#[deprecated(note = "use JSValue::is_integer() instead")]
pub unsafe fn JS_NumberIsInteger(ctx: ContextRef, val: JSValue) -> i32 {
    val.is_integral(ctx).map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::to_i32() instead")]
pub unsafe fn JS_ToInt32Free(ctx: ContextRef, pres: *mut i32, val: JSValue) -> i32 {
    val.to_i32(ctx).map_or(-1, |v| {
        *pres = v;
        0
    })
}

/// `val` is not consumed
#[deprecated(note = "use JSValue::to_i32() instead")]
pub unsafe fn JS_ToInt32(ctx: ContextRef, val: &JSValue) -> Result<i32, i32> {
    let mut n = 0;
    if JS_ToInt32Free(ctx, &mut n, val.clone()) == -1 {
        Err(-1)
    } else {
        Ok(n)
    }
}

#[deprecated(note = "use JSValue::to_u32() instead")]
pub unsafe fn JS_ToUint32(ctx: ContextRef, val: &JSValue) -> Result<u32, i32> {
    val.to_u32(ctx)
}

#[inline(always)]
#[deprecated(note = "use JSValue::to_u32() instead")]
pub unsafe fn JS_ToUint32Free(ctx: ContextRef, val: JSValue) -> Result<u32, i32> {
    val.to_u32(ctx)
}

#[deprecated(note = "use JSValue::coerce_to_u8_clamp() instead")]
pub unsafe fn JS_ToUint8ClampFree(ctx: ContextRef, pres: *mut i32, val: JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::coerce_to_u8_clamp() instead")]
pub unsafe fn JS_ToUint8Clamp(ctx: ContextRef, pres: *mut i32, val: &JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::to_f64() instead")]
pub unsafe fn JS_ToFloat64Free(ctx: ContextRef, pres: *mut f64, val: JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::to_f64() instead")]
pub unsafe fn JS_ToFloat64(ctx: ContextRef, pres: *mut f64, val: &JSValue) -> i32 {
    JS_ToFloat64Free(ctx, pres, val.clone())
}

/// Note: `val` is consumed
#[deprecated(note = "use JSValue::to_f64() instead")]
pub unsafe fn __JS_ToFloat64Free(ctx: ContextRef, pres: *mut f64, val: JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "unused")]
pub unsafe fn free_function_bytecode(
    rt: *mut RuntimeImpl,
    mut b: NonNull<Bytecode>,
) -> Option<NonNull<Bytecode>> {
    unreachable!("unused")
}

/// Make function closure with bytecode and captured variables.
/// Returns closure function object.
#[deprecated(note = "use Bytecode::make_closure() instead")]
pub fn js_closure(
    ctx: ContextRef,
    bc: &Bytecode,
    cur_var_refs: &[JSVarRef],
    parent_frame: Option<&mut StackFrame>,
) -> JSValue {
    bc.make_closure(ctx, cur_var_refs, parent_frame, None)
        .map_or(JSValue::EXCEPTION, |x| x.into())
}

#[deprecated(note = "use Bytecode::make_closure_to() instead")]
pub(crate) unsafe fn js_closure2(
    ctx: ContextRef,
    func_obj: JSValue,
    bc: Bytecode,
    cur_var_refs: &[JSVarRef],
    parent_frame: Option<&mut StackFrame>,
) -> JSValue {
    unreachable!("unused")
    // bc.make_closure_to(
    //     ctx,
    //     func_obj.as_object_unchecked().clone(),
    //     cur_var_refs,
    //     parent_frame,
    // )
    // .map_or(JSValue::EXCEPTION, JSValue::Object)
}

#[deprecated(note = "use JSFunctionDef::add_closure_var() instead")]
/// returns added closure var index, or Err for error.
pub unsafe fn add_closure_var(
    ctx: ContextRef,
    s: &mut JSFunctionDef,
    is_local: bool,
    is_arg: bool,
    var_idx: u16,
    var_name: AtomRef,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    unreachable!("unused");
    // s.add_closure_var(
    //     ctx, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    // )
}

/// `fun_obj` is consumed, `this_obj` is not consumed.
/// `fun_obj` could be Bytecode or Module.
#[deprecated(note = "unused")]
pub unsafe fn JS_EvalFunctionInternal(
    ctx: ContextRef,
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
pub unsafe fn JS_EvalFunction(ctx: ContextRef, fun_obj: &JSValue) -> JSValue {
    return JS_EvalFunctionInternal(ctx, fun_obj, &ctx.global_obj.clone(), &[], None);
}

#[deprecated(note = "use JSFunctionDef::add_func_var() instead")]
pub unsafe fn add_func_var(ctx: ContextRef, fd: *mut JSFunctionDef, name: AtomRef) -> i32 {
    unreachable!("unused");
    // (*fd).add_func_var(ctx, name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_var_this() instead")]
pub unsafe fn add_var_this(ctx: ContextRef, fd: *mut JSFunctionDef) -> i32 {
    (*fd).add_var_this(ctx).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_arguments(ctx, false) instead")]
pub unsafe fn add_arguments_var(ctx: ContextRef, fd: *mut JSFunctionDef) -> i32 {
    (*fd).add_arguments(ctx, false).map_or(-1, |x| x as i32)
}

// add an argument definition in the argument scope. Only needed when
// "eval()" may be called in the argument scope. Return 0 if OK.
#[deprecated(note = "use JSFunctionDef::add_arguments() instead")]
pub unsafe fn add_arguments_arg(ctx: ContextRef, fd: &mut JSFunctionDef) -> i32 {
    (*fd).add_arguments(ctx, true).map_or(-1, |x| x as i32)
}

/// find closure var by name, returning index of the closure var
#[deprecated(note = "use JSFunctionDef::find_closure_var() instead")]
pub unsafe fn find_closure_var(
    ctx: ContextRef,
    s: &mut JSFunctionDef,
    var_name: AtomRef,
) -> Option<u16> {
    unreachable!("unused");
}

// 'fd' must be a parent of 's'. Create in 's' a closure referencing a
// local variable (is_local = TRUE) or a closure (is_local = FALSE) in
// 'fd'
#[deprecated(note = "use JSFunctionDef::get_closure_var2() instead")]
pub unsafe fn get_closure_var2(
    ctx: ContextRef,
    s: &mut JSFunctionDef,
    fd: *const JSFunctionDef,
    is_local: bool,
    is_arg: bool,
    var_idx: u16,
    var_name: AtomRef,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    unreachable!("unused");
    // s.get_closure_var2(
    //     ctx, fd, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    // )
}

#[deprecated(note = "use JSFunctionDef::get_closure_var2() instead")]
pub unsafe fn get_closure_var(
    ctx: ContextRef,
    s: &mut JSFunctionDef,
    fd: *const JSFunctionDef,
    is_arg: bool,
    var_idx: u16,
    var_name: AtomRef,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    unreachable!("unused");
    // s.get_closure_var2(
    //     ctx, fd, true, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    // )
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
    (*s).parse_destructuring_element(tok, is_arg, hasval, has_ellipsis, allow_initializer, false)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSParseState::parse_destructuring_var() instead")]
pub unsafe fn js_parse_destructuring_var(s: *mut JSParseState, tok: i32, is_arg: bool) -> AtomRef {
    unreachable!("unused")
    // (*s).parse_destructuring_var(tok, is_arg)
    //     .unwrap_or(JS_ATOM_NULL)
}

#[deprecated(note = "use JSValue::install_native_field_entry() instead")]
pub unsafe fn JS_SetPropertyFunctionList(
    ctx: ContextRef,
    obj: &JSValue,
    tab: *const FieldDef,
    len: usize,
) {
    unreachable!("unused");
    // obj.install_property_entries(ctx, std::slice::from_raw_parts(tab, len))
    //     .unwrap();
}

#[deprecated(note = "use JSValue::install_field_defs() instead")]
pub unsafe fn JS_InstantiateFunctionListItem(
    ctx: ContextRef,
    obj: &JSValue,
    atom: AtomRef,
    e: *const FieldDef,
) -> i32 {
    unreachable!("unused");
}

/* Note: the property value is not initialized. Return NULL if memory
error. */
#[deprecated(note = "use ObjectRef::add_own_property() instead")]
pub unsafe fn add_property(
    ctx: ContextRef,
    p: *mut JSValue,
    prop: AtomRef,
    prop_flags: u32,
) -> *mut JSProperty {
    unreachable!("unused")
}

#[deprecated(note = "use AGenPayload::resume_next() instead")]
pub unsafe fn js_async_generator_resume_next(ctx: ContextRef, s: *mut AGenPayload) {
    (*s).resume_next(ctx);
}

#[deprecated(note = "use AGenPayload::complete() instead")]
pub unsafe fn js_async_generator_complete(ctx: ContextRef, s: *mut AGenPayload) {
    (*s).complete();
}

/// `value` is not consumed.
#[deprecated(note = "use AGenPayload::resolve() instead")]
pub unsafe fn js_async_generator_resolve(
    ctx: ContextRef,
    s: *mut AGenPayload,
    value: JSValue,
    done: bool,
) {
    (*s).resolve(ctx, value, done);
}

#[inline(always)]
#[deprecated(note = "use AGenPayload::reject() instead")]
pub unsafe fn js_async_generator_reject(ctx: ContextRef, s: *mut AGenPayload, exception: JSValue) {
    (*s).reject(ctx, exception);
}

#[deprecated(note = "use AGenPayload::resolve_or_reject() instead")]
pub unsafe fn js_async_generator_resolve_or_reject(
    ctx: ContextRef,
    s: *mut AGenPayload,
    result: JSValue,
    is_reject: bool,
) {
    unreachable!("unnused")
    // (*s).resolve_or_reject(ctx, &result, is_reject);
}

#[cfg(feature = "dump_bytecode")]
#[deprecated(note = "use Bytecode::dump() instead")]
pub fn js_dump_function_bytecode(ctx: ContextRef, bc: &Bytecode) {
    bc.dump(ctx);
}

#[deprecated(note = "use JSFunctionDef::compute_stack_size() instead")]
pub unsafe fn compute_stack_size(ctx: ContextRef, fd: *mut JSFunctionDef) -> Result<u16, i32> {
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

#[deprecated(note = "use symbol_to_atom() instead")]
pub(crate) fn js_symbol_to_atom(val: &JSValue) -> AtomRef {
    unreachable!("unused");
}

/// `atom` must be a non-numeric string atom
#[deprecated(note = "use ContextRef::create_symbol_from_atom() instead")]
pub unsafe fn JS_NewSymbolFromAtom(ctx: ContextRef, atom: AtomRef, atom_kind: AtomKind) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::create_symbol() instead")]
pub fn JS_NewSymbol(ctx: ContextRef, p: StringImpl, atom_kind: AtomKind) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::to_atom() instead")]
pub fn JS_ValueToAtom(ctx: ContextRef, val: &JSValue) -> AtomRef {
    unreachable!("unused");
}

/* return < 0, 0 or > 0 */
#[deprecated(note = "use StringBuf::cmp() instead")]
pub unsafe fn js_string_compare(ctx: ContextRef, p1: &StringImpl, p2: &StringImpl) -> i32 {
    match p1.content().cmp(p2.content()) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

#[deprecated(note = "use StringImpl::is_empty()")]
pub fn JS_IsEmptyString(v: &JSValue) -> bool {
    if let JSValue::String(s) = v {
        s.is_empty()
    } else {
        false
    }
}

#[deprecated(note = "use ObjectRef::shape_prepare_update() instead")]
pub unsafe fn js_shape_prepare_update(
    ctx: ContextRef,
    p: *mut ObjectRef,
    pprs: *mut *mut ShapeProperty,
) -> i32 {
    unreachable!("unused");
}

/// check if token is reserved keyword or not.
///  if it is a keyword, convert keyword token.
#[deprecated(note = "use JSParseState::update_token_ident() instead")]
pub fn update_token_ident(s: &mut JSParseState) {
    s.update_token_ident()
}

/// Prepare a module to be executed by resolving all the imported variables.
#[deprecated(note = "use JSModuleDef::link() instead")]
pub fn js_link_module(ctx: ContextRef, m: &ModuleDef) -> i32 {
    unreachable!("unused")
    // m.link(ctx).map_or(-1, |_| 0)
}

// Prepare a module to be executed by resolving all the imported variables.
#[deprecated(note = "use ModuleDef::inner_module_linking() instead")]
pub unsafe fn js_inner_module_linking(
    ctx: ContextRef,
    m: &ModuleDef, // FIXME: use ModuleInner instead
    pstack_top: &mut Option<ModuleDef>,
    index: i32,
) -> i32 {
    unreachable!("unused")
}

/// find the loaded module by module name
#[deprecated(note = "use ContextRef::find_loaded_module() instead")]
pub fn js_find_loaded_module(ctx: ContextRef, name: AtomRef) -> Option<ModuleDef> {
    unreachable!("unused");
}

// Load all the required modules for module 'm'
#[deprecated(note = "use ContextRef::resolve_module() instead")]
pub unsafe fn js_resolve_module(ctx: ContextRef, m: &ModuleDef) -> i32 {
    ctx.resolve_module(m).map_or(-1, |_| 0)
}

#[deprecated(note = "unused")]
pub unsafe fn js_resolve_export_throw_error(
    ctx: ContextRef,
    res: JSResolveResultEnum,
    m: &ModuleDef,
    export_name: AtomRef,
) {
    unreachable!("unused")
}

#[deprecated(note = "not used")]
pub unsafe fn add_resolve_entry(
    ctx: ContextRef,
    lst: &mut Vec<*mut c_void>, // JSResolveEntry
    m: &ModuleDef,
    name: AtomRef,
) -> Result<(), i32> {
    unreachable!("unused")
}

/// add dependent module
#[deprecated(note = "use JSModuleDef::add_dependency() instead")]
pub unsafe fn add_req_module_entry(ctx: ContextRef, m: &ModuleDef, module_name: AtomRef) -> i32 {
    unreachable!("unused")
}

#[deprecated(note = "use ModuleDef::resolve_export_inner() instead")]
pub unsafe fn js_resolve_export1(
    ctx: ContextRef,
    pmodule: *mut *mut ModuleDefImpl,
    pme: *mut *mut ExportEntry,
    m: &ModuleDef,
    export_name: AtomRef,
    s: *mut std::ffi::c_void, // JSResolveState,
) -> JSResolveResultEnum {
    unreachable!("unused");
}

// If the return value is JS_RESOLVE_RES_FOUND, return the module
// (*pmodule) and the corresponding local export entry
// (*pme). Otherwise return (NULL, NULL)
#[deprecated(note = "use ModuleDef::resolve_export() instead")]
pub unsafe fn js_resolve_export(
    ctx: ContextRef,
    pmodule: *mut *mut ModuleDefImpl,
    pme: *mut *mut ExportEntry,
    m: &mut ModuleDef,
    export_name: AtomRef,
) -> JSResolveResultEnum {
    unreachable!("unused");
}

#[deprecated(note = "use JSParseState::parse_export() instead")]
pub unsafe fn js_parse_export(s: *mut JSParseState) -> i32 {
    (*s).parse_export().map_or(-1, |_| 0)
}

#[repr(C)]
#[deprecated(note = "use Vec<JSResultEntry> instead")]
pub struct JSResolveState {
    pub array: *mut c_void, // JSResolveEntry
    pub size: i32,
    pub count: i32,
}

#[deprecated(note = "use JSResolveState::find()")]
pub unsafe fn find_resolve_entry(
    s: *mut JSResolveState,
    m: &ModuleDef,
    name: AtomRef,
) -> Option<i32> {
    unreachable!("unused");
}

#[deprecated(note = "use ModuleDef::add_export_internal() or JSParseState::add_module_export()")]
unsafe fn add_export_entry2(
    ctx: ContextRef,
    s: *mut JSParseState,
    m: &ModuleDef,
    local_name: AtomRef,
    export_name: AtomRef,
    export_type: ExportType,
) -> Result<(), i32> {
    unreachable!("unused");
}

/// `local_name`, `export_name` are not consumed.
#[deprecated(note = "use JSParseState::add_module_export() instead")]
pub fn add_export_entry<'a>(
    s: &mut JSParseState,
    m: &ModuleDef,
    local_name: AtomRef,
    export_name: AtomRef,
    export_type: ExportType,
) -> Result<(), i32> {
    let ctx = s.ctx.clone();
    unsafe { add_export_entry2(&ctx, s, m, local_name, export_name, export_type) }
}

#[deprecated(note = "use JSParseState::add_import() instead")]
pub unsafe fn add_import(
    s: *mut JSParseState,
    m: &ModuleDef,
    local_name: AtomRef,
    import_name: AtomRef,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use ContextRef::create_array_buffer_ex() instead")]
pub unsafe fn js_array_buffer_constructor3(
    ctx: ContextRef,
    new_target: JSValue,
    byte_length: usize,
    class_id: ClassID,
    buf: *mut u8,
    free_func: Option<JSFreeArrayBufferDataFunc>,
    opaque: *mut c_void,
    alloc_flag: bool,
) -> JSValue {
    unreachable!()
}

/// the callback to free ArrayBufObjPayload.
/// - opaque: ArrayBufObjPayload.opaque field
/// - data: ArrayBufObjPayload.data field
/// - data_length: ArrayBufObjPayload.byte_length field
#[deprecated(note = "not used")]
pub unsafe fn js_array_buffer_free(
    rt: *mut RuntimeImpl,
    opaque: *mut c_void,
    data: *mut u8,
    data_length: usize,
) {
    // (data, data_length) is actually raw pointer to Box<[u8]>,
    // restore then drop it.
    let s = std::ptr::slice_from_raw_parts_mut(data, data_length);
    drop(Box::from_raw(s));
}

#[deprecated(note = "use ArrayBuffer::create() instead")]
pub unsafe fn js_array_buffer_constructor2(
    ctx: ContextRef,
    new_target: &JSValue,
    byte_length: usize,
    class_id: ClassID,
) -> JSValue {
    unreachable!("unused")
}

#[deprecated(note = "not used")]
pub unsafe fn js_array_buffer_constructor1(
    ctx: ContextRef,
    new_target: &JSValue,
    byte_length: usize,
) -> JSValue {
    unreachable!("unused")
}

// if the property is an expression, name = JS_ATOM_NULL
#[deprecated(note = "use JSParseState::parse_property_name() instead")]
pub unsafe fn js_parse_property_name(
    s: *mut JSParseState,
    pname: *mut AtomRef,
    allow_method: bool,
    allow_var: bool,
    allow_private: bool,
) -> i32 {
    unreachable!("unused")
    // match (*s).parse_property_name(allow_method, allow_var, allow_private) {
    //     Ok((name, prop)) => {
    //         *pname = name;
    //         prop
    //     }
    //     Err(_) => {
    //         *pname = JS_ATOM_NULL;
    //         -1
    //     }
    // }
}

// XXX: improve speed with early bailout
// XXX: no longer works if regexps are present. Could use previous
//   regexp parsing heuristics to handle most cases
#[deprecated(note = "use JSParseState::parse_skip_parens_token() instead")]
pub unsafe fn js_parse_skip_parens_token(
    s: *mut JSParseState,
    pbits: *mut i32,
    no_line_terminator: bool,
) -> i32 {
    (*s).parse_skip_parens_token(pbits.as_mut().unwrap(), no_line_terminator)
        .unwrap_or(-1)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_u8(s: *mut JSParseState, val: u8) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_u8(val)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_u16(s: *mut JSParseState, val: u16) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_u16(val)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_u32(s: *mut JSParseState, val: u32) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_u32(val)
}

/// emit an atom, increase ref_count
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_atom_dup(s: *mut JSParseState, name: AtomRef) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_atom_dup(name)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
unsafe fn emit_op_u8(s: *mut JSParseState, op: u8) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_op_u8(op, (*s).last_line_num)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_op(s: *mut JSParseState, op: OpCode) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_op(op, (*s).last_line_num)
}

/* don't update the last opcode and don't emit line number info */
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_label_raw(s: *mut JSParseState, label: i32) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_label_raw(label)
}

/* return the label ID offset */
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_label(s: *mut JSParseState, label: i32) -> i32 {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_label(label, (*s).last_line_num)
}

/* return label or -1 if dead code */
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_goto(s: *mut JSParseState, opcode: OpCode, label: i32) -> i32 {
    unreachable!("unused");
    // (*s).cur_func_mut()
    //     .emit_goto(opcode, label, (*s).last_line_num)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_push_const(s: *mut JSParseState, val: JSValue, as_atom: bool) -> i32 {
    unreachable!("unused");
    // (*s).cur_func_mut()
    //     .emit_push_const(val, as_atom, (*s).last_line_num)
    //     .map_or(-1, |_| 0)
}

/* initialize the class fields, called by the constructor. Note:
super() can be called in an arrow function, so <this> and
<class_fields_init> can be variable references */
#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_class_field_init(s: *mut JSParseState) {
    unreachable!("unused");
    // (*s).cur_func_mut()
    //     .emit_class_field_init((*s).last_line_num)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_op_temp(s: *mut JSParseState, op: OpCodeTemp) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_op_temp(op, (*s).last_line_num)
}

#[deprecated(note = "use JSFunctionDef::emit_XXX() instead")]
pub unsafe fn emit_return(s: *mut JSParseState, hasval: bool) {
    unreachable!("unused");
    // (*s).cur_func_mut().emit_return(hasval, (*s).last_line_num)
}

#[deprecated(note = "use JSParseState::emit_break() instead")]
pub unsafe fn emit_break(s: *mut JSParseState, name: AtomRef, is_continue: bool) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSParseState::emit_spread_code() instead")]
pub unsafe fn js_emit_spread_code(s: *mut JSParseState, depth: i32) {
    (*s).emit_spread_code(depth);
}

#[deprecated(note = "use JSParseState::emit_class_init_start() instead")]
pub unsafe fn emit_class_init_start(
    s: *mut JSParseState,
    cf: *mut ClassFieldsDef,
) -> Result<(), i32> {
    unreachable!("unused");
    // (*s).emit_class_init_start(cf.as_mut().unwrap())
}

#[deprecated(note = "use JSParseState::emit_class_init_end() instead")]
pub unsafe fn emit_class_init_end(s: *mut JSParseState, cf: *const ClassFieldsDef) {
    unreachable!("unused");
}

#[deprecated(note = "use JSParseState::parse_statement_or_decl() instead")]
pub unsafe fn js_parse_statement_or_decl(s: &mut JSParseState, decl_mask: i32) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use AtomRef::is_const() instead")]
pub fn __JS_AtomIsConst(v: AtomRef) -> bool {
    v.is_const()
}

#[deprecated(note = "use AtomRef::is_small_int() instead")]
pub fn __JS_AtomIsTaggedInt(v: AtomRef) -> bool {
    v.is_small_int()
}

#[deprecated(note = "use AtomRef::new_small_int() instead")]
pub fn __JS_AtomFromUInt32(n: u32) -> AtomRef {
    AtomRef::new_small_int(n)
}

#[deprecated(note = "use AtomRef::dup() instead")]
pub unsafe fn JS_DupAtomRT(rt: *mut RuntimeImpl, v: AtomRef) -> AtomRef {
    unreachable!("unused");
}

#[deprecated(note = "use AtomRef::dup() instead")]
#[inline(always)]
pub unsafe fn JS_DupAtom(ctx: ContextRef, v: AtomRef) -> AtomRef {
    unreachable!("unused");
}

#[deprecated(note = "use Atom::kind() instead")]
pub unsafe fn JS_AtomGetKind(ctx: ContextRef, v: AtomRef) -> AtomDescKind {
    v.kind()
}

/// debug helper: print atom, don't use in production code
#[deprecated(note = "use AtomRef::to_str() instead")]
pub unsafe fn print_atom(rt: &RuntimeImpl, atom: AtomRef) {
    print!("{}", atom.to_utf8());
}

#[deprecated(note = "use JSFunctionDef::new() instead")]
pub unsafe fn js_new_function_def(
    ctx: ContextRef,
    parent: *mut JSFunctionDef,
    is_eval: bool,
    is_func_expr: bool,
    filename: &str,
    line_num: i32,
) -> *mut JSFunctionDef {
    unreachable!("use JSFunctionDef::new() instead");

    // if let Ok(d) = JSFunctionDef::new(ctx, parent, is_eval, is_func_expr, filename, line_num) {
    //     debug_assert_eq!(d.scope_first, -1);
    //     Box::into_raw(d)
    // } else {
    //     std::ptr::null_mut()
    // }
}

/// update label ref count by delta. returns new ref count.
#[deprecated(note = "use JSFunctionDef::update_label_ref_count() instead")]
pub unsafe fn update_label(fd: *mut JSFunctionDef, label: u32, delta: i32) -> i32 {
    (*fd).update_label_ref_count(label, delta)
}

/// var_name is not consumed.
// return 0 if OK or -1 if the private field could not be resolved
#[deprecated(note = "use JSFunctionDef::resolve_scope_private_field() instead")]
pub fn resolve_scope_private_field(
    ctx: ContextRef,
    s: *mut JSFunctionDef,
    var_name: AtomRef,
    scope_level: i32,
    op: u8,
    bc: &mut Vec<u8>,
) -> i32 {
    unreachable!("unused");
    // unsafe {
    //     (*s).resolve_scope_private_field(ctx, var_name, scope_level, op, bc)
    //         .map_or(-1, |_| 0)
    // }
}

// search in all scopes
#[deprecated(note = "use JSFunctionDef::find_private_class_field_all() instead")]
pub fn find_private_class_field_all(
    fd: *const JSFunctionDef,
    name: AtomRef,
    scope_level: i32,
) -> i32 {
    unreachable!("unused");
    // unsafe {
    //     (*fd)
    //         .find_private_class_field_all(name, scope_level)
    //         .unwrap_or(-1)
    // }
}

/// var_name is not consumed
#[deprecated(note = "use JSFunctionDef::resolve_scope_private_field1() instead")]
pub fn resolve_scope_private_field1(
    ctx: ContextRef,
    pis_ref: *mut bool,
    pvar_kind: *mut JSVarKindEnum,
    s: *mut JSFunctionDef,
    var_name: AtomRef,
    scope_level: i32,
) -> i32 {
    unreachable!("unused");
    // unsafe {
    //     match (*s).resolve_scope_private_field1(ctx, var_name, scope_level) {
    //         Ok((idx, kind, is_ref)) => {
    //             *pis_ref = is_ref;
    //             *pvar_kind = kind;
    //             idx
    //         }
    //         Err(_) => -1,
    //     }
    // }
}

/// `label_name` is not consumed
#[deprecated(note = "use JSFunctionDef::push_break_entry() instead")]
pub unsafe fn push_break_entry(
    fd: *mut JSFunctionDef,
    be: *mut BlockEnv,
    label_name: AtomRef,
    label_break: i32,
    label_cont: i32,
    drop_count: i32,
) {
    unreachable!("unused");
}

#[deprecated(note = "use JSFunctionDef::pop_break_entry() instead")]
pub unsafe fn pop_break_entry(fd: *mut JSFunctionDef) {
    let be = (*fd).top_break;
    (*fd).top_break = (*be).prev;
}

/// return the target label, following the OP_goto jumps
/// the first opcode at destination is stored in *pop
#[deprecated(note = "use JSFunctionDef::find_jump_target() instead")]
pub unsafe fn find_jump_target(
    s: *mut JSFunctionDef,
    label: u32,
    pop: *mut u8,
    pline: *mut i32,
) -> u32 {
    let (label, op, line) = (*s).find_jump_target(label);
    if !pop.is_null() {
        *pop = op;
    }
    if !pline.is_null() {
        *pline = line
    }
    label
}

#[deprecated(note = "use JSFunctionDef::compute_pc2line_info() instead")]
pub unsafe fn compute_pc2line_info(s: *mut JSFunctionDef) {
    (*s).compute_pc2line_info();
}

#[deprecated(note = "use JSFunctionDef::code_has_label() instead")]
pub unsafe fn code_has_label(s: *mut std::ffi::c_void, pos: usize, label: u32) -> bool {
    unreachable!("unused")
}

#[deprecated(note = "use ObjectIter iterator")]
pub fn JS_GetIterator2(ctx: ContextRef, obj: &JSValue, method: &JSValue) -> JSValue {
    let enum_obj = JS_Call(ctx, method, obj, &[]);
    if enum_obj.is_exception() {
        return enum_obj;
    }

    if !enum_obj.is_object() {
        ctx.throw_type_error_not_an_object();
        return JSValue::EXCEPTION;
    }

    return enum_obj;
}

#[deprecated(note = "use ObjectRef::get_iterator_must() instead")]
pub fn JS_GetIterator(ctx: ContextRef, obj: &JSValue, is_async: bool) -> JSValue {
    unreachable!("unused");
    // let iter = if is_async {
    //     let mut val = obj.get_property(ctx, JS_ATOM_Symbol_asyncIterator);
    //     if val.is_exception() {
    //         return JSValue::EXCEPTION;
    //     }

    //     if val.is_undefined_or_null() {
    //         val = obj.get_property(ctx, JS_ATOM_Symbol_iterator);
    //         if val.is_exception() {
    //             return val;
    //         }

    //         let sync_iter = JS_GetIterator2(ctx, obj, &val);
    //         if sync_iter.is_exception() {
    //             return JSValue::EXCEPTION;
    //         }

    //         let ret = ctx
    //             .create_async_from_sync_iterator(sync_iter)
    //             .map(JSValue::Object)
    //             .unwrap_or(JSValue::EXCEPTION);

    //         return ret;
    //     }

    //     val
    // } else {
    //     let v = obj.get_property(ctx, JS_ATOM_Symbol_iterator);
    //     if v.is_exception() {
    //         return JSValue::EXCEPTION;
    //     }
    //     v
    // };

    // if !iter.is_function() {
    //     ctx.throw_type_error("value is not iterable");
    //     return JSValue::EXCEPTION;
    // }

    // JS_GetIterator2(ctx, obj, &iter)
}

// return -1 if exception, 0 if slow case, 1 if the enumeration is finished
#[deprecated(
    note = "use JSForInIterator::prepare_prototype_chain_enum() + ObjectRef::define_property() to achieve"
)]
pub unsafe fn js_for_in_prepare_prototype_chain_enum(ctx: ContextRef, enum_obj: &JSValue) -> i32 {
    unreachable!("unused");
}

/// return *pdone: 0-not done, 1-done, 2-the iterator object is not parsed
#[deprecated(note = "use IteratorObject::apply_next_method() instead")]
pub fn JS_IteratorNext2(
    ctx: ContextRef,
    enum_obj: &JSValue,
    method: &JSValue,
    args: &[JSValue],
    pdone: &mut i32,
) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use IteratorObject::next() instead")]
pub fn JS_IteratorNext(
    ctx: ContextRef,
    enum_obj: &JSValue,
    method: &JSValue,
    args: &[JSValue],
    pdone: &mut bool,
) -> JSValue {
    unreachable!("unused");
}

/// TODO: migrate to Iterator::close()
/// or replace enum_obj: &JSValue with enum_obj: &ObjectRef
#[deprecated(note = "use IteratorObject::close() instead")]
pub fn JS_IteratorClose(
    ctx: ContextRef,
    enum_obj: &JSValue,
    has_exception: bool,
) -> Result<(), i32> {
    unreachable!("unused");
}

#[deprecated(note = "use JSForInIterator::new() instead")]
pub fn build_for_in_iterator(ctx: ContextRef, obj: JSValue) -> JSValue {
    ForInIterPayload::create(ctx, obj).map_or(JSValue::EXCEPTION, JSValue::Object)
}

/// return TRUE if 'atom' is a symbol with a string description
#[deprecated(note = "use AtomRef::has_description() instead")]
pub fn JS_AtomSymbolHasDescription(ctx: ContextRef, atom: AtomRef) -> bool {
    atom.has_string_desc()
}

/// if `obj` is a string object, returns its length. otherwise, returns 0.
/// this func is used as a class exotic method, `obj` is supposed to be an object of JS_CLASS_STRING class.
#[deprecated(note = "use TryInto<StringImpl> to convert obj into StringImpl, then get length")]
pub(crate) fn js_string_obj_get_length(obj: JSValue) -> usize {
    TryInto::<StringImpl>::try_into(obj).map_or(0, |s| s.len())
}

#[deprecated]
pub unsafe fn skip_spaces(pc: *const u8) -> usize {
    let p_start = pc;
    let mut p = pc;
    let mut p_next = std::ptr::null::<u8>();

    loop {
        let c = *p as char;

        if c.is_ascii() {
            if !matches!(c, '\t'..='\r' | ' ') {
                break;
            }
            p = p.offset(1);
        } else if let Ok(uc) = unicode_from_utf8_v0(p, UTF8_CHAR_LEN_MAX as _, &mut p_next) {
            if !char_is_space(uc) {
                break;
            } else {
                p = p_next;
            }
        }
    }

    p.offset_from(p_start) as usize
}

#[deprecated(note = "use AtomRef::to_int() instead")]
#[inline(always)]
pub fn __JS_AtomToUInt32(atom: AtomRef) -> u32 {
    atom.0 & !JS_ATOM_TAG_INT
}

/// test if the `atom` can be used as an array index (in 0..u32::MAX range).
#[deprecated(note = "use AtomRef::to_array_index() instead")]
pub fn JS_AtomIsArrayIndex(ctx: ContextRef, pval: *mut u32, atom: AtomRef) -> bool {
    unreachable!("unused");
}

#[deprecated(note = "use AtomRegistry::init_const_atoms() instead")]
pub unsafe fn JS_InitAtoms(rt: &mut RuntimeImpl) -> Result<(), i32> {
    unreachable!("unused");
}

/// find atom by str
#[deprecated(note = "use AtomRef::find() instead")]
pub fn __JS_FindAtom(rt: &RuntimeImpl, str: &str, atom_kind: AtomKind) -> Option<AtomRef> {
    unreachable!("unused")
}

#[deprecated(note = "use JSValue::to_string_non_undefined_or_null() instead")]
pub fn JS_ToStringCheckObject(ctx: ContextRef, val: JSValue) -> Result<StringImpl, i32> {
    val.to_string_non_undefined_or_null(ctx)
    // if val.is_undefined_or_null() {
    //     ctx.throw_type_error("null or undefined are forbidden");
    //     Err(-1)
    // } else {
    //     val.to_string(ctx)
    // }
}

#[deprecated(note = "use JSParseState::parse_regexp() instead")]
pub unsafe fn js_parse_regexp(s: &mut JSParseState) -> Result<(), i32> {
    unreachable!("unused")
}

#[deprecated(note = "use JSValue::call() instead")]
#[inline(always)]
pub fn JS_Call(
    ctx: ContextRef,
    func_obj: &JSValue,
    this_obj: &JSValue,
    args: &[JSValue],
) -> JSValue {
    func_obj
        .call(ctx, this_obj, args)
        .unwrap_or(JSValue::EXCEPTION)
}

/// call func_obj, and free func_obj
#[deprecated(note = "use JS_Call() instead")]
pub fn JS_CallFree(
    ctx: ContextRef,
    func_obj: JSValue,
    this_obj: JSValue,
    args: &[JSValue],
) -> JSValue {
    func_obj
        .call(ctx, &this_obj, args)
        .unwrap_or(JSValue::EXCEPTION)
}

/// `this_val` is not consumed
#[deprecated(note = "use JSValue::invoke() instead")]
pub fn JS_Invoke(ctx: ContextRef, this_val: &JSValue, atom: AtomRef, args: &[JSValue]) -> JSValue {
    unreachable!("ununsed");
    // let func_obj = this_val.get_property(ctx, atom);
    // if func_obj.is_err() {
    //     return JSValue::EXCEPTION;
    // }
    // JS_Call(ctx, &func_obj.unwrap(), this_val, args)
}

/// `this_val` is consumed
#[deprecated(note = "use JSValue::invoke() instead")]
pub fn JS_InvokeFree(
    ctx: ContextRef,
    this_val: JSValue,
    atom: AtomRef,
    args: &[JSValue],
) -> JSValue {
    unreachable!("ununsed");
    // JS_Invoke(ctx, &this_val, atom, args)
}

#[deprecated(note = "use ContextRef::take_exception() instead")]
pub unsafe fn JS_GetException(ctx: ContextRef) -> JSValue {
    ctx.take_exception()
}

#[deprecated(note = "use StringBuf::find() instead")]
pub unsafe fn string_indexof(p1: *mut StringImpl, p2: *mut StringImpl, from: i32) -> i32 {
    debug_assert!(from >= 0);
    (*p1)
        .content()
        .index_of((*p2).content(), from as _)
        .map_or(-1, |i| i as i32)
}

#[deprecated(note = "use StringBuf::find_char() instead")]
pub unsafe fn string_indexof_char(p: *mut StringImpl, c: i32, from: i32) -> i32 {
    (*p).content()
        .index_of_char(char::from_u32(c as u32).unwrap(), from as _)
        .map_or(-1, |i| i as i32)

    // let len = (*p).len() as i32;
    // if (0..len).contains(&from) {
    //     let mut it = (*p).content().chars().enumerate().skip(from as _);

    //     if (*p).is_wide_char() || c & !0xff == 0 {
    //         it.find(|(_, ch)| (*ch) as i32 == c)
    //             .map(|(i, _)| i as i32)
    //             .unwrap_or(-1)
    //     } else {
    //         -1
    //     }
    // } else {
    //     -1
    // }
}

#[deprecated(note = "use ContextRef::create_array_from_slice() instead")]
pub fn js_create_array(ctx: ContextRef, tab: &[JSValue]) -> Result<Array, i32> {
    unreachable!("unused");
}

/// can be called on Array or Arguments objects. return < 0 if memory alloc error.
/// returns -1 if fail.
#[deprecated(note = "use ObjectRef::destruct_fast_array() instead")]
pub fn convert_fast_array_to_array(ctx: ContextRef, obj: &ObjectRef) -> i32 {
    obj.destruct_fast_array(ctx).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSValue::to_array_length() instead")]
pub unsafe fn JS_ToArrayLengthFree(
    ctx: ContextRef,
    val: JSValue,
    is_array_ctor: bool,
) -> Result<u32, i32> {
    val.to_array_length(ctx, is_array_ctor).map(|v| v as u32)
}

#[deprecated(note = "use Proxy::get_method() instead")]
pub unsafe fn get_proxy_method<'a>(
    ctx: &ContextImpl,
    obj: &JSValue,
    name: AtomRef,
) -> Result<(JSValue, &'a ProxyPayload), i32> {
    if let ObjectPayload::PROXY(s) = obj.as_object_unchecked().payload() {
        unimplemented!()
    } else {
        unreachable!()
    }
}

#[deprecated(note = "use Proxy::class_finalize instead")]
pub fn js_proxy_finalizer(rt: &mut RuntimeImpl, is: &ObjectPayload, id: usize) {
    unimplemented!();
}

#[deprecated(note = "use Proxy::class_mark instead")]
pub fn js_proxy_mark(val: JSValue) {
    unimplemented!();
}

#[deprecated(note = "use Proxy::is_array() instead")]
pub fn js_proxy_isArray(ctx: ContextRef, val: &JSValue) -> Result<bool, i32> {
    unreachable!("unused")
}

#[deprecated(note = "use Proxy::is_extensible() instead")]
pub fn js_proxy_isExtensible(ctx: ContextRef, obj: &JSValue) -> Result<bool, i32> {
    unreachable!("unused")
}

#[deprecated(note = "use Proxy::prevent_extensions() instead")]
pub fn js_proxy_preventExtensions(ctx: ContextRef, obj: &JSValue) -> Result<bool, i32> {
    unreachable!("unused")
}

#[deprecated(note = "use Proxy::get_prototype_of() instead")]
pub unsafe fn js_proxy_getPrototypeOf(ctx: ContextRef, obj: &JSValue) -> JSValue {
    unreachable!("unused")
}

#[deprecated(note = "use Proxy::set_prototype_of instead")]
pub unsafe fn js_proxy_setPrototypeOf(
    ctx: ContextRef,
    obj: &JSValue,
    proto_val: JSValue,
    throw_flag: bool,
) -> Result<bool, i32> {
    unreachable!("unused")
}

#[deprecated(note = "use JSPropertyDescriptor::parse() instead")]
pub fn js_obj_to_desc(ctx: *const ContextRef, d: *mut JSPropertyDescriptor, desc: JSValue) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use JSPropertyDescriptor::make_object() instead")]
pub fn js_create_desc(
    ctx: *const ContextRef,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> JSValue {
    unreachable!("unused")
}

#[deprecated(note = "use GeneratorPayload::complete() instead")]
pub unsafe fn free_generator_stack_rt(rt: *mut RuntimeImpl, s: *mut GeneratorPayload) {
    (*s).complete();
}

#[deprecated(note = "use JSParseState::get_pos() instead")]
pub unsafe fn js_parse_get_pos(s: *mut JSParseState, sp: *mut c_void /*JSParsePos*/) -> i32 {
    unreachable!("unused");
    // (*sp).last_line_num = (*s).last_line_num;
    // (*sp).line_num = (*s).token.line_num;
    // (*sp).ptr = (*s).token.ptr;
    // (*sp).got_lf = (*s).got_lf;
    // 0
}

#[deprecated(note = "use JSParseState::seek_token() instead")]
pub unsafe fn js_parse_seek_token(
    s: *mut JSParseState,
    sp: *const c_void, /*JSParsePos*/
) -> i32 {
    unreachable!("unused");
    // if (*s).seek_token(sp.as_ref().unwrap()).is_err() {
    //     -1
    // } else {
    //     0
    // }
}

#[deprecated(note = "use ObjectRef::define_property_desc_multi() instead")]
pub fn JS_ObjectDefineProperties(
    ctx: ContextRef,
    obj: &JSValue,
    properties: &JSValue,
) -> Result<(), i32> {
    unreachable!("unused");

    // if !obj.is_object() {
    //     ctx.throw_type_error_not_an_object();
    //     return Err(-1);
    // }

    // let props = properties.to_object(ctx)?;

    // if let JSValue::Object(obj) = obj {
    //     props.own_property_names_foreach(
    //         ctx,
    //         JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK,
    //         |cx, name: PropertyKey| {
    //             let desc = props.get_property(cx, name.atom)?;
    //             if let JSValue::Object(desc) = desc {
    //                 obj.define_property_desc(cx, name.atom, &desc, JS_PROP_THROW)
    //                     .map(|_| ())
    //             } else {
    //                 ctx.throw_type_error_not_an_object();
    //                 return Err(-1);
    //             }
    //         },
    //     )
    // } else {
    //     ctx.throw_type_error_not_an_object();
    //     Err(-1)
    // }
}

#[deprecated]
pub unsafe fn js_autoinit_get_id(pr: *const JSProperty) -> i32 {
    unreachable!("unuese");
}

#[deprecated(note = "use JSProperty::realm() instead")]
pub unsafe fn js_autoinit_get_realm(pr: &JSProperty) -> ContextRef {
    if let JSProperty::LazyInit { realm, .. } = pr {
        realm.upgrade().unwrap()
    } else {
        unreachable!()
    }
}

#[deprecated]
pub unsafe fn js_autoinit_free(rt: &RuntimeImpl, pr: *const JSProperty) {}

/* warning: 'prs' is reallocated after it */
#[deprecated(note = "use JSProperty::auto_init() instead")]
pub unsafe fn JS_AutoInitProperty(
    ctx: ContextRef,
    p: &JSValue,
    prop: AtomRef,
    pr: &JSProperty,
    mut prs: *mut ShapeProperty,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "unused")]
pub unsafe fn js_free_desc(ctx: ContextRef, desc: *mut JSPropertyDescriptor) {
    unreachable!("unused");
}

#[deprecated(note = "use JSValue::set_prototype_internal() instead")]
pub fn JS_SetPrototypeInternal(
    ctx: ContextRef,
    obj: &JSValue,
    proto_val: JSValue,
    throw_flag: bool,
) -> Result<bool, i32> {
    obj.set_prototype_internal(ctx, proto_val, throw_flag)
}

// XXX: could normalize strings to speed up comparison
#[deprecated(note = "use MapKV::normalize_key() instead")]
pub unsafe fn map_normalize_key(ctx: ContextRef, key: JSValue) -> JSValue {
    MapPair::normalize_key(key)
}

// XXX: better hash ?
#[deprecated(note = "use JSValue::hash() instead")]
pub unsafe fn map_hash_key(ctx: ContextRef, key: JSValue) -> u32 {
    key.hash()
}

#[deprecated(note = "use JSMapState::find() instead")]
pub unsafe fn map_find_record(ctx: ContextRef, s: *mut JSMapState, key: JSValue) -> *mut MapPair {
    unreachable!("unused");
}

#[deprecated(note = "use JSMapState::add_kv() instead")]
pub unsafe fn map_add_record(ctx: ContextRef, s: *mut JSMapState, key: JSValue) -> *mut MapPair {
    unreachable!("unused");
}

#[deprecated]
pub unsafe fn map_hash_resize(ctx: ContextRef, s: *mut JSMapState) {
    unreachable!("unused")
}

#[deprecated(note = "use ObjectRef::as_BCFunction().is_some() to check")]
pub(crate) const fn js_class_has_bytecode(class_id: ClassID) -> bool {
    matches!(
        class_id,
        JS_CLASS_BYTECODE_FUNCTION
            | JS_CLASS_GENERATOR_FUNCTION
            | JS_CLASS_ASYNC_FUNCTION
            | JS_CLASS_ASYNC_GENERATOR_FUNCTION
    )
}

/// Check if object has a valid 'name' property
/// Returns true if:
/// 1. Has a non-normal 'name' property (getter/setter)
/// 2. 'name' is a non-empty string
/// 3. 'name' is a symbol with description
#[deprecated(note = "use ObjectRef::has_name() instead")]
pub fn js_object_has_name(obj: &JSValue) -> bool {
    unreachable!("unused");
}

#[deprecated(note = "use ObjectRef::define_name() instead")]
pub fn JS_DefineObjectName(ctx: ContextRef, obj: &JSValue, name: AtomRef, flags: u32) -> i32 {
    unreachable!("ununsed");
}

/// Note: `obj` and `str_val` are not consumed.
#[deprecated(note = "use ObjectRef::define_name_computed() instead")]
pub unsafe fn JS_DefineObjectNameComputed(
    ctx: ContextRef,
    obj: &JSValue,
    str_val: &JSValue,
    flags: u32,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use PromiseResolveFunc::class_call() instead")]
pub fn js_promise_resolve_function_call() {}

/// return true if 'input' contains the source of a module
/// (heuristic). 'input' must be a zero terminated.
//
/// Heuristic: skip comments and expect 'import' keyword not followed by '(' or '.' or export keyword.
#[deprecated(note = "use JSParseState::detect_module() instead")]
pub unsafe fn JS_DetectModule(input: &[u8]) -> bool {
    unreachable!("unused");

    // let buf_p = input.as_ptr();
    // let mut p = skip_shebang(buf_p, buf_p.add(input.len()));

    // match simple_next_token(&mut p, false) {
    //     TOK_IMPORT => {
    //         let tok = simple_next_token(&mut p, false);
    //         !matches!(tok, CHAR_BRACKET_L | CHAR_DOT)
    //     }
    //     TOK_EXPORT => true,
    //     _ => false,
    // }
}

/// Note: 'name' is freed on error
#[deprecated(note = "use JSModuleDefInner::new() instead")]
pub unsafe fn js_new_module_def(ctx: ContextRef, name: AtomRef) -> *mut ModuleDefImpl {
    unreachable!("unused");
}

#[deprecated(note = "see impl Drop for JSModuleDefInner")]
pub unsafe fn js_free_module_def(ctx: ContextRef, m: *mut ModuleDefImpl) {
    unreachable!("unused")

    // JS_FreeAtom(ctx, (*m).module_name);

    // for i in 0..(*m).req_module_entries_count {
    //     let rme: *mut ModuleDependency = (*m).dependencies.offset(i as isize);
    //     JS_FreeAtom(ctx, (*rme).module_name);
    // }
    // js_free(ctx, (*m).dependencies.cast());

    // for me in (*m).export_entries.drain(..) {
    //     if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = me.export_type {
    //         debug_assert!(var_ref.is_some());
    //         if let Some(vr) = var_ref {
    //             JSVarRef::free(ctx.rt_mut(), vr.cast());
    //         }
    //     }

    //     JS_FreeAtom(ctx, me.export_name);
    //     JS_FreeAtom(ctx, me.local_name);
    // }

    // js_free(ctx, (*m).star_export_entries.cast());

    // for i in 0..(*m).import_entries_count {
    //     let mi: *mut JSImportEntry = (*m).import_entries.offset(i as isize);
    //     JS_FreeAtom(ctx, (*mi).import_name);
    // }
    // js_free(ctx, (*m).import_entries.cast());
    // js_free(ctx, (*m).async_parent_modules.cast());

    // JS_FreeValue(ctx, &(*m).module_ns);
    // JS_FreeValue(ctx, &(*m).func_obj);
    // JS_FreeValue(ctx, (*m).eval_exception);
    // JS_FreeValue(ctx, (*m).meta_obj);
    // JS_FreeValue(ctx, (*m).promise);
    // JS_FreeValue(ctx, (*m).resolving_funcs[0]);
    // JS_FreeValue(ctx, (*m).resolving_funcs[1]);
    // list_del(&mut (*m).link);

    // drop(Box::from_raw(m));
}

// Create the <eval> function associated with the module
#[deprecated(note = "use ModuleDefImpl::create_module_bytecode_function() instead")]
pub unsafe fn js_create_module_bytecode_function(
    ctx: ContextRef,
    m: &ModuleDef,
) -> Result<(), i32> {
    unreachable!("unused")

    // debug_assert!(!m.0.func_created.get());

    // let module_bc = {
    //     let bc = m.0.bytecode.replace(JSValue::UNDEFINED);
    //     m.0.bytecode.replace(bc.clone());
    //     bc
    // };

    // if let JSValue::Bytecode(b) = module_bc {
    //     // create function from bytecode
    //     let (isa, class_id) = {
    //         let mut var_refs = Vec::<JSVarRef>::new();
    //         let cvar_count = b.0.borrow().closure_var_count;
    //         if cvar_count != 0 {
    //             var_refs
    //                 .try_reserve_exact(cvar_count as usize)
    //                 .map_err(|_| {
    //                     ctx.throw_out_of_memory();
    //                     -1
    //                 })?;

    //             // create the global variables.
    //             // The other variables are imported from other modules
    //             for cv in b.closure_vars_slice().iter() {
    //                 if cv.is_local() {
    //                     let vr = js_create_module_var(ctx, cv.is_lexical())?;
    //                     #[cfg(DUMP_MODULE_RESOLVE)]
    //                     println!("local {}: {:p}", i, var_ref);

    //                     var_refs.push(vr);
    //                 } else {
    //                     // The other variables are imported from other modules.
    //                     // Add slot with a placeholder.
    //                     var_refs.push(JSVarRef::new(ctx.rt_mut(), JSValue::UNDEFINED));
    //                 }
    //             }
    //         }

    //         // create bytecode function when has_tla=false;
    //         // create async function when has_tla=true;
    //         if m.0.has_tla.get() {
    //             // b.0.borrow_mut().js_mode |= JS_MODE_ASYNC as u8;
    //             // (
    //             //     ObjectPayload::ASYNC_FUNCTION(BCFuncPayload {
    //             //         bytecode: Some(b.clone()),
    //             //         var_refs,
    //             //         home_object: None,
    //             //     }),
    //             //     JS_CLASS_BYTECODE_FUNCTION,
    //             // )
    //             (
    //                 ObjectPayload::BYTECODE_FUNCTION(BCFuncPayload {
    //                     bytecode: Some(b.clone()),
    //                     var_refs,
    //                     home_object: None,
    //                 }),
    //                 JS_CLASS_ASYNC_FUNCTION,
    //             )
    //         } else {
    //             (
    //                 ObjectPayload::BYTECODE_FUNCTION(BCFuncPayload {
    //                     bytecode: Some(b.clone()),
    //                     var_refs,
    //                     home_object: None,
    //                 }),
    //                 JS_CLASS_ASYNC_FUNCTION,
    //             )
    //         }
    //     };

    //     // create function object
    //     let proto = ctx.class_proto[class_id as usize].clone();
    //     let func_obj = ctx.create_object_with_proto(proto, isa).map_err(|_| -1)?;
    //     m.0.bytecode.replace(func_obj.into());
    // } else {
    //     debug_assert!(
    //         matches!(
    //             module_bc.object().map(|o| o.class_id()),
    //             Some(JS_CLASS_BYTECODE_FUNCTION)
    //         ),
    //         "module bytecode has been created into function object",
    //     );
    // }

    // Ok(())
}

#[deprecated(note = "use ModuleDef::new() instead")]
pub unsafe fn JS_NewModuleValue(m: ModuleDef) -> JSValue {
    m.into()
}

// must be done before js_link_module() because of cyclic references
#[deprecated(note = "use ModuleDef::create_module_function() instead")]
pub unsafe fn js_create_module_function(ctx: ContextRef, m: &ModuleDef) -> i32 {
    unreachable!("unused");
}

// spec: InnerModuleEvaluation. Return (index, JSValue::UNDEFINED) or (-1, exception)
#[deprecated(note = "use ModuleDef::eval_internal() instead")]
pub unsafe fn js_inner_module_evaluation(
    ctx: ContextRef,
    m: *mut ModuleDef,
    index: i32,
    pstack_top: &mut Option<ModuleDef>,
    pvalue: *mut JSValue,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use ModuleDef::execute_async() instead")]
pub unsafe fn js_execute_async_module(ctx: ContextRef, m: *mut ModuleDef) -> i32 {
    unreachable!("unused");
    // (*m).execute_async(ctx).map_or(-1, |_| -1)
}

#[deprecated(note = "use ModuleDef::execute_sync() instead")]
pub unsafe fn js_execute_sync_module(
    ctx: ContextRef,
    m: *mut ModuleDef,
    pvalue: *mut JSValue,
) -> i32 {
    unreachable!("unused");
}

#[deprecated(note = "use ModuleDef::execute_async_reject_callback() instead")]
pub fn js_async_module_execution_rejected(ctx: ContextRef, args: CallContext) -> JSResult {
    unreachable!("unused");
    // ModuleDef::execute_async_reject_callback(ctx, args)
}

#[deprecated]
pub struct ExecModuleList {
    pub tab: *mut *mut ModuleDef,
    pub count: i32,
    pub size: i32,
}

#[deprecated(note = "use gather_module_ancestors() instead")]
pub unsafe fn gather_available_ancestors(
    ctx: ContextRef,
    module: *mut ModuleDef,
    exec_list: &mut ExecModuleList,
) -> i32 {
    unreachable!("use gather_module_ancestors() instead");

    // if js_check_stack_overflow(ctx.rt(), 0) {
    //     JS_ThrowStackOverflow(ctx);
    //     return -1;
    // }

    // for i in 0..(*module).async_parent_modules_count {
    //     let m = *(*module).async_parent_modules.offset(i as isize);
    //     if unsafe { !(*m).in_execution_list(exec_list) } && !(*(*m).cycle_root).eval_has_exception {
    //         debug_assert_eq!(
    //             (*m).status,
    //             JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
    //         );
    //         debug_assert!(!(*m).eval_has_exception);
    //         debug_assert!((*m).async_evaluation);
    //         debug_assert!((*m).pending_async_dependencies > 0);

    //         (*m).pending_async_dependencies -= 1;
    //         if (*m).pending_async_dependencies == 0 {
    //             if js_resize_array(
    //                 ctx,
    //                 &mut exec_list.tab as *mut _ as *mut *mut _,
    //                 std::mem::size_of::<*mut ModuleDef>() as _,
    //                 &mut exec_list.size,
    //                 exec_list.count + 1,
    //             ) != 0
    //             {
    //                 return -1;
    //             }
    //             *exec_list.tab.offset((*exec_list).count as isize) = m;
    //             exec_list.count += 1;

    //             if !(*m).has_tla {
    //                 if gather_available_ancestors(ctx, m, exec_list) < 0 {
    //                     return -1;
    //                 }
    //             }
    //         }
    //     }
    // }

    // 0
}

#[deprecated(note = "use ModuleDef::in_execution_list() instead")]
pub unsafe fn find_in_exec_module_list(exec_list: &mut ExecModuleList, m: *mut ModuleDef) -> bool {
    unreachable!("unused")
    // for i in 0..(*exec_list).count {
    //     if *(*exec_list).tab.offset(i as isize) == m {
    //         return true;
    //     }
    // }
    // false
}

#[deprecated(note = "use ModuleDef::find_export() instead")]
pub unsafe fn find_export_entry(m: &ModuleDef, export_name: AtomRef) -> Option<ExportEntry> {
    unreachable!("unused");
    // m.find_export(export_name)
}

#[deprecated(note = "use JSModuleDefInner::get_exportd_names() instead")]
pub unsafe fn get_exported_names(
    ctx: ContextRef,
    s: *mut c_void, // GetExportNamesState,
    m: *mut ModuleDefImpl,
    from_star: bool,
) -> Result<(), i32> {
    unreachable!("unused");
    // (*m).get_exported_names(ctx, s.as_mut().unwrap(), from_star)
}

#[deprecated(note = "use ModuleDef::add_star_export_entry() instead")]
pub(crate) fn add_star_export_entry(
    ctx: ContextRef,
    m: &ModuleDef,
    req_module_idx: u16,
) -> Result<(), i32> {
    m.add_star_export_entry(ctx, req_module_idx)
}

#[deprecated(note = "use ModuleDef::execute_async_fulfill_callback() instead")]
pub fn js_async_module_execution_fulfilled(ctx: ContextRef, args: CallContext) -> JSResult {
    unreachable!("unused");
    // ModuleDef::execute_async_fulfill_callback(ctx, args)
}

#[deprecated(note = "use ModuleDef::set_evaluated() instead")]
pub unsafe fn js_set_module_evaluated(ctx: ContextRef, m: &ModuleDef) {
    unreachable!("unused");
    // m.0.set_evaluated(ctx);
}

/// Class id for function kind
#[deprecated(note = "use JSFunctionKindEnum::class_id() instead")]
pub const func_kind_to_class_id: [ClassID; 4] = [
    JS_CLASS_BYTECODE_FUNCTION,        // 0 JS_FUNC_NORMAL
    JS_CLASS_GENERATOR_FUNCTION,       // 1 JS_FUNC_GENERATOR
    JS_CLASS_ASYNC_FUNCTION,           // 2 JS_FUNC_ASYNC
    JS_CLASS_ASYNC_GENERATOR_FUNCTION, // 3 JS_FUNC_ASYNC_GENERATOR
];

#[deprecated(note = "use AGenPayload::complete_return() instead")]
pub fn js_async_generator_completed_return(
    ctx: ContextRef,
    s: &mut AGenPayload,
    value: JSValue,
) -> Result<(), i32> {
    unreachable!("unused")
    // s.completed_return(ctx, value)
}

#[deprecated(note = "use AGenPayload::_await() instead")]
fn js_async_generator_await(s: &AGenPayload, ctx: ContextRef, value: JSValue) -> Result<(), i32> {
    unreachable!("unused")
}

/// JS_CLASS_DATAVIEW is not allowed.
#[deprecated(note = "use TypedArrayPayload::SIZE_LOG2() instead.")]
#[inline(always)]
pub(crate) fn typed_array_size_log2(classid: ClassID) -> u8 {
    TypedArray::SIZE_LOG2(classid)
}

#[deprecated(note = "use TypedArray::create_from() instead")]
pub fn js_typed_array_constructor_obj(
    ctx: ContextRef,
    new_target: &JSValue,
    obj: &JSValue,
    classid: i32,
) -> JSValue {
    TypedArray::create_from(ctx, new_target, obj.clone(), 0, 0, classid as _)
        .map_or(JSValue::EXCEPTION, |x| x.into())
}

#[deprecated]
pub fn js_typed_array_constructor_ta(
    ctx: ContextRef,
    new_target: &JSValue,
    src_obj: JSValue,
    classid: i32,
) -> JSResult {
    unreachable!("unused");
    // TypedArray::try_from(src_obj)
    //     .and_then(|ta| ta.create_to_new(ctx, new_target, classid as ClassID))
}

#[deprecated(note = "use ArrayBuffer::detach() instead")]
pub fn JS_DetachArrayBuffer(ctx: ContextRef, obj: &JSValue) {
    if let Some(ab) = obj.as_object_ArrayBuffer() {
        ab.detatch(ctx);
    }
}

#[deprecated]
pub fn js_get_radix(ctx: ContextRef, val: &JSValue) -> Result<i32, i32> {
    let radix: i32 = val.to_i32_saturating(ctx)?;
    if !(2..=36).contains(&radix) {
        ctx.throw_range_error("radix must be between 2 and 36");
        return Err(-1);
    }
    Ok(radix)
}

#[deprecated(note = "use RegExp::class_finalize() instead")]
pub fn js_regexp_finalizer(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize) {}

#[deprecated(note = "use JSParseState::parse_function_check_names() instead")]
fn js_parse_function_check_names(s: &JSParseState, fd: &JSFunctionDef, func_name: AtomRef) -> i32 {
    unreachable!("unused")
}

#[deprecated(note = "use JSFunctionDef::parse_check_duplicate_parameter() instead")]
pub(super) fn js_parse_check_duplicate_parameter(s: &JSParseState, name: AtomRef) -> i32 {
    unreachable!("unused")
}

#[deprecated(note = "use JSFunctionDef::add_eval_variables() instead")]
fn add_eval_variables(ctx: ContextRef, s: &mut JSFunctionDef) {
    unreachable!("unused")
}

#[deprecated(note = "use JSModuleDefInner::import_meta() instead")]
pub unsafe fn JS_GetImportMeta(ctx: ContextRef, m: *mut ModuleDefImpl) -> JSValue {
    unreachable!("unused");
}

// XXX: could generate specific bytecode
#[deprecated(note = "use JSParseState::parse_class_default_ctor() instead")]
pub unsafe fn js_parse_class_default_ctor(
    s: &mut JSParseState,
    has_super: bool,
    pfd: *mut *mut JSFunctionDef,
) -> i32 {
    unreachable!("unused");
}

/// 将数组 obj 中从 start_pos 开始的 count 个元素复制到 to_pos 开始的位置。
/// - 正向复制（dir > 0）：从 start_pos 到 start_pos + count - 1 的元素依次复制到 to_pos 到 to_pos + count - 1。
/// - 反向复制（dir < 0）：从 start_pos + count - 1 到 start_pos 的元素依次复制到 to_pos + count - 1 到 to_pos。
#[deprecated(note = "use copy_sub_array() instead")]
fn JS_CopySubArray(
    ctx: ContextRef,
    obj: &JSValue,
    to_pos: usize,
    start_pos: usize,
    count: usize,
    dir: i32,
) -> Result<(), i32> {
    todo!()
    // let fast_array = if let JSValue::Object(o) = obj {
    //     if o.flags().is_fast_array() && o.class_id() == JS_CLASS_ARRAY {
    //         Some(o)
    //     } else {
    //         None
    //     }
    // } else {
    //     None
    // };

    // if let Some(fa) = fast_array {
    //     fa.with_fast_array_values_mut(|lst| {
    //         let len = lst.len();

    //         // calc actual copy count
    //         let cnt1 = if start_pos + count > len {
    //             if len > start_pos { len - start_pos } else { 0 }
    //         } else {
    //             count
    //         };
    //         let cnt2 = if to_pos + count > len {
    //             if len > to_pos { len - to_pos } else { 0 }
    //         } else {
    //             count
    //         };
    //         let cnt = std::cmp::min(cnt1, cnt2);

    //         if cnt > 0 {
    //             if dir > 0 {
    //                 for i in 0..cnt {
    //                     let v1 = lst[start_pos + i].clone();
    //                     lst[to_pos + i] = v1;
    //                 }
    //             } else {
    //                 for i in (0..cnt).rev() {
    //                     let v1 = lst[start_pos + i].clone();
    //                     lst[to_pos + i] = v1;
    //                 }
    //             }
    //         }
    //     })
    //     .unwrap();
    // } else {
    //     // sparse array
    //     for i in 0..count {
    //         match obj.try_get_property_i64(ctx, (start_pos + i) as i64)? {
    //             Some(val) => {
    //                 obj.set_property_i64(ctx, (to_pos + i) as _, val)?;
    //             }
    //             None => {
    //                 obj.delete_property_i64(ctx, (to_pos + 1) as _, JS_PROP_THROW)?;
    //             }
    //         }
    //     }
    // }

    // Ok(())
}

#[deprecated]
pub unsafe fn JS_DumpAtoms(rt: &RuntimeImpl) {
    // This only dumps hashed atoms, not JS_ATOM_TYPE_SYMBOL atoms
    // println!(
    //     "AtomRef count={} size={} hash_size={}:",
    //     (*rt).atom_count,
    //     (*rt).atom_size,
    //     (*rt).atom_hash_size
    // );

    // println!("AtomRef hash table: {{");
    // for i in 0..(*rt).atom_hash_size {
    //     let mut h = *(*rt).atom_hash.offset(i as isize);
    //     if h != 0 {
    //         print!("  {i}:");
    //         while h != 0 {
    //             let p = *(*rt).atom_array.offset(h as isize);
    //             print!(" ");
    //             JS_DumpString(rt, p);
    //             h = (*p).hash_next;
    //         }
    //         println!();
    //     }
    // }
    // println!("}}");

    // println!("AtomRef table (non-const): {{");
    // for (i, astr) in (*rt).atom_slots[ATOM_CONST_COUNT as usize..]
    //     .iter()
    //     .enumerate()
    // {
    //     if let Some(p) = astr.as_ref() {
    //         print!(
    //             "  {}: {{ {} {:08x} {:?}",
    //             i + ATOM_CONST_COUNT as usize,
    //             p.atom_info().unwrap().type_hint() as u8,
    //             Atom::calc_atom_hash(p.content(), AtomKind::String),
    //             p.content()
    //         );

    //         // JS_DumpString(rt, p);
    //         println!(" }}");
    //     }
    // }

    // println!("}}");
}

#[deprecated(note = "use ContextRef::clone() instead")]
pub fn JS_DupContext(ctx: ContextRef) -> ContextRef {
    ctx.clone()
}

#[deprecated(note = "unused")]
pub unsafe fn JS_FreeContext(ctx: ContextRef) {
    unreachable!("unused");
    // if ctx.dec_ref() == 0 {
    //     let rt = ctx.rt_mut();

    //     if rt.gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
    //         let node = ctx.gc_node_ptr();
    //         Node::unlink(node);
    //         rt.gc_zero_ref_count_list.push_raw(node);

    //         if rt.gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
    //             rt.gc_free_zeroed();
    //         }
    //     }
    // }
}

// test if 'var_name' is in the variable object on the stack. If is it
// the case, handle it and jump to 'label_done'.
// If argument `label_done` is None, new label will be created.
#[deprecated(note = "use JSFunctionDef::var_object_test() instead")]
pub unsafe fn var_object_test(
    ctx: ContextRef,
    s: *mut JSFunctionDef,
    var_name: &Atom,
    op: u8,
    bc: &mut Vec<u8>,
    label_done: &mut Option<u32>,
    is_with: bool,
) {
    todo!()
    // label_done.replace(s.var_object_test(var_name, op, bc, *label_done, is_with));
}

/// Resolves special pseudo variables (this, new.target, etc.)
/// Returns the variable definition index if found
/// Returns None if the variable is not a pseudo variable
#[deprecated(note = "use JSFunctionDef::resolve_pseudo_var() instead")]
pub unsafe fn resolve_pseudo_var(
    ctx: ContextRef,
    s: *mut JSFunctionDef,
    var_name: &Atom,
) -> Option<u16> {
    todo!()
    // (*s).resolve_pseudo_var(ctx, var_name)
}

#[deprecated(note = "use JSVarRef::dispose() instead")]
pub unsafe fn free_var_ref(rt: *mut RuntimeImpl, var_ref: *mut JSVarRef) {
    unreachable!("unused");
}

#[deprecated(note = "use JSStackFrame::close_lexical_var() instead")]
pub unsafe fn close_lexical_var(ctx: ContextRef, sf: *mut StackFrame, var_idx: u16, is_arg: bool) {
    (*sf).close_lexical_var(var_idx, is_arg);
}

#[deprecated(note = "use JSStackFrame::close_var_refs() instead")]
pub unsafe fn close_var_refs(ctx: ContextRef, sf: *mut StackFrame) {
    (*sf).close_var_refs();
}

#[deprecated(note = "unused")]
pub unsafe fn JS_CreateAsyncFromSyncIterator(ctx: ContextRef, sync_iter: JSValue) -> JSValue {
    unreachable!("unused")
}

/// Get enumerate object and next method of sp[-1] object.
/// Note: new values are updated to sp[-1], sp[0], old value in sp[-1] is consumed.
/// if fail, sp[-1] is not consumed.
#[deprecated(
    note = "use JSValue::get_iterator_must() and JSValue::get_property(JS_ATOM_next) combination"
)]
pub(super) fn js_for_of_start(
    ctx: ContextRef,
    sp: *mut JSValue,
    is_async: bool,
) -> Result<(), i32> {
    let iter_obj = unsafe { &*sp.offset(-1) }.get_iterator_must(ctx, is_async)?;
    let next = iter_obj.get_property(ctx, Atom::CONST_next)?;
    unsafe {
        std::ptr::replace(sp.offset(-1), iter_obj.into());
        std::ptr::replace(sp.offset(0), next);
    }
    Ok(())
}

#[deprecated(note = "use ContextRef::create_instance_with_ctor() instead")]
pub unsafe fn js_create_from_ctor(_ctx: ContextRef, _ctor: JSValue, _class_id: ClassID) -> JSValue {
    unreachable!("unused");
}

#[deprecated(note = "use ObjectRef::prevent_extensions() or JSValue::prevent_extensions() instead")]
pub fn JS_PreventExtensions(ctx: ContextRef, obj: &JSValue) -> Result<bool, i32> {
    if let JSValue::Object(p) = obj {
        if p.class_id() == JS_CLASS_PROXY {
            obj.prevent_extensions(ctx)
        } else {
            p.set_flag_extensible(false);
            Ok(true)
        }
    } else {
        Ok(false)
    }
}

// check if the brand of the home object of 'func' is present on 'obj'
#[deprecated(note = "use JSValue::check_brand() instead")]
pub unsafe fn JS_CheckBrand(ctx: ContextRef, obj: &JSValue, func: &JSValue) -> Result<bool, i32> {
    obj.check_brand(ctx, func)
}

// add a private brand field to 'home_obj' if not already present and
// if obj is != null add a private brand to it
#[deprecated(note = "use JSValue::add_brand() instead")]
pub fn JS_AddBrand(ctx: ContextRef, obj: &JSValue, home_obj: &JSValue) -> Result<(), i32> {
    obj.add_brand(ctx, home_obj)
}

/// Mutually define properties on both `func_obj` and `proto` side:
/// - Define property `func_obj.prototype = proto`
/// - Define property `proto.constructor = func_obj`.
///
/// Note: The `func_obj` may not necessarily be a constructor
#[deprecated(note = "use ContextRef::bind_ctor_proto_ex() instead")]
pub fn JS_SetConstructor2(
    ctx: ContextRef,
    func_obj: &JSValue,
    proto: &JSValue,
    proto_flags: u32,
    ctor_flags: u32,
) {
    ctx.bind_ctor_proto_ex(func_obj, proto, proto_flags, ctor_flags)
        .unwrap();
}

/// Mutually define properties on both `func_obj` and `proto` side:
/// - Define property `func_obj.prototype = proto`
/// - Define property `proto.constructor = func_obj`.
///
/// Note: The `func_obj` may not necessarily be a constructor
#[deprecated(note = "use ContextRef::bind_ctor_proto() instead")]
pub fn JS_SetConstructor(ctx: ContextRef, func_obj: &JSValue, proto: &JSValue) {
    ctx.bind_ctor_proto(func_obj, proto).unwrap();
}

#[deprecated(note = "use ShapeHandle::dump() instead")]
pub unsafe fn JS_DumpShape(_rt: &RuntimeImpl, _i: i32, _sh: &ShapeHandle) {
    unreachable!("unused");
    // sh.dump(rt, i);
}

#[deprecated(note = "use BCFunction::method_set_home_object() instead")]
pub fn js_method_set_home_object(func_obj: &JSValue, home_obj: Option<&ObjectRef>) {
    if let Some(o) = func_obj.as_object_BCFunction() {
        o.method_set_home_object(home_obj);
    }
}

#[deprecated(note = "use ObjectRef::set_flag_constructor() directly")]
pub(crate) fn JS_SetConstructorBit(func_obj: &JSValue, val: bool) -> bool {
    if let JSValue::Object(p) = func_obj {
        p.set_flag_constructor(val);
        true
    } else {
        false
    }
}

// This test must be fast if atom is not a numeric index (e.g. a
// method name). Return JS_UNDEFINED if not a numeric
// index. JS_EXCEPTION can also be returned.
#[deprecated(note = "use Atom::to_numeric_index() instead")]
pub fn JS_AtomIsNumericIndex1(ctx: ContextRef, atom: &Atom) -> JSValue {
    atom.to_numeric_index(ctx)
        .map_or(JSValue::EXCEPTION, |n| match n {
            Some(IntOrFloat::Int64(n)) => JSValue::from(n),
            Some(IntOrFloat::Float64(n)) => JSValue::from(n),
            None => JSValue::UNDEFINED,
        })
}

// return -1 if exception or TRUE/FALSE
#[deprecated(note = "use Atom::is_numeric_index() instead")]
pub unsafe fn JS_AtomIsNumericIndex(ctx: ContextRef, atom: &Atom) -> i32 {
    atom.is_numeric_index(ctx).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use TypedArray::class_finalize() instead")]
pub fn js_typed_array_finalizer(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize) {
    if let Some(ta) = isa.typed_array() {
        // during the GC the finalizers are called in an arbitrary
        // order so the ArrayBuffer finalize may have been called
        ta.array_buffer().payload().remove_typed_array_weakref(id);
    }
}

/* WARNING: 'p' must be a typed array */
#[deprecated(note = "use TypedArray::is_detached() instead")]
pub unsafe fn typed_array_is_detached(p: *mut JSValue) -> bool {
    unreachable!("use TypedArray::is_detached() instead")
}

#[deprecated(note = "use JSParseState::json_next_token()")]
fn json_next_token(s: &mut JSParseState) -> Result<(), i32> {
    s.json_next_token()
}

// XXX: add a specific eval mode so that Function("}), ({") is rejected
#[deprecated(note = "should use BytecodeFunction::class_constructor ???")]
pub fn js_function_constructor<'a>(ctx: ContextRef, cc: CallContext) -> JSResult {
    let func_kind =
        unsafe { std::mem::transmute::<u8, crate::function::JSFunctionKindEnum>(cc.magic() as u8) };

    // build function source
    let mut source = StringBuf::with_capacity(64, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    source.push_char('(').unwrap();
    if matches!(
        func_kind,
        JSFunctionKindEnum::JS_FUNC_ASYNC | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
    ) {
        source.push_str("async").unwrap();
    }
    source.push_str("function").unwrap();

    if matches!(
        func_kind,
        JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
    ) {
        source.push_char('*').unwrap();
    }
    source.push_str(" anonymous(").unwrap();

    if let Some((body, args)) = cc.arguments().split_last() {
        // args list
        for (i, a) in args.iter().enumerate() {
            if i != 0 {
                source.push_char(',').map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            let s = a.to_string(ctx)?;
            source.push_content(s.content()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
        source.push_str(") {\n").map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        // body
        let s = body.to_string(ctx)?;
        source.push_content(s.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        source.push_str("\n})").map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    } else {
        source.push_str(") {})").map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
    }

    // eval to make function
    let func_obj = ctx.eval_internal(
        &JSValue::Object(ctx.global_obj()),
        source.to_utf8().as_bytes(),
        "<input>",
        JS_EVAL_TYPE_INDIRECT,
        -1,
    )?;

    // set prototype, if provided by `this`
    let new_target = cc.this();
    if !new_target.is_undefined() {
        let mut proto = new_target.get_property(ctx, Atom::CONST_prototype)?;
        if !proto.is_object() {
            let realm = JS_GetFunctionRealm(ctx, new_target)?;
            proto = realm
                .class_proto(func_kind.class_id())
                .map_or(JSValue::NULL, JSValue::Object);
        }

        func_obj.set_prototype_internal(ctx, proto, true)?;
    }

    Ok(func_obj)
}

#[deprecated(note = "use ContextRef::throw_type_error_revoked_proxy() instead")]
pub fn JS_ThrowTypeErrorRevokedProxy<'a>(ctx: ContextRef) {
    ctx.throw_type_error("revoked proxy");
}

#[deprecated(note = "use ContextRef::throw_syntax_error() instead")]
pub fn JS_ThrowSyntaxError<'a>(ctx: ContextRef, message: &str) {
    ctx.throw_error(ErrorKind::Syntax, message);
}

#[deprecated(note = "use ContextRef::build_rest() instead")]
pub fn js_build_rest<'a>(ctx: &ContextImpl, first: usize, args: &[JSValue]) -> JSValue {
    ctx.build_rest(first, args).unwrap_or(JSValue::EXCEPTION)
}

#[deprecated(note = "use JSParseState::json_parse_value() instead")]
fn json_parse_value<'a>(s: &mut JSParseState<'a>) -> JSResult {
    let mut val: JSValue = JSValue::NULL;

    match s.token.val {
        // '{'
        CHAR_CURLY_L => {
            s.json_next_token()?;

            val = s.ctx.create_base_object()?.into();
            if s.token.val != b'}' as i32 {
                loop {
                    let prop_name = if s.token.val == TOK_STRING {
                        Atom::from_str(
                            s.ctx.rt().as_ref(),
                            &escape_str(&s.token.data.string().unwrap().to_utf8()),
                            AtomKind::String,
                        )?
                    } else if s.ext_json && s.token.val == TOK_IDENT {
                        s.token.data.ident_atom().unwrap()
                    } else {
                        s.throw_syntax_error("expecting property name");
                        return Err(-1);
                    };

                    s.json_next_token()?;
                    json_parse_expect(s, ':' as i32)?;

                    let prop_val = s.json_parse_value()?;
                    val.define_property_value(&s.ctx, &prop_name, prop_val, JS_PROP_C_W_E)?;

                    if s.token.val != ',' as i32 {
                        break;
                    }
                    s.json_next_token()?;
                    if s.ext_json && s.token.val == b'}' as i32 {
                        break;
                    }
                }
            }
            json_parse_expect(s, b'}' as i32)?;
        }
        // '['
        CHAR_SQUARE_L => {
            s.json_next_token()?;

            let mut lst = Vec::<JSValue>::new();
            if s.token.val != ']' as i32 {
                'arr: loop {
                    let el = s.json_parse_value()?;

                    if lst.len() == lst.capacity() {
                        lst.try_reserve(32).map_err(|_| {
                            s.ctx.throw_out_of_memory();
                            -1
                        })?;
                    }
                    lst.push(el);

                    if s.token.val != ',' as i32 {
                        break 'arr;
                    }

                    s.json_next_token()?;

                    if s.ext_json && s.token.val == b']' as i32 {
                        break 'arr;
                    }
                }
            }

            // make Array
            lst.shrink_to_fit();
            val = s.ctx.create_array_with_values(lst)?.into();

            json_parse_expect(s, b']' as i32)?;
        }
        TOK_STRING => {
            let sbuf = s.token.data.take_string().unwrap();
            val = s.ctx.create_string_with_buf(sbuf)?.into();
            s.json_next_token()?;
        }
        TOK_NUMBER => {
            let n = s.token.data.number().unwrap();
            val = JSValue::from(n);
            s.json_next_token()?;
        }
        TOK_IDENT => {
            val = match s.token.data.ident_atom().unwrap().to_ref() {
                JS_ATOM_true => JSValue::Bool(true),
                JS_ATOM_false => JSValue::Bool(false),
                JS_ATOM_null => JSValue::NULL,
                _ => {
                    if s.token.val == TOK_EOF {
                        s.throw_syntax_error("unexpected end of input");
                    } else {
                        s.throw_syntax_error(&format!("unexpected token: '{}'", unsafe {
                            let t = std::slice::from_raw_parts(
                                s.token.ptr,
                                s.buf_ptr.offset_from(s.token.ptr) as usize,
                            );
                            std::str::from_utf8_unchecked(t)
                        }));
                    }
                    return Err(-1);
                }
            };

            s.json_next_token()?;
        }
        _ => {
            if s.token.val == TOK_EOF {
                s.throw_syntax_error("unexpected end of input");
            } else {
                s.throw_syntax_error(&format!("unexpected token: '{}'", unsafe {
                    let t = std::slice::from_raw_parts(
                        s.token.ptr,
                        s.buf_ptr.offset_from(s.token.ptr) as usize,
                    );
                    std::str::from_utf8_unchecked(t)
                }));
            }
            return Err(-1);
        }
    }

    Ok(val)
}

/// Resume async function execution with an given state
#[deprecated(note = "use AsyncStateImpl::run_async_function() instead")]
fn async_function_resume(ctx: &mut ContextImpl, s: &AsyncFuncState) {
    if s.is_completed() {
        match func_ret {
            Ok(val) => {
                // normal return
                let func = s.0.borrow().resolving_funcs[0].clone(); // resolve: short borrow
                let _ = func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&val));
            }
            Err(_) => {
                let func = s.0.borrow().resolving_funcs[1].clone(); // reject: short borrow
                let error = ctx.rt().take_exception();
                let _ = func.call(ctx, &JSValue::UNDEFINED, &[error]);
            }
        }
    } else {
        let value = unsafe {
            let cur_sp = s.0.borrow().frame.stack_cur_ptr();
            std::ptr::replace(cur_sp.offset(-1).as_ptr(), JSValue::UNDEFINED)
        };

        // await
        let promise = if let Ok(p) = {
            let cc = CallContext::new_simple(
                unsafe { ContextRef::from_ref_unchecked(ctx) },
                ctx.promise_ctor.clone(),
                std::slice::from_ref(&value),
            );
            js_promise_resolve(cc.ctx_ptr(), cc)
        } {
            Promise::try_from(p).unwrap()
        } else {
            let exc = ctx.rt().take_exception();
            let func = s.0.borrow().resolving_funcs[1].clone();
            let _ = func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&exc)); // TODO what to do if exception?
            return;
        };

        let resolving_funcs = if let Ok(rr) = js_async_function_resolve_create(ctx, s) {
            rr
        } else {
            let error = ctx.rt().take_exception();
            let func = s.0.borrow().resolving_funcs[1].clone();
            let _ = func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&error));
            [const { JSValue::UNDEFINED }; 2]
        };

        let res = promise.reaction(
            ctx,
            &resolving_funcs,
            // no need to create 'thrownawayCapability' as in the spec
            &[const { JSValue::UNDEFINED }; 2],
        );

        if res.is_err() {
            let exc = ctx.rt().take_exception();
            let func = &s.0.borrow().resolving_funcs[1].clone();
            let _ = func.call(ctx, &JSValue::UNDEFINED, &[exc]);
        }
    }
}

#[deprecated(note = "use AsyncFuncState::async_function_resolve_create() instead")]
fn js_async_function_resolve_create(
    ctx: &ContextImpl,
    s: &AsyncFuncState,
) -> Result<[JSValue; 2], i32> {
    let mut resolving_funcs = [const { JSValue::UNDEFINED }; 2];

    for i in 0..2 {
        let func_obj = ctx
            .create_object_with_proto(Some(*ctx.function_proto()), {
                if i == 0 {
                    ObjectPayload::ASYNC_FUNCTION_RESOLVE(s.clone())
                } else {
                    ObjectPayload::ASYNC_FUNCTION_REJECT(s.clone())
                }
            })
            .map_err(|_| -1)?;

        resolving_funcs[i] = func_obj.into();
    }

    Ok(resolving_funcs)
}

#[deprecated(note = "use AsyncFuncState::class_call() instead")]
pub(crate) fn js_async_function_resolve_call(
    mut ctx: ContextRef,
    func_obj: &JSValue,
    _this_obj: &JSValue,
    args: &[JSValue],
    _flags: i32,
) -> JSResult {
    let p = unsafe { func_obj.as_object_unchecked() };
    let is_reject = p.class_id() == JS_CLASS_ASYNC_FUNCTION_REJECT;

    let async_state = p.payload().async_function_resolving().unwrap();
    async_state.set_throw_flag(is_reject);

    if is_reject {
        ctx.throw(if args.is_empty() {
            JSValue::UNDEFINED
        } else {
            args[0].clone()
        });
    } else {
        // return value of await
        debug_assert!(async_state.0.borrow().frame.cur_sp_offset > 0);
        async_state.set_return_value(if args.is_empty() {
            JSValue::UNDEFINED
        } else {
            args[0].clone()
        });
    }

    async_function_resume(&mut ctx, &async_state);
    Ok(JSValue::UNDEFINED)
}

#[deprecated(note = "use get_script_or_module_name() instead")]
fn JS_GetScriptOrModuleName(ctx: &ContextImpl, n_stack_levels: i32) -> Atom {
    unreachable!("unused")
}
