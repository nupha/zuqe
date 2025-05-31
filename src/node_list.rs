use std::{
    marker::PhantomData,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

pub struct Node<T: Deref> {
    prev: *mut Self,
    next: *mut Self,
    data: T,
}

impl<T: Deref> Deref for Node<T> {
    type Target = <T as Deref>::Target;

    fn deref(&self) -> &Self::Target {
        self.data.deref()
    }
}

impl<T: DerefMut> DerefMut for Node<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data.deref_mut()
    }
}

#[cfg(debug_assertions)]
impl<T: Deref> Drop for Node<T> {
    fn drop(&mut self) {
        debug_assert!(self.prev.is_null());
        debug_assert!(self.next.is_null());
        // unsafe {
        //     if !self.prev.is_null() {
        //         drop(Box::from_raw(self.prev));
        //     }
        //     if !self.next.is_null() {
        //         drop(Box::from_raw(self.next));
        //     }
        // }
    }
}

impl<T: Deref> Node<T> {
    pub fn new(mut data: T, set_link: impl FnOnce(&mut T, NonNull<Node<T>>)) -> Box<Self> {
        unsafe {
            let ptr = {
                let ly = core::alloc::Layout::from_size_align_unchecked(
                    core::mem::size_of::<Self>(),
                    core::mem::align_of::<Self>(),
                );
                std::alloc::alloc(ly).cast::<Self>()
            };
            debug_assert!(!ptr.is_null(), "fail to alloc"); // TODO

            set_link(&mut data, NonNull::new_unchecked(ptr));

            ptr.write(Self {
                prev: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
                data,
            });

            Box::from_raw(ptr)
        }
    }

    pub fn into_inner(self) -> T {
        unsafe {
            let r = std::ptr::read(&self.data);

            if !self.prev.is_null() {
                drop(Box::from_raw(self.prev));
            }
            if !self.next.is_null() {
                drop(Box::from_raw(self.next));
            }

            std::mem::forget(self);
            r
        }
    }

    #[deprecated(note = "use Node::detach_from_list()")]
    pub fn remove_from_list(&mut self) {
        unsafe {
            Self::detach_from_list(NonNull::from(self));
        }
    }

    pub unsafe fn detach_from_list(mut node: NonNull<Node<T>>) {
        match (node.as_mut().prev.as_mut(), node.as_mut().next.as_mut()) {
            (Some(before), Some(after)) => {
                before.next = after;
                after.prev = before;
            }
            (Some(before), None) => {
                before.next = std::ptr::null_mut();
                node.as_mut().prev = std::ptr::null_mut();
                node.as_mut().next = std::ptr::null_mut();
            }
            (None, None) => {}
            (None, Some(_)) => {
                unreachable!();
            }
        }

        node.as_mut().prev = std::ptr::null_mut();
        node.as_mut().next = std::ptr::null_mut();
    }
}

pub struct ListNodeIter<'a, T: Deref> {
    cursor: *const Node<T>,
    _mark: PhantomData<&'a T>,
}

impl<'a, T: Deref> Iterator for ListNodeIter<'a, T> {
    type Item = &'a Node<T>;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if let Some(c) = self.cursor.as_ref() {
                self.cursor = c.next;
                Some(c)
            } else {
                None
            }
        }
    }
}

pub struct ListNodeIterMut<'a, T: DerefMut> {
    cursor: *mut Node<T>,
    _mark: PhantomData<&'a T>,
}

impl<'a, T: DerefMut> Iterator for ListNodeIterMut<'a, T> {
    type Item = &'a mut Node<T>;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if let Some(c) = self.cursor.as_mut() {
                self.cursor = c.next;
                Some(c)
            } else {
                None
            }
        }
    }
}

pub struct NodeList<T: DerefMut> {
    dummy_node: ManuallyDrop<Box<Node<T>>>,
}

impl<T: DerefMut> Drop for NodeList<T> {
    fn drop(&mut self) {
        debug_assert!(self.dummy_node.prev.is_null());
        debug_assert!(self.dummy_node.next.is_null());

        // if !self.dummy_node.next.is_null() {
        //     drop(unsafe { Box::from_raw(self.dummy_node.next) });
        // }
    }
}

impl<T: DerefMut> NodeList<T> {
    pub fn new() -> Self {
        let n = unsafe {
            let ly = core::alloc::Layout::from_size_align(
                core::mem::size_of::<Node<T>>(),
                core::mem::align_of::<Node<T>>(),
            )
            .unwrap();
            let p = std::alloc::alloc_zeroed(ly).cast::<Node<T>>();
            Box::from_raw(p)
        };

        Self {
            dummy_node: ManuallyDrop::new(n),
        }
    }

    pub unsafe fn push_raw(&mut self, mut node: NonNull<Node<T>>) {
        debug_assert!(node.as_ref().prev.is_null());
        debug_assert!(node.as_ref().next.is_null());

        if let Some(n) = self.dummy_node.next.as_mut() {
            node.as_mut().next = n;
            n.prev = node.as_ptr();
        }

        self.dummy_node.next = node.as_ptr();
        node.as_mut().prev = self.dummy_node.as_mut();
    }

    pub fn push(&mut self, node: Box<Node<T>>) -> NonNull<Node<T>> {
        unsafe {
            let p = NonNull::new_unchecked(Box::into_raw(node));
            self.push_raw(p);
            p
        }
    }

    pub fn pop(&mut self) -> Option<Box<Node<T>>> {
        unsafe {
            if let Some(top) = self.dummy_node.next.as_mut() {
                if let Some(a) = top.next.as_mut() {
                    a.prev = self.dummy_node.as_mut();
                    self.dummy_node.as_mut().next = a;
                } else {
                    self.dummy_node.as_mut().next = std::ptr::null_mut();
                }

                top.prev = std::ptr::null_mut();
                top.next = std::ptr::null_mut();
                Some(Box::from_raw(top))
            } else {
                None
            }
        }
    }

    pub fn iter(&self) -> ListNodeIter<T> {
        ListNodeIter {
            cursor: self.dummy_node.next,
            _mark: PhantomData,
        }
    }

    pub fn iter_mut(&mut self) -> ListNodeIterMut<T> {
        ListNodeIterMut {
            cursor: self.dummy_node.next,
            _mark: PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.dummy_node.next.is_null()
    }
}
