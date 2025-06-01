use std::{marker::PhantomPinned, pin::Pin, ptr::NonNull};

use crate::{
    container_of,
    context::{JSContext, JS_MODE_ASYNC},
    gc::{GCMarkFunc, GCObject, GCObjectHeader, GCObjectNodeData, GCObjectType, RefCounted},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    utils::Node,
    value::{set_value, JSValue, JS_FreeValueRT},
};

use super::JSAsyncFunctionState;

#[repr(u8)]
#[derive(Debug)]

pub enum VarRefState {
    /// var ref is detached
    Detached { value: JSValue },

    /// reference to a target var
    Bind {
        var_idx: u16,
        async_func: *mut JSAsyncFunctionState,
    },
}

#[repr(C)]
pub struct JSVarRef {
    pub header: GCObjectHeader,

    /// bitfield for is_detached and is_arg
    pub bitfield: u8,

    pub state: VarRefState,

    /// 根据变量是否被分离(detached)有不同用途：
    /// - 当变量被分离时：存储实际的 JSValue 值(value)
    /// - 当变量未被分离时：存储链表节点信息和异步函数状态指针(var_idx, async_func)

    // /// used when is_detached = TRUE
    // pub value: JSValue,
    // /// used when is_detached = FALSE
    // /// index of the function variable on the stack
    // pub var_idx: u16,
    // /// used when is_detached = FALSE
    // /// != NULL if async stack frame
    // pub async_func: *mut JSAsyncFunctionState,

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

    fn gc_mark_children(&mut self, rt: &mut JSRuntime, mark_func: GCMarkFunc) {
        match self.state {
            VarRefState::Detached { value } => unsafe {
                rt.gc_mark_value(value, mark_func);
            },
            VarRefState::Bind { async_func, .. } => unsafe {
                if !async_func.is_null() {
                    mark_func(rt, async_func);
                }
            },
        }
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} {:4} [var_ref]", self, self.ref_count());
    }

    fn finalize(&mut self, rt: &mut JSRuntime) {
        debug_assert_eq!(self.ref_count(), 0);
        match self.state {
            VarRefState::Detached { value } => unsafe {
                JS_FreeValueRT(rt, value);
            },
            VarRefState::Bind { async_func, .. } => {
                if !async_func.is_null() {
                    JSAsyncFunctionState::free(rt, async_func);
                }
            }
        }

        #[cfg(debug_assertions)]
        {
            self.cleared = true;
        }
    }

    fn gc_node_ptr(&self) -> NonNull<Node<GCObjectNodeData>> {
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
            .field("state", &self.state)
            .field("bitfield", &self.bitfield)
            .field("pvalue", &self.pvalue)
            .finish()
    }
}

impl JSVarRef {
    /// a dummpy var ref used as placeholder
    pub const INVALID: NonNull<Pin<Self>> = NonNull::<Pin<Self>>::dangling();

    /// create a detached var ref
    pub fn new(value: JSValue) -> Pin<Box<Self>> {
        let mut vr = Box::new(Self {
            header: GCObjectHeader::new(1),
            bitfield: 0,
            state: VarRefState::Detached { value },
            pvalue: std::ptr::null_mut(), // unknown for now
            _mark: PhantomPinned,

            #[cfg(debug_assertions)]
            cleared: false,
        });

        // setup pvalue pointer
        match &vr.state {
            VarRefState::Detached { value } => {
                // DANGER: self referenced!
                let p = value as *const JSValue as *mut JSValue;
                vr.pvalue = p;
            }
            VarRefState::Bind { .. } => unreachable!(),
        }

        Pin::from(vr)
    }

