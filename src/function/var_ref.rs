use std::{
    cell::RefCell,
    marker::PhantomPinned,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::NonNull,
    rc::Rc,
};

use crate::{
    container_of,
    context::{JSContext, JS_MODE_ASYNC},
    gc::{add_gc_object, remove_gc_object, JSGCObjectHeader, JSGCObjectTypeEnum},
    list_entry,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    types::{
        init_list_head, list_add_tail, list_del, list_foreach_safe, list_foreach_with_ret,
        list_head,
    },
    value::{JSValue, JS_DupValue, JS_FreeValueRT},
};

use super::{async_func_free, JSAsyncFunctionState};

#[repr(C)]
#[derive(Clone, Copy)]
pub union JSVarRefHeaderOrGCFields {
    pub header: JSGCObjectHeader, // must come first
    pub gc_fields: JSVarRefGCFields,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct JSVarRefGCFields {
    // reserved for header.ref_count and header.mark/gc_obj_type
    __gc_ref_count: i32,
    __gc_mark: u8,

    /// bitfield for is_detached and is_arg
    pub bitfield: u8,
    /// index of the function variable on the stack
    pub var_idx: u16,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub union JSVarRefValueOrLinks {
    pub value: JSValue,       // used when is_detached = TRUE
    pub links: JSVarRefLinks, // used when is_detached = FALSE
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct JSVarRefLinks {
    pub u0: JSVarRefHeaderOrGCFields,

    /// JSStackFrame.var_ref_list list
    pub var_ref_list: list_head,

    /// != NULL if async stack frame
    pub async_func: *mut JSAsyncFunctionState,
}

#[repr(C)]
pub struct JSVarRefInner {
    pub u0: JSVarRefHeaderOrGCFields,

    pub var_idx: u16,

    /// bitfield for is_detached and is_arg
    pub bitfield: u8,

    /// 一个指向 JSValue 的指针。这个字段有两个主要用途：
    /// 1. 变量值的定位 ：
    ///    - 当变量在栈上时（detached = false）， pvalue 指向栈上的变量值位置（ var_buf 或 arg_buf ）
    ///    - 当变量被分离时（detached = true）， pvalue 指向 JSVarRef 结构体内部的 value_or_links.value 字段
    /// 2. 闭包变量访问 ：
    ///    - 它允许闭包函数访问外部作用域的变量
    ///    - 通过这个指针可以实现对变量的读写操作，无论变量实际存储在栈上还是已经被分离到堆上
    pub pvalue: *mut JSValue,

    /// 根据变量是否被分离(detached)有不同用途：
    /// - 当变量被分离时：存储实际的 JSValue 值
    /// - 当变量未被分离时：存储链表节点信息和异步函数状态指针
    pub value_or_links: JSVarRefValueOrLinks,

    /// SAFETY: pvalue指针可能指向自引用(value_or_links.value)，所以!Unpin
    pub _mark: PhantomPinned,

    #[cfg(debug_assertions)]
    cleared: bool,
}

#[cfg(debug_assertions)]
impl Drop for JSVarRefInner {
    fn drop(&mut self) {
        debug_assert!(self.cleared);
        unsafe {
            debug_assert!(self.u0.header.ref_count == 0);
            debug_assert!(self.u0.header.link.prev.is_null());
            debug_assert!(self.u0.header.link.next.is_null());
        }
    }
}

impl std::fmt::Debug for JSVarRefInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JSVarRef")
            .field("var_idx", &self.var_idx)
            .field("bitfield", &self.bitfield)
            .field("pvalue", &self.pvalue)
            .field("value_or_links", &"...")
            .finish()
    }
}

impl JSVarRefInner {
    /// a dummpy var ref used as placeholder
    pub const INVALID: NonNull<Self> = NonNull::<Self>::dangling();

