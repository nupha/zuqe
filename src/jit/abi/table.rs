//! The helper index constants and the dispatch table generated from
//! them.

use super::*;

/// Stack helpers in call order; the compiler loads entry `id` and calls it.
pub(crate) const HELPER_COUNT: usize = 127;
pub(crate) const HELPER_ADD: usize = 0;
pub(crate) const HELPER_SUB: usize = 1;
pub(crate) const HELPER_MUL: usize = 2;
pub(crate) const HELPER_DIV: usize = 3;
pub(crate) const HELPER_MOD: usize = 4;
pub(crate) const HELPER_LT: usize = 5;
pub(crate) const HELPER_LTE: usize = 6;
pub(crate) const HELPER_GT: usize = 7;
pub(crate) const HELPER_GTE: usize = 8;
pub(crate) const HELPER_EQ: usize = 9;
pub(crate) const HELPER_NEQ: usize = 10;
pub(crate) const HELPER_STRICT_EQ: usize = 11;
pub(crate) const HELPER_STRICT_NEQ: usize = 12;
pub(crate) const HELPER_AND: usize = 13;
pub(crate) const HELPER_OR: usize = 14;
pub(crate) const HELPER_XOR: usize = 15;
pub(crate) const HELPER_SHL: usize = 16;
pub(crate) const HELPER_SAR: usize = 17;
pub(crate) const HELPER_SHR: usize = 18;
pub(crate) const HELPER_PLUS: usize = 19;
pub(crate) const HELPER_NEG: usize = 20;
pub(crate) const HELPER_INC: usize = 21;
pub(crate) const HELPER_DEC: usize = 22;
pub(crate) const HELPER_LNOT: usize = 23;
pub(crate) const HELPER_NOT: usize = 24;
pub(crate) const HELPER_TYPEOF: usize = 25;
/// Index of [`h_to_bool`], which takes the operand as a second argument and
/// returns an `i64` (`-1` on failure) — it is called through `truthy_sig`, not
/// the shared helper signature.
pub(crate) const HELPER_TO_BOOL: usize = 26;
/// Index of [`h_safepoint`], called at loop back edges.
pub(crate) const HELPER_SAFEPOINT: usize = 27;
/// Index of [`h_push_atom`].
pub(crate) const HELPER_PUSH_ATOM: usize = 28;
/// Index of [`h_inc_local`].
pub(crate) const HELPER_INC_LOCAL: usize = 29;
/// Index of [`h_dec_local`].
pub(crate) const HELPER_DEC_LOCAL: usize = 30;
/// Index of [`h_get_var`].
pub(crate) const HELPER_GET_VAR: usize = 31;
/// Index of [`h_get_var_undef`].
pub(crate) const HELPER_GET_VAR_UNDEF: usize = 32;
/// Index of [`h_put_var`].
pub(crate) const HELPER_PUT_VAR: usize = 33;
/// Index of [`h_put_var_init`].
pub(crate) const HELPER_PUT_VAR_INIT: usize = 34;
/// Index of [`h_get_array_el`].
pub(crate) const HELPER_GET_ARRAY_EL: usize = 35;
/// Index of [`h_get_array_el2`].
pub(crate) const HELPER_GET_ARRAY_EL2: usize = 36;
/// Index of [`h_get_array_el3`].
pub(crate) const HELPER_GET_ARRAY_EL3: usize = 37;
/// Index of [`h_put_array_el`].
pub(crate) const HELPER_PUT_ARRAY_EL: usize = 38;
/// Index of [`h_get_field`].
pub(crate) const HELPER_GET_FIELD: usize = 39;
/// Index of [`h_get_field2`].
pub(crate) const HELPER_GET_FIELD2: usize = 40;
/// Index of [`h_put_field`].
pub(crate) const HELPER_PUT_FIELD: usize = 41;
/// Index of [`h_pow`].
pub(crate) const HELPER_POW: usize = 42;
/// Index of [`h_to_object`].
pub(crate) const HELPER_TO_OBJECT: usize = 43;
/// Index of [`h_to_propkey`].
pub(crate) const HELPER_TO_PROPKEY: usize = 44;
/// Index of [`h_in`].
pub(crate) const HELPER_IN: usize = 45;
/// Index of [`h_instanceof`].
pub(crate) const HELPER_INSTANCEOF: usize = 46;
/// Index of [`h_delete`].
pub(crate) const HELPER_DELETE: usize = 47;
/// Index of [`h_add_loc`].
pub(crate) const HELPER_ADD_LOC: usize = 48;
/// Index of [`h_post_inc`].
pub(crate) const HELPER_POST_INC: usize = 49;
/// Index of [`h_post_dec`].
pub(crate) const HELPER_POST_DEC: usize = 50;
/// Index of [`h_nip_catch`].
pub(crate) const HELPER_NIP_CATCH: usize = 51;
/// Index of [`h_rest`].
pub(crate) const HELPER_REST: usize = 52;
/// Index of [`h_special_object`].
pub(crate) const HELPER_SPECIAL_OBJECT: usize = 53;
/// Index of [`h_push_this`].
pub(crate) const HELPER_PUSH_THIS: usize = 54;
/// Index of [`h_fclosure`].
pub(crate) const HELPER_FCLOSURE: usize = 55;
/// Index of [`h_set_name`].
pub(crate) const HELPER_SET_NAME: usize = 56;
/// Index of [`h_get_var_ref`].
pub(crate) const HELPER_GET_VAR_REF: usize = 57;
/// Index of [`h_put_var_ref`].
pub(crate) const HELPER_PUT_VAR_REF: usize = 58;
/// Index of [`h_set_var_ref`].
pub(crate) const HELPER_SET_VAR_REF: usize = 59;
/// Index of [`h_get_var_ref_check`].
pub(crate) const HELPER_GET_VAR_REF_CHECK: usize = 60;
/// Index of [`h_put_var_ref_check`].
pub(crate) const HELPER_PUT_VAR_REF_CHECK: usize = 61;
/// Index of [`h_put_var_ref_check_init`].
pub(crate) const HELPER_PUT_VAR_REF_CHECK_INIT: usize = 62;
/// Index of [`h_for_in_start`].
pub(crate) const HELPER_FOR_IN_START: usize = 63;
/// Index of [`h_for_in_next`].
pub(crate) const HELPER_FOR_IN_NEXT: usize = 64;
/// Index of [`h_for_of_start`].
pub(crate) const HELPER_FOR_OF_START: usize = 65;
/// Index of [`h_for_of_next`].
pub(crate) const HELPER_FOR_OF_NEXT: usize = 66;
/// Index of [`h_iterator_check_object`].
pub(crate) const HELPER_ITERATOR_CHECK_OBJECT: usize = 67;
/// Index of [`h_iterator_get_value_done`].
pub(crate) const HELPER_ITERATOR_GET_VALUE_DONE: usize = 68;
/// Index of [`h_iterator_close`].
pub(crate) const HELPER_ITERATOR_CLOSE: usize = 69;
/// Index of [`h_iterator_next`].
pub(crate) const HELPER_ITERATOR_NEXT: usize = 70;
/// Index of [`h_iterator_call`].
pub(crate) const HELPER_ITERATOR_CALL: usize = 71;
/// Index of [`h_define_field`].
pub(crate) const HELPER_DEFINE_FIELD: usize = 72;
/// Index of [`h_set_name_computed`].
pub(crate) const HELPER_SET_NAME_COMPUTED: usize = 73;
/// Index of [`h_set_proto`].
pub(crate) const HELPER_SET_PROTO: usize = 74;
/// Index of [`h_set_home_object`].
pub(crate) const HELPER_SET_HOME_OBJECT: usize = 75;
/// Index of [`h_define_array_el`].
pub(crate) const HELPER_DEFINE_ARRAY_EL: usize = 76;
/// Index of [`h_copy_data_properties`].
pub(crate) const HELPER_COPY_DATA_PROPERTIES: usize = 77;
/// Index of [`h_define_method`].
pub(crate) const HELPER_DEFINE_METHOD: usize = 78;
/// Index of [`h_define_method_computed`].
pub(crate) const HELPER_DEFINE_METHOD_COMPUTED: usize = 79;
/// Index of [`h_define_class`].
pub(crate) const HELPER_DEFINE_CLASS: usize = 80;
/// Index of [`h_define_class_computed`].
pub(crate) const HELPER_DEFINE_CLASS_COMPUTED: usize = 81;
/// Index of [`h_check_ctor_return`].
pub(crate) const HELPER_CHECK_CTOR_RETURN: usize = 82;
/// Index of [`h_check_ctor`].
pub(crate) const HELPER_CHECK_CTOR: usize = 83;
/// Index of [`h_init_ctor`].
pub(crate) const HELPER_INIT_CTOR: usize = 84;
/// Index of [`h_check_brand`].
pub(crate) const HELPER_CHECK_BRAND: usize = 85;
/// Index of [`h_add_brand`].
pub(crate) const HELPER_ADD_BRAND: usize = 86;
/// Index of [`h_private_symbol`].
pub(crate) const HELPER_PRIVATE_SYMBOL: usize = 87;
/// Index of [`h_get_private_field`].
pub(crate) const HELPER_GET_PRIVATE_FIELD: usize = 88;
/// Index of [`h_put_private_field`].
pub(crate) const HELPER_PUT_PRIVATE_FIELD: usize = 89;
/// Index of [`h_define_private_field`].
pub(crate) const HELPER_DEFINE_PRIVATE_FIELD: usize = 90;
/// Index of [`h_append`].
pub(crate) const HELPER_APPEND: usize = 91;
/// Index of [`h_private_in`].
pub(crate) const HELPER_PRIVATE_IN: usize = 92;
/// Index of [`h_get_super`].
pub(crate) const HELPER_GET_SUPER: usize = 93;
/// Index of [`h_get_super_value`].
pub(crate) const HELPER_GET_SUPER_VALUE: usize = 94;
/// Index of [`h_put_super_value`].
pub(crate) const HELPER_PUT_SUPER_VALUE: usize = 95;
/// Index of [`h_close_loc`].
pub(crate) const HELPER_CLOSE_LOC: usize = 96;
/// Index of [`h_object`].
pub(crate) const HELPER_OBJECT: usize = 97;
/// Index of [`h_array_from`].
pub(crate) const HELPER_ARRAY_FROM: usize = 98;
/// Index of [`h_make_loc_ref`].
pub(crate) const HELPER_MAKE_LOC_REF: usize = 99;
/// Index of [`h_make_arg_ref`].
pub(crate) const HELPER_MAKE_ARG_REF: usize = 100;
/// Index of [`h_make_var_ref_ref`].
pub(crate) const HELPER_MAKE_VAR_REF_REF: usize = 101;
/// Index of [`h_make_var_ref`].
pub(crate) const HELPER_MAKE_VAR_REF: usize = 102;
/// Index of [`h_get_ref_value`].
pub(crate) const HELPER_GET_REF_VALUE: usize = 103;
/// Index of [`h_put_ref_value`].
pub(crate) const HELPER_PUT_REF_VALUE: usize = 104;
/// Index of [`h_with_get_var`].
pub(crate) const HELPER_WITH_GET_VAR: usize = 105;
/// Index of [`h_with_put_var`].
pub(crate) const HELPER_WITH_PUT_VAR: usize = 106;
/// Index of [`h_with_delete_var`].
pub(crate) const HELPER_WITH_DELETE_VAR: usize = 107;
/// Index of [`h_with_make_ref`].
pub(crate) const HELPER_WITH_MAKE_REF: usize = 108;
/// Index of [`h_with_get_ref`].
pub(crate) const HELPER_WITH_GET_REF: usize = 109;
/// Index of [`h_delete_var`].
pub(crate) const HELPER_DELETE_VAR: usize = 110;
/// Index of [`h_throw`].
pub(crate) const HELPER_THROW: usize = 111;
/// Index of [`h_throw_error`].
pub(crate) const HELPER_THROW_ERROR: usize = 112;
/// Index of [`h_regexp`].
pub(crate) const HELPER_REGEXP: usize = 113;
/// Index of [`h_push_bigint_i32`].
pub(crate) const HELPER_PUSH_BIGINT_I32: usize = 114;
/// Index of [`h_call`].
pub(crate) const HELPER_CALL: usize = 115;
/// Index of [`h_call_method`].
pub(crate) const HELPER_CALL_METHOD: usize = 116;
/// Index of [`h_call_constructor`].
pub(crate) const HELPER_CALL_CONSTRUCTOR: usize = 117;
/// Index of [`h_apply`].
pub(crate) const HELPER_APPLY: usize = 118;
/// Index of [`h_gc_bind`], called with the barrier's own signature
/// (`frame`, owner node, stored value).
pub(crate) const HELPER_GC_BIND: usize = 119;
/// Index of [`h_call_inline`], the direct call at a monomorphic site.
pub(crate) const HELPER_CALL_INLINE: usize = 120;
/// Index of [`h_call_method_inline`], the direct method call at a monomorphic
/// site (the direct-call path).
pub(crate) const HELPER_CALL_METHOD_INLINE: usize = 121;
/// Index of [`h_inline_guard`], the guard an inlined monomorphic call site
/// tests before running the callee's body in the caller (the inlined call path).
pub(crate) const HELPER_INLINE_GUARD: usize = 122;
/// Index of [`h_alloc_object`], an empty object literal inside an inlined
/// body. The value is read back from `JitExecFrame::scratch`.
pub(crate) const HELPER_ALLOC_OBJECT: usize = 123;
/// Index of [`h_alloc_empty_array`], an empty array literal inside an inlined body.
pub(crate) const HELPER_ALLOC_ARRAY0: usize = 124;
/// Index of [`h_call_method_apply`], a native-callee method site's fast path for
/// `Function.prototype.apply`.
pub(crate) const HELPER_CALL_APPLY: usize = 125;
/// Index of [`h_object_literal`], the merged object-literal builder.
pub(crate) const HELPER_OBJECT_LITERAL: usize = 126;