    /// create a bind var ref
    pub fn new_bind(
        var_idx: u16,
        is_arg: bool,
        pvalue: *mut JSValue,
        async_func: *mut JSAsyncFunctionState,
    ) -> Pin<Box<JSVarRef>> {
        debug_assert!(!pvalue.is_null());

        let mut vr = Box::new(Self {
            header: GCObjectHeader::new(1),
            bitfield: 0,
            state: VarRefState::Bind {
                var_idx,
                async_func,
            },
            pvalue,
            _mark: PhantomPinned,

            #[cfg(debug_assertions)]
            cleared: false,
        });
        vr.set_arg(is_arg);

        Pin::from(vr)
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
                let mut node = var_ref.as_mut().gc_node_ptr();

                Node::unlink(node);
                node.as_mut().finalize(rt.as_mut().unwrap());
                drop(Box::from_raw(node.as_ptr()));
            }
        }
    }

    pub fn is_detached(&self) -> bool {
        if let VarRefState::Bind { .. } = self.state {
            true
        } else {
            false
        }
    }

    /// detach bind var ref as detached var ref, duplicate var value as detached value.
    /// this makes var ref no longer refer to a local variable.
    /// if has async funtion, free it too.
    /// if `self` is already detached, do nothing.
    pub fn detach(&mut self, rt: &mut JSRuntime) {
        if let VarRefState::Bind { async_func, .. } = self.state {
            debug_assert!(!self.pvalue.is_null());

            if !async_func.is_null() {
                JSAsyncFunctionState::free(rt, async_func);
            }

            // change state
            self.state = VarRefState::Detached {
                value: unsafe {
                    (*self.pvalue).dup() // pvalue is pointer to var value
                },
            };

            // fix pvalue to state value.
            // DANGER: self referenced.
            self.pvalue = if let VarRefState::Detached { value } = &self.state {
                value as *const JSValue as *mut JSValue
            } else {
                unreachable!()
            };
        }
    }

    #[deprecated]
    pub fn set_detached(&mut self, v: bool) {
        unreachable!("don't use this")
        // self.bitfield = (self.bitfield & !0b1000_0000) | ((v as u8) << 7)
    }

    pub fn is_arg(&self) -> bool {
        self.bitfield & 0b0100_0000 != 0
    }

    pub fn set_arg(&mut self, v: bool) {
        self.bitfield = (self.bitfield & !0b0100_0000) | ((v as u8) << 6)
    }

    /// returns id of target bind var, if state is bind.
    pub fn var_idx(&self) -> Option<u16> {
        if let VarRefState::Bind { var_idx, .. } = self.state {
            Some(var_idx)
        } else {
            None
        }
    }

    /// get async function state pointer
    pub fn async_func(&self) -> Option<NonNull<JSAsyncFunctionState>> {
        if let VarRefState::Bind { async_func, .. } = self.state {
            NonNull::new(async_func)
        } else {
            None
        }
    }

    #[deprecated]
    pub fn set_var_idx(&mut self, v: u16) {
        unreachable!("don't use this");
        // self.var_idx = v
    }

    /// get value of var.
    /// returned value is duplicated, caller should free it.
    #[inline(always)]
    pub fn value_dup(&self) -> JSValue {
        unsafe { (*self.pvalue).dup() }
    }

    /// set value of var ref, free old value.
    /// Note: `value` is consumed
    #[inline(always)]
    pub fn set_value(&mut self, ctx: &mut JSContext, value: JSValue) {
        unsafe {
            set_value(ctx, self.pvalue, value);
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
    // fn make_var_ref(
    //     &mut self,
    //     ctx: &mut JSContext,
    //     var_idx: u16,
    //     is_arg: bool,
    //     pvalue: *mut JSValue,
    // ) -> Result<Pin<Box<JSVarRef>>, i32> {
    //     let vr = JSVarRef::new_bind(var_idx, is_arg, pvalue, std::ptr::null_mut());
    //     Ok(vr)
    // }

    /// find VarRef by id and is_arg. if not found, create new.
    pub fn get_var_ref(
        &mut self,
        ctx: &mut JSContext,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<NonNull<Pin<JSVarRef>>, i32> {
        // search existing var refs
        if let Some(vr) = self
            .var_ref_list
            .iter()
            .find(|v| unsafe {
                let v = v.cast::<JSVarRef>().as_ref();
                v.var_idx() == Some(var_idx) && v.is_arg() == is_arg
            })
            .copied()
        {
            unsafe {
                vr.cast::<JSVarRef>().as_mut().inc_ref();
            }
            return Ok(vr);
        }

        // make new VarRef bind to target var_idx
        let mut vref = JSVarRef::new_bind(
            var_idx,
            is_arg,
            unsafe {
                if is_arg {
                    self.arg_base_ptr.offset(var_idx as isize)
                } else {
                    self.var_base_ptr.offset(var_idx as isize)
                }
            },
            if self.js_mode & JS_MODE_ASYNC != 0 {
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
                unsafe {
                    let af = container_of!(self as *mut JSStackFrame, JSAsyncFunctionState, frame);
                    (*af).inc_ref();
                    af
                }
            } else {
                std::ptr::null_mut()
            },
        );

        let vr_ptr: NonNull<Pin<JSVarRef>> = unsafe {
            let p = vref.as_mut().get_unchecked_mut();
            NonNull::new_unchecked((p as *mut JSVarRef).cast())
        };

        ctx.rt_mut()
            .gc_add_object(unsafe { Pin::into_inner_unchecked(vref) });

        // register to stack frame var ref list.
        // Note: inc ref_count let var_ref_list hold at least 1 strong ref.
        // unsafe {
        //     vr_ptr.cast::<JSVarRef>().as_mut().inc_ref();
        // }
        self.var_ref_list.push(vr_ptr);

        Ok(vr_ptr)
    }

    pub fn close_var_refs(&mut self, ctx: &mut JSContext) {
        for var_ref in self.var_ref_list.iter() {
            unsafe {
                var_ref.cast::<JSVarRef>().as_mut().detach(ctx.rt_mut());
            }
        }
    }

    pub fn close_lexical_var(&mut self, ctx: &mut JSContext, var_idx: u16, is_arg: bool) {
        if let Some(i) = self.var_ref_list.iter().position(|x| unsafe {
            let v = x.cast::<JSVarRef>().as_ref();
            v.var_idx() == Some(var_idx) && v.is_arg() == is_arg
        }) {
            // remove the VarRef and make it detached
            unsafe {
                self.var_ref_list
                    .swap_remove(i)
                    .cast::<JSVarRef>()
                    .as_mut()
                    .detach(ctx.rt_mut());
            }
        }
    }
}
