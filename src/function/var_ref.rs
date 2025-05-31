use std::{marker::PhantomPinned, pin::Pin, ptr::NonNull};

use crate::{
    container_of,
    context::{JSContext, JS_MODE_ASYNC},
    gc::{GCObject, GCObjectNodeData, GCObjectType, JSGCObjectHeader, RefCounted},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    utils::Node,
    value::{JSValue, JS_FreeValueRT},
};

use super::JSAsyncFunctionState;

#[repr(C)]
pub struct JSVarRef {
    pub header: JSGCObjectHeader,

    /// bitfield for is_detached and is_arg
    pub bitfield: u8,

    /// 根据变量是否被分离(detached)有不同用途：
    /// - 当变量被分离时：存储实际的 JSValue 值(value)
    /// - 当变量未被分离时：存储链表节点信息和异步函数状态指针(var_idx, async_func)

    /// used when is_detached = TRUE
    pub value: JSValue,

    /// used when is_detached = FALSE
    /// index of the function variable on the stack
    pub var_idx: u16,
    /// used when is_detached = FALSE
    /// != NULL if async stack frame
    pub async_func: *mut JSAsyncFunctionState,

    /// 一个指向 JSValue 的指针。这个字段有两个主要用途：
    /// 1. 变量值的定位 ：
    ///    - 当变量在栈上时（detached = false）， pvalue 指向栈上的变量值位置（ var_buf 或 arg_buf ）
    ///    - 当变量被分离时（detached = true）， pvalue 指向 JSVarRef 结构体内部的 value_or_links.value 字段
    /// 2. 闭包变量访问 ：
    ///    - 它允许闭包函数访问外部作用域的变量
    ///    - 通过这个指针可以实现对变量的读写操作，无论变量实际存储在栈上还是已经被分离到堆上
    pub pvalue: *mut JSValue,

    #[cfg(debug_assertions)]
    cleared: bool,

    /// SAFETY: pvalue指针可能指向自引用(value)，所以!Unpin
    pub _mark: PhantomPinned,
}

impl RefCounted for JSVarRef {
    fn ref_count(&self) -> i32 {
        self.header.ref_count
    }

    fn inc_ref(&mut self) -> i32 {
        self.header.ref_count += 1;
        self.header.ref_count
    }

    fn dec_ref(&mut self) -> i32 {
        self.header.ref_count -= 1;
        self.header.ref_count
    }
}

impl GCObject for JSVarRef {
    fn gc_type(&self) -> GCObjectType {
        GCObjectType::VarRef(NonNull::from(self).cast::<Pin<JSVarRef>>())
    }

    fn gc_mark(&self) -> u8 {
        self.header.gc_mark()
    }

    fn set_gc_mark(&mut self, mark: u8) {
        self.header.set_gc_mark(mark);
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} {:4} [var_ref]", self, self.ref_count());
    }

    fn finalize(&mut self, rt: &mut JSRuntime) {
        debug_assert_eq!(self.ref_count(), 0);

        if self.is_detached() {
            unsafe {
                JS_FreeValueRT(rt, self.value);
            }
        } else {
            if !self.async_func.is_null() {
                JSAsyncFunctionState::free(rt, self.async_func);
            }
        }

        #[cfg(debug_assertions)]
        {
            self.cleared = true;
        }
    }

    fn gc_list_node(&self) -> NonNull<Node<GCObjectNodeData>> {
        unsafe {
            debug_assert!(!self.header.list_node.is_null());
            NonNull::new_unchecked(self.header.list_node)
        }
    }
}

#[cfg(debug_assertions)]
impl Drop for JSVarRef {
    fn drop(&mut self) {
        debug_assert_eq!(self.ref_count(), 0);
        debug_assert!(self.cleared);
    }
}

impl std::fmt::Debug for JSVarRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JSVarRef")
            .field("var_idx", &self.var_idx)
            .field("bitfield", &self.bitfield)
            .field("pvalue", &self.pvalue)
            .field("value_or_links", &"...")
            .finish()
    }
}

impl JSVarRef {
    /// a dummpy var ref used as placeholder
    pub const INVALID: NonNull<Pin<Self>> = NonNull::<Pin<Self>>::dangling();

    pub fn new(value: JSValue) -> Pin<Box<Self>> {
        let vr = Box::pin(Self {
            header: JSGCObjectHeader::new(1),
            bitfield: 0,
            pvalue: std::ptr::null_mut(), // unknown for now

            value,

            var_idx: 0,
            async_func: std::ptr::null_mut(),

            #[cfg(debug_assertions)]
            cleared: false,

            _mark: PhantomPinned,
        });

        vr
    }

