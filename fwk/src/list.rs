/*
 * Intrusive doubly-linked lists and queues.
 * This module isn't reexported so public items are only visible in the crate.
 */

use core::cell::Cell;
use core::marker::{ PhantomData, PhantomPinned };
use core::ptr;
use core::ops::{Deref, DerefMut};

pub struct Node<T> {
    links: Cell<(*const Node<T>, *const Node<T>)>,
    payload: Cell<Option<*const T>>,
    _marker: PhantomPinned,
}

pub trait Linkable: Sized {
    fn to_links(&self) -> &Node<Self>;
}

impl<T> Node<T> {
    pub const fn new() -> Self {
        Node {
            links: Cell::new((ptr::null(), ptr::null())),
            payload: Cell::new(None),
            _marker: PhantomPinned,
        }
    }

    fn set_next(&self, new_next: *const Node<T>) {
        let (prev, _) = self.links.get();
        self.links.set((prev, new_next));
    }

    fn set_prev(&self, new_prev: *const Node<T>) {
        let (_, next) = self.links.get();
        self.links.set((new_prev, next));
    }

    unsafe fn unlink(&self) -> Option<*const T> {
        self.payload.take().inspect(|_| {
            let (prev, next) = self.links.take();
            (*prev).set_next(next);
            (*next).set_prev(prev);
        })
    }
}

struct GenericList<'a, T: Linkable> {
    root: Node<T>,
    _marker: PhantomData<Cell<&'a T>>, /* invariant over both 'a and T. */
}

impl<'a, T: Linkable> GenericList<'a, T> {
    const fn new() -> Self {
        GenericList {
            root: Node::new(),
            _marker: PhantomData,
        }
    }

    fn init(&self) {
        let this = &self.root as *const Node<T>;
        self.root.links.set((this, this));
    }

    fn peek_head(&self) -> Option<&'a Node<T>> {
        let (_, next) = self.root.links.get();
        let nonempty = next != &self.root;
        nonempty.then(|| unsafe { &*next })
    }

    fn append(&self, node: &'a Node<T>) -> &'a Node<T> {
        let (prev, next) = self.root.links.take();
        node.links.set((prev, &self.root));
        self.root.links.set((node, next));
        unsafe {
            (*prev).set_next(node);
        }
        node
    }
}

pub struct QueueRef<'a, T: Linkable> {
    list: GenericList<'a, T>, /* may only contain shared refs to T. */
}

impl<'a, T: Linkable> QueueRef<'a, T> {
    pub const fn new() -> Self {
        Self {
            list: GenericList::new(),
        }
    }

    pub fn init(&self) {
        self.list.init();
    }

    pub fn enqueue(&self, object: &'a T) -> &'a Node<T> {
        let ptr: *const T = object;
        let node = object.to_links();
        node.payload.set(Some(ptr));
        self.list.append(node)
    }

    pub fn dequeue(&self) -> Option<&'a T> {
        self.list
            .peek_head()
            .map(|node| unsafe { &*node.unlink().unwrap() })
    }
}

/*
 * Container of mutable refs manages them as non-copyable opaque objects Mut.
 */
pub struct Mut<'a, T>(&'a mut T);

impl<'a, T> Mut<'a, T> {
    pub fn new(r: &'a mut T) -> Self {
        Self(r)
    }

    pub fn release(self) -> &'a mut T {
        self.0
    }
}

impl<T> Deref for Mut<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0
    }
}

impl<T> DerefMut for Mut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0
    }
}

pub struct QueueMut<'a, T: Linkable> {
    list: GenericList<'a, T>, /* may only contain mutable refs to T. */
}

impl<'a, T: Linkable> QueueMut<'a, T> {
    pub const fn new() -> Self {
        Self {
            list: GenericList::new(),
        }
    }

    pub fn init(&self) {
        self.list.init();
    }

    pub fn enqueue(&self, wrapper: Mut<'a, T>) -> &'a Node<T> {
        let object = wrapper.release();
        let ptr: *mut T = object;
        let node = object.to_links();
        node.payload.set(Some(ptr));
        self.list.append(node)
    }

    pub fn dequeue(&self) -> Option<Mut<'a, T>> {
        self.list
            .peek_head()
            .map(|node| unsafe { Mut::new(&mut *(node.unlink().unwrap() as *mut T)) })
    }
}