/// The helper dispatch table. Function pointers are immutable and never
/// interchanged, so sharing the table across threads is sound.
#[repr(transparent)]
pub(crate) struct HelperTable(pub(crate) [*const (); HELPER_COUNT]);
unsafe impl Sync for HelperTable {}

pub(crate) static HELPERS: HelperTable = HelperTable([
    h_add as *const (),
    h_sub as *const (),
    h_mul as *const (),
    h_div as *const (),
    h_mod as *const (),
    h_lt as *const (),
    h_lte as *const (),
    h_gt as *const (),
    h_gte as *const (),
    h_eq as *const (),
    h_neq as *const (),
    h_strict_eq as *const (),
    h_strict_neq as *const (),
    h_bit_and as *const (),
    h_bit_or as *const (),
    h_bit_xor as *const (),
    h_bit_shl as *const (),
    h_bit_sar as *const (),
    h_bit_shr as *const (),
    h_plus as *const (),
    h_neg as *const (),
    h_inc as *const (),
    h_dec as *const (),
    h_lnot as *const (),
    h_not as *const (),
    h_typeof as *const (),
    h_to_bool as *const (),
    h_safepoint as *const (),
    h_push_atom as *const (),
    h_inc_local as *const (),
    h_dec_local as *const (),
    h_get_var as *const (),
    h_get_var_undef as *const (),
    h_put_var as *const (),
    h_put_var_init as *const (),
    h_get_array_el as *const (),
    h_get_array_el2 as *const (),
    h_get_array_el3 as *const (),
    h_put_array_el as *const (),
    h_get_field as *const (),
    h_get_field2 as *const (),
    h_put_field as *const (),
    h_pow as *const (),
    h_to_object as *const (),
    h_to_propkey as *const (),
    h_in as *const (),
    h_instanceof as *const (),
    h_delete as *const (),
    h_add_loc as *const (),
    h_post_inc as *const (),
    h_post_dec as *const (),
    h_nip_catch as *const (),
    h_rest as *const (),
    h_special_object as *const (),
    h_push_this as *const (),
    h_fclosure as *const (),
    h_set_name as *const (),
    h_get_var_ref as *const (),
    h_put_var_ref as *const (),
    h_set_var_ref as *const (),
    h_get_var_ref_check as *const (),
    h_put_var_ref_check as *const (),
    h_put_var_ref_check_init as *const (),
    h_for_in_start as *const (),
    h_for_in_next as *const (),
    h_for_of_start as *const (),
    h_for_of_next as *const (),
    h_iterator_check_object as *const (),
    h_iterator_get_value_done as *const (),
    h_iterator_close as *const (),
    h_iterator_next as *const (),
    h_iterator_call as *const (),
    h_define_field as *const (),
    h_set_name_computed as *const (),
    h_set_proto as *const (),
    h_set_home_object as *const (),
    h_define_array_el as *const (),
    h_copy_data_properties as *const (),
    h_define_method as *const (),
    h_define_method_computed as *const (),
    h_define_class as *const (),
    h_define_class_computed as *const (),
    h_check_ctor_return as *const (),
    h_check_ctor as *const (),
    h_init_ctor as *const (),
    h_check_brand as *const (),
    h_add_brand as *const (),
    h_private_symbol as *const (),
    h_get_private_field as *const (),
    h_put_private_field as *const (),
    h_define_private_field as *const (),
    h_append as *const (),
    h_private_in as *const (),
    h_get_super as *const (),
    h_get_super_value as *const (),
    h_put_super_value as *const (),
    h_close_loc as *const (),
    h_object as *const (),
    h_array_from as *const (),
    h_make_loc_ref as *const (),
    h_make_arg_ref as *const (),
    h_make_var_ref_ref as *const (),
    h_make_var_ref as *const (),
    h_get_ref_value as *const (),
    h_put_ref_value as *const (),
    h_with_get_var as *const (),
    h_with_put_var as *const (),
    h_with_delete_var as *const (),
    h_with_make_ref as *const (),
    h_with_get_ref as *const (),
    h_delete_var as *const (),
    h_throw as *const (),
    h_throw_error as *const (),
    h_regexp as *const (),
    h_push_bigint_i32 as *const (),
    h_call as *const (),
    h_call_method as *const (),
    h_call_constructor as *const (),
    h_apply as *const (),
    h_gc_bind as *const (),
    h_call_inline as *const (),
    h_call_method_inline as *const (),
    h_inline_guard as *const (),
    h_alloc_object as *const (),
    h_alloc_empty_array as *const (),
    h_call_method_apply as *const (),
    h_object_literal as *const (),
]);
