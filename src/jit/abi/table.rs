// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The helper index constants and the dispatch table, both generated from one
//! ordered list so an id and its table entry cannot drift apart.

use super::*;

macro_rules! helpers {
    ($($(#[$meta:meta])* $name:ident => $func:ident,)*) => {
        /// Position in the table below; the variants *are* the helper ids.
        #[allow(dead_code, non_camel_case_types)]
        enum HelperId {
            $($name,)*
            Count,
        }

        $(
            $(#[$meta])*
            pub(crate) const $name: usize = HelperId::$name as usize;
        )*

        /// Stack helpers in call order; the compiler loads entry `id` and calls it.
        pub(crate) const HELPER_COUNT: usize = HelperId::Count as usize;

        /// The helper dispatch table. Function pointers are immutable and never
        /// interchanged, so sharing the table across threads is sound.
        #[repr(transparent)]
        pub(crate) struct HelperTable(pub(crate) [*const (); HELPER_COUNT]);
        unsafe impl Sync for HelperTable {}

        pub(crate) static HELPERS: HelperTable = HelperTable([
            $($func as *const (),)*
        ]);
    };
}

helpers! {
    HELPER_ADD => h_add,
    HELPER_SUB => h_sub,
    HELPER_MUL => h_mul,
    HELPER_DIV => h_div,
    HELPER_MOD => h_mod,
    HELPER_LT => h_lt,
    HELPER_LTE => h_lte,
    HELPER_GT => h_gt,
    HELPER_GTE => h_gte,
    HELPER_EQ => h_eq,
    HELPER_NEQ => h_neq,
    HELPER_STRICT_EQ => h_strict_eq,
    HELPER_STRICT_NEQ => h_strict_neq,
    HELPER_AND => h_bit_and,
    HELPER_OR => h_bit_or,
    HELPER_XOR => h_bit_xor,
    HELPER_SHL => h_bit_shl,
    HELPER_SAR => h_bit_sar,
    HELPER_SHR => h_bit_shr,
    HELPER_PLUS => h_plus,
    HELPER_NEG => h_neg,
    HELPER_INC => h_inc,
    HELPER_DEC => h_dec,
    HELPER_LNOT => h_lnot,
    HELPER_NOT => h_not,
    HELPER_TYPEOF => h_typeof,
    /// `h_to_bool` takes the operand as a second argument and returns an
    /// `i64` (`-1` on failure): it is called through `truthy_sig`, not the
    /// shared helper signature.
    HELPER_TO_BOOL => h_to_bool,
    /// Called at loop back edges.
    HELPER_SAFEPOINT => h_safepoint,
    HELPER_PUSH_ATOM => h_push_atom,
    HELPER_INC_LOCAL => h_inc_local,
    HELPER_DEC_LOCAL => h_dec_local,
    HELPER_GET_VAR => h_get_var,
    HELPER_GET_VAR_UNDEF => h_get_var_undef,
    HELPER_PUT_VAR => h_put_var,
    HELPER_PUT_VAR_INIT => h_put_var_init,
    HELPER_GET_ARRAY_EL => h_get_array_el,
    HELPER_GET_ARRAY_EL2 => h_get_array_el2,
    HELPER_GET_ARRAY_EL3 => h_get_array_el3,
    HELPER_PUT_ARRAY_EL => h_put_array_el,
    HELPER_GET_FIELD => h_get_field,
    HELPER_GET_FIELD2 => h_get_field2,
    HELPER_PUT_FIELD => h_put_field,
    HELPER_POW => h_pow,
    HELPER_TO_OBJECT => h_to_object,
    HELPER_TO_PROPKEY => h_to_propkey,
    HELPER_IN => h_in,
    HELPER_INSTANCEOF => h_instanceof,
    HELPER_DELETE => h_delete,
    HELPER_ADD_LOC => h_add_loc,
    HELPER_POST_INC => h_post_inc,
    HELPER_POST_DEC => h_post_dec,
    HELPER_NIP_CATCH => h_nip_catch,
    HELPER_REST => h_rest,
    HELPER_SPECIAL_OBJECT => h_special_object,
    HELPER_PUSH_THIS => h_push_this,
    HELPER_FCLOSURE => h_fclosure,
    HELPER_SET_NAME => h_set_name,
    HELPER_GET_VAR_REF => h_get_var_ref,
    HELPER_PUT_VAR_REF => h_put_var_ref,
    HELPER_SET_VAR_REF => h_set_var_ref,
    HELPER_GET_VAR_REF_CHECK => h_get_var_ref_check,
    HELPER_PUT_VAR_REF_CHECK => h_put_var_ref_check,
    HELPER_PUT_VAR_REF_CHECK_INIT => h_put_var_ref_check_init,
    HELPER_FOR_IN_START => h_for_in_start,
    HELPER_FOR_IN_NEXT => h_for_in_next,
    HELPER_FOR_OF_START => h_for_of_start,
    HELPER_FOR_OF_NEXT => h_for_of_next,
    HELPER_ITERATOR_CHECK_OBJECT => h_iterator_check_object,
    HELPER_ITERATOR_GET_VALUE_DONE => h_iterator_get_value_done,
    HELPER_ITERATOR_CLOSE => h_iterator_close,
    HELPER_ITERATOR_NEXT => h_iterator_next,
    HELPER_ITERATOR_CALL => h_iterator_call,
    HELPER_DEFINE_FIELD => h_define_field,
    HELPER_SET_NAME_COMPUTED => h_set_name_computed,
    HELPER_SET_PROTO => h_set_proto,
    HELPER_SET_HOME_OBJECT => h_set_home_object,
    HELPER_DEFINE_ARRAY_EL => h_define_array_el,
    HELPER_COPY_DATA_PROPERTIES => h_copy_data_properties,
    HELPER_DEFINE_METHOD => h_define_method,
    HELPER_DEFINE_METHOD_COMPUTED => h_define_method_computed,
    HELPER_DEFINE_CLASS => h_define_class,
    HELPER_DEFINE_CLASS_COMPUTED => h_define_class_computed,
    HELPER_CHECK_CTOR_RETURN => h_check_ctor_return,
    HELPER_CHECK_CTOR => h_check_ctor,
    HELPER_INIT_CTOR => h_init_ctor,
    HELPER_CHECK_BRAND => h_check_brand,
    HELPER_ADD_BRAND => h_add_brand,
    HELPER_PRIVATE_SYMBOL => h_private_symbol,
    HELPER_GET_PRIVATE_FIELD => h_get_private_field,
    HELPER_PUT_PRIVATE_FIELD => h_put_private_field,
    HELPER_DEFINE_PRIVATE_FIELD => h_define_private_field,
    HELPER_APPEND => h_append,
    HELPER_PRIVATE_IN => h_private_in,
    HELPER_GET_SUPER => h_get_super,
    HELPER_GET_SUPER_VALUE => h_get_super_value,
    HELPER_PUT_SUPER_VALUE => h_put_super_value,
    HELPER_CLOSE_LOC => h_close_loc,
    HELPER_OBJECT => h_object,
    HELPER_ARRAY_FROM => h_array_from,
    HELPER_MAKE_LOC_REF => h_make_loc_ref,
    HELPER_MAKE_ARG_REF => h_make_arg_ref,
    HELPER_MAKE_VAR_REF_REF => h_make_var_ref_ref,
    HELPER_MAKE_VAR_REF => h_make_var_ref,
    HELPER_GET_REF_VALUE => h_get_ref_value,
    HELPER_PUT_REF_VALUE => h_put_ref_value,
    HELPER_WITH_GET_VAR => h_with_get_var,
    HELPER_WITH_PUT_VAR => h_with_put_var,
    HELPER_WITH_DELETE_VAR => h_with_delete_var,
    HELPER_WITH_MAKE_REF => h_with_make_ref,
    HELPER_WITH_GET_REF => h_with_get_ref,
    HELPER_DELETE_VAR => h_delete_var,
    HELPER_THROW => h_throw,
    HELPER_THROW_ERROR => h_throw_error,
    HELPER_REGEXP => h_regexp,
    HELPER_PUSH_BIGINT_I32 => h_push_bigint_i32,
    HELPER_CALL => h_call,
    HELPER_CALL_METHOD => h_call_method,
    HELPER_CALL_CONSTRUCTOR => h_call_constructor,
    HELPER_APPLY => h_apply,
    /// Called with the barrier's own signature (`frame`, owner node, stored
    /// value).
    HELPER_GC_BIND => h_gc_bind,
    /// The direct call at a monomorphic site.
    HELPER_CALL_MONO => h_call_mono,
    /// The direct method call at a monomorphic site.
    HELPER_CALL_METHOD_MONO => h_call_method_mono,
    /// The guard an inlined monomorphic call site tests before running the
    /// callee's body in the caller.
    HELPER_INLINE_GUARD => h_inline_guard,
    /// An empty object literal inside an inlined body; the value is read back
    /// from `JitExecFrame::scratch`.
    HELPER_ALLOC_OBJECT => h_alloc_object,
    /// An empty array literal inside an inlined body.
    HELPER_ALLOC_ARRAY0 => h_alloc_empty_array,
    /// A native-callee method site's fast path for `Function.prototype.apply`.
    HELPER_CALL_APPLY => h_call_method_apply,
    /// The merged object-literal builder.
    HELPER_OBJECT_LITERAL => h_object_literal,
}