    pub fn free(rt: *mut JSRuntime, mut var_ref: NonNull<Self>) {
        unsafe {
            debug_assert!(
                var_ref.as_ref().ref_count() > 0,
                "var ref ref_count={}",
                var_ref.as_ref().ref_count()
            );
        }

        unsafe {
            if var_ref.as_mut().dec_ref() == 0 {
                // remove from gc list, do finalize clean up and drop
                let mut node = var_ref.as_mut().gc_list_node();

                Node::unlink(node);
                node.as_mut().finalize(rt.as_mut().unwrap());
                drop(Box::from_raw(node.as_ptr()));
            }
        }
    }

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
    ) -> Result<Pin<Box<JSVarRef>>, i32> {
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
    ) -> Result<NonNull<Pin<JSVarRef>>, i32> {
        // search in existing var refs
        if let Some(vr) = self
            .var_ref_list
            .iter()
            .find(|v| unsafe {
                let v = v.as_ptr().cast::<JSVarRef>();
                (*v).var_idx() == var_idx && (*v).is_arg() == is_arg
            })
            .copied()
        {
            unsafe {
                (*vr.as_ptr().cast::<JSVarRef>()).inc_ref();
            }
            return Ok(vr);
        }

        // make new VarRef
        let mut vr = self.make_var_ref(ctx, var_idx, is_arg)?;
        let vr_ptr: NonNull<Pin<JSVarRef>>;

        unsafe {
            let p = vr.as_mut().get_unchecked_mut();
            vr_ptr = NonNull::new_unchecked((p as *mut JSVarRef).cast());

            // for detached=false, var_idx, async_func is meaningful.
            p.set_detached(false);
            p.set_arg(is_arg);
            p.set_var_idx(var_idx);
            p.async_func = if self.js_mode & JS_MODE_ASYNC != 0 {
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

                let af = container_of!(self as *mut JSStackFrame, JSAsyncFunctionState, frame);
                (*af).inc_ref();
                af
            } else {
                std::ptr::null_mut()
            };
            p.pvalue = if is_arg {
                self.arg_base_ptr.offset(var_idx as isize)
            } else {
                self.var_base_ptr.offset(var_idx as isize)
            };

            ctx.rt_mut().gc_add_object(Pin::into_inner_unchecked(vr));
        }

        // register to stack frame var ref list.
        // Note: inc ref_count will let var_ref_list hold at least 1 strong ref.
        unsafe {
            vr_ptr.cast::<JSVarRef>().as_mut().inc_ref();
        }
        self.var_ref_list.push(vr_ptr);

        Ok(vr_ptr)
    }

    pub fn close_var_refs(&mut self, ctx: &mut JSContext) {
        for var_ref in self.var_ref_list.iter() {
            unsafe {
                let vr = var_ref.as_ptr().cast::<JSVarRef>().as_mut().unwrap();

                let var_idx = vr.var_idx();
                if !vr.async_func.is_null() {
                    JSAsyncFunctionState::free(ctx.rt_mut(), vr.async_func);
                }

                vr.value = if vr.is_arg() {
                    *self.arg_base_ptr.add(var_idx as usize)
                } else {
                    *self.var_base_ptr.add(var_idx as usize)
                }
                .dup();

                // Note: self referenced.
                vr.pvalue = std::ptr::addr_of_mut!(vr.value);

                // the reference is no longer to a local variable
                vr.set_detached(true);
            }
        }
    }

    pub fn close_lexical_var(&mut self, ctx: &mut JSContext, var_idx: u16, is_arg: bool) {
        if let Some(i) = self.var_ref_list.iter().position(|x| unsafe {
            let v = x.as_ptr().cast::<JSVarRef>();
            (*v).var_idx() == var_idx && (*v).is_arg() == is_arg
        }) {
            // remove the VarRef
            let var_ref = self.var_ref_list.swap_remove(i);

            unsafe {
                let var_ref = var_ref.cast::<JSVarRef>().as_mut();

                if !var_ref.async_func.is_null() {
                    JSAsyncFunctionState::free(ctx.rt_mut(), var_ref.async_func);
                }

                var_ref.value = (*self.var_base_ptr.add(var_idx as usize)).dup();
                // DANGER: self referenced.
                var_ref.pvalue = std::ptr::addr_of_mut!((*var_ref).value);
                // the reference is no longer to a local variable
                var_ref.set_detached(true);
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