    pub fn is_detached(&self) -> bool {
        self.bitfield & 0b1000_0000 != 0
    }
    pub fn set_detached(&mut self, v: bool) {
        self.bitfield = (self.bitfield & !0b1000_0000) | ((v as u8) << 7)
    }

    pub fn is_arg(&self) -> bool {
        self.bitfield & 0b0100_0000 != 0
    }

    pub fn set_arg(&mut self, v: bool) {
        self.bitfield = (self.bitfield & !0b0100_0000) | ((v as u8) << 6)
    }

    pub fn var_idx(&self) -> u16 {
        self.var_idx
    }
    pub fn set_var_idx(&mut self, v: u16) {
        self.var_idx = v
    }

    /// do clean up before varref drop
    pub(crate) unsafe fn before_drop_clean_up(&mut self, rt: *mut JSRuntime) {
        debug_assert_eq!(self.u0.header.ref_count, 1);

        if self.is_detached() {
            JS_FreeValueRT(rt, self.value_or_links.value);
        } else {
            list_del(&mut self.value_or_links.links.var_ref_list); // still on the stack

            if !self.value_or_links.links.async_func.is_null() {
                async_func_free(rt, self.value_or_links.links.async_func);
            }
        }

        // remove varref from gc list, tmp list or zero ref count list
        list_del(&mut self.u0.header.link);

        #[cfg(debug_assertions)]
        {
            self.cleared = true;
            self.u0.header.ref_count = 0;
        }
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct JSVarRef(NonNull<JSVarRefInner>);

impl Clone for JSVarRef {
    fn clone(&self) -> Self {
        unsafe {
            (*self.0.as_ptr()).u0.header.ref_count += 1;
        }
        Self(self.0)
    }
}

impl Drop for JSVarRef {
    fn drop(&mut self) {
        if self.0 != JSVarRefInner::INVALID {
            unsafe {
                let p = self.0.as_mut();
                debug_assert!(p.u0.header.ref_count > 0);
                p.u0.header.ref_count -= 1;
                if p.u0.header.ref_count == 0 {
                    drop(Box::from_raw(self.0.as_ptr()));
                }
            }
        }
    }
}

impl JSVarRef {
    /// a dummpy var ref used as placeholder
    pub const PLACEHOLDER: Self = Self(JSVarRefInner::INVALID);

    pub fn new(init_value: JSValue) -> Self {
        let mut vr = Box::new(JSVarRefInner {
            u0: JSVarRefHeaderOrGCFields {
                header: JSGCObjectHeader::new(1),
            },
            var_idx: 0,
            bitfield: 0,
            pvalue: std::ptr::null_mut(), // unknown for now
            value_or_links: JSVarRefValueOrLinks { value: init_value },
            _mark: PhantomPinned,

            #[cfg(debug_assertions)]
            cleared: false,
        });

        unsafe {
            init_list_head(&mut vr.u0.header.link);
        }

        Self(unsafe { NonNull::new_unchecked(Box::into_raw(vr)) })
    }

    #[inline(always)]
    pub fn as_ptr(&self) -> *mut JSVarRefInner {
        self.0.as_ptr()
    }

    #[inline(always)]
    pub fn as_ref(&self) -> Pin<&JSVarRefInner> {
        unsafe {
            let p = self.0.as_ref();
            Pin::new_unchecked(p)
        }
    }

    #[inline(always)]
    pub fn as_mut(&mut self) -> Pin<&mut JSVarRefInner> {
        unsafe {
            let p = self.0.as_mut();
            Pin::new_unchecked(p)
        }
    }

    /// get VarRef ref_count
    #[inline(always)]
    pub fn ref_count(&self) -> usize {
        unsafe { self.0.as_ref().u0.header.ref_count as _ }
    }

    #[inline(always)]
    /// decrement VarRef ref_count
    pub unsafe fn dec_ref(&mut self) {
        debug_assert!(self.ref_count() > 0);
        unsafe {
            self.0.as_mut().u0.header.ref_count -= 1;
        }
    }

    /// if has only one ref count, consume and return inner data as Some(JSVarRefInner), otherwise None.
    pub fn into_inner(self) -> Option<Pin<Box<JSVarRefInner>>> {
        let mut s = self;
        unsafe {
            if s.0.as_ref().u0.header.ref_count == 1 {
                let p = s.0.as_ptr().cast::<JSVarRefInner>();
                s.0 = JSVarRefInner::INVALID;
                Some(Pin::from(Box::from_raw(p)))
            } else {
                None
            }
        }
    }
}

#[deprecated(note = "use JSVarRef::dispose() instead")]
pub unsafe fn free_var_ref(rt: *mut JSRuntime, var_ref: *mut JSVarRef) {
    unreachable!("unused");
}

#[deprecated(note = "use JSStackFrame::close_lexical_var() instead")]
pub unsafe fn close_lexical_var(
    ctx: *mut JSContext,
    sf: *mut JSStackFrame,
    var_idx: u16,
    is_arg: bool,
) {
    (*sf).close_lexical_var(ctx.as_mut().unwrap(), var_idx, is_arg);
}

#[deprecated(note = "use JSStackFrame::close_var_refs() instead")]
pub unsafe fn close_var_refs(ctx: *mut JSContext, sf: *mut JSStackFrame) {
    (*sf).close_var_refs(ctx.as_mut().unwrap());
}

impl JSStackFrame {
    fn make_var_ref(
        &mut self,
        ctx: &mut JSContext,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<JSVarRef, i32> {
        let mut vr = JSVarRef::new(JSValue::UNDEFINED);
        debug_assert_eq!(vr.ref_count(), 1);

        unsafe {
            let p = vr.as_mut().get_unchecked_mut();
            p.set_var_idx(var_idx);
            p.set_detached(false);
            p.set_arg(is_arg);
        }

        Ok(vr)
    }

    /// find VarRef by id and is_arg. if not found, create new.
    pub fn get_var_ref(
        &mut self,
        ctx: &mut JSContext,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<JSVarRef, i32> {
        // search in existing var refs
        if let Some(vr) = self.var_ref_list.iter().find_map(|v| {
            let vr = v.as_ref();
            if vr.var_idx() == var_idx && vr.is_arg() == is_arg {
                Some(v.clone())
            } else {
                None
            }
        }) {
            return Ok(vr);
        }

        // not found, make new var_ref
        let mut vr = self.make_var_ref(ctx, var_idx, is_arg)?;

        unsafe {
            let p = vr.as_mut().get_unchecked_mut();

            p.set_detached(false);
            p.set_arg(is_arg);
            p.set_var_idx(var_idx);
            p.value_or_links.links.async_func = if self.js_mode & JS_MODE_ASYNC != 0 {
                // The stack frame is detached and may be destroyed at any
                // time so its reference count must be increased. Calling
                // close_var_refs() when destroying the stack frame is not
                // possible because it would change the graph between the GC
                // objects. Another solution could be to temporarily detach
                // the JSVarRef of async functions during the GC. It would
                // have the advantage of allowing the release of unused stack
                // frames in a cycle.

                // `self` is from the JSAsyncFunctionState::frame field.
                // use container_of!() to get the JSAsyncFunctionState that contains `self` stack frame.

                let afs = container_of!(self as *mut JSStackFrame, JSAsyncFunctionState, frame);
                (*afs).header.ref_count += 1;
                afs
            } else {
                std::ptr::null_mut()
            };
            p.pvalue = if is_arg {
                self.arg_base_ptr.offset(var_idx as isize)
            } else {
                self.var_base_ptr.offset(var_idx as isize)
            };

            // Note:: gc obj list accepts weak ref,
            // don't increase refcount of var_ref.
            add_gc_object(
                ctx.rt_mut(),
                vr.as_ptr().cast(),
                JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_VAR_REF,
            );
        }

        // register to var ref list
        self.var_ref_list.push(vr.clone());

        Ok(vr)
    }

    pub fn close_var_refs(&mut self, ctx: &mut JSContext) {
        for var_ref in self.var_ref_list.iter_mut() {
            // no need to unlink var_ref->var_ref_link as the list is never used afterwards
            let vr = var_ref.as_ref();
            let var_idx = vr.var_idx();

            unsafe {
                if !vr.value_or_links.links.async_func.is_null() {
                    async_func_free(ctx.rt_mut(), vr.value_or_links.links.async_func);
                }

                // SAFETY: update fields of !Unpin
                let p = var_ref.as_mut().get_unchecked_mut();

                // set value
                p.value_or_links.value = if p.is_arg() {
                    *self.arg_base_ptr.offset(var_idx as isize)
                } else {
                    *self.var_base_ptr.offset(var_idx as isize)
                }
                .dup();

                // self referenced.
                p.pvalue = std::ptr::addr_of_mut!(p.value_or_links.value);

                // the reference is no longer to a local variable
                p.set_detached(true);
            }
        }

        // list_foreach_safe(&mut self.var_ref_list, |el| {
        //     let var_ref = list_entry!(el, JSVarRef, value_or_links);

        //     /* no need to unlink var_ref->var_ref_link as the list is never used afterwards */
        //     if !(*var_ref).value_or_links.links.async_func.is_null() {
        //         async_func_free(ctx.rt_mut(), (*var_ref).value_or_links.links.async_func);
        //     }

        //     let var_idx = (*var_ref).var_idx();
        //     (*var_ref).value_or_links.value = JS_DupValue(
        //         ctx,
        //         if (*var_ref).is_arg() {
        //             *self.arg_base_ptr.offset(var_idx as isize)
        //         } else {
        //             *self.var_base_ptr.offset(var_idx as isize)
        //         },
        //     );

        //     (*var_ref).pvalue = &mut (*var_ref).value_or_links.value;

        //     // the reference is no longer to a local variable
        //     (*var_ref).set_detached(true);
        // });
    }

    pub unsafe fn close_lexical_var(&mut self, ctx: &mut JSContext, var_idx: u16, is_arg: bool) {
        if let Some(i) = self
            .var_ref_list
            .iter()
            .position(|v| v.as_ref().var_idx() == var_idx && v.as_ref().is_arg() == is_arg)
        {
            // remove the var ref, list order is not cared
            let mut var_ref = self.var_ref_list.swap_remove(i);
            if !var_ref.as_ref().value_or_links.links.async_func.is_null() {
                async_func_free(
                    ctx.rt_mut(),
                    var_ref.as_ref().value_or_links.links.async_func,
                );
            }

            // SAFETY: update fields of !Unpin
            {
                let p = var_ref.as_mut().get_unchecked_mut();

                p.value_or_links.value = (*self.var_base_ptr.add(var_idx as usize)).dup();
                // self referenced.
                p.pvalue = std::ptr::addr_of_mut!(p.value_or_links.value);
                // the reference is no longer to a local variable
                p.set_detached(true);
            }
        }

        // list_foreach_safe(&mut self.var_ref_list, |el| {
        //     let var_ref = list_entry!(el, JSVarRef, value_or_links);

        //     if (*var_ref).var_idx() == var_idx && (*var_ref).is_arg() == is_arg {
        //         list_del(&mut (*var_ref).value_or_links.links.var_ref_list);

        //         if !(*var_ref).value_or_links.links.async_func.is_null() {
        //             async_func_free((*ctx).rt_mut(), (*var_ref).value_or_links.links.async_func);
        //         }

        //         (*var_ref).value_or_links.value =
        //             JS_DupValue(ctx, *self.var_base_ptr.offset(var_idx as isize));

        //         (*var_ref).pvalue = &mut (*var_ref).value_or_links.value;

        //         // the reference is no longer to a local variable
        //         (*var_ref).set_detached(true);
        //     }
        // });
    }
}
