#![no_std]
#![feature(const_mut_refs)]
#![feature(waker_getters)]
#![feature(slice_take)]
#[allow(unused_unsafe)]

pub mod mg {
    
use core::mem;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker, RawWakerVTable, RawWaker};
use core::marker::PhantomData;
use core::any::Any;
use core::cell::{Cell, OnceCell};
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::convert::Infallible;

#[cfg(not(test))] 
extern "C" { 
    fn interrupt_mask(new_mask: u8) -> u8; 
    fn interrupt_request(vector: u16);
}

struct CriticalSection {
    old_mask: u8
}

impl CriticalSection {
    fn new() -> Self {
        Self {
            old_mask: unsafe { interrupt_mask(1) }
        }
    }
}

impl Drop for CriticalSection {
    fn drop(&mut self) {
        unsafe { interrupt_mask(self.old_mask) };
    }
}

struct Ref<'a, T>(&'a mut T);

impl<'a, T> Ref<'a, T> {
    fn new(r: &'a mut T) -> Self {
        Self(r)
    }
       
    fn release(self) -> &'a mut T {
        self.0
    }
}

struct Node<T> {
    links: Cell<Option<(*const Node<T>, *const Node<T>)>>,
    payload: Cell<Option<*mut T>>,
}

impl<T> Node<T> {
    const fn new() -> Node<T> {
        Node {
            links: Cell::new(None),
            payload: Cell::new(None),
        }
    }
    
    fn set_next(&self, new_next: *const Node<T>) {
        if let Some((prev, _)) = self.links.take() {
            self.links.set(Some((prev, new_next)));
        }
    }
    
    fn set_prev(&self, new_prev: *const Node<T>) {
        if let Some((_, next)) = self.links.take() {
            self.links.set(Some((new_prev, next)));
        }
    }
    
    fn unlink(&self) {   
        if let Some((prev, next)) = self.links.take() {
            unsafe {
                (*prev).set_next(next);
                (*next).set_prev(prev);
            }
        }
    }

    fn to_obj<'a>(&self) -> Ref<'a, T> {
        let ptr = self.payload.take().unwrap();
        unsafe { Ref::new(&mut *ptr) }
    }
}

trait Linkable: Sized {
    fn to_links(&self) -> &Node<Self>;
}

struct List<'a, T: Linkable> {
    root: Node<T>,
    _marker: PhantomData<Cell<&'a T>>,
}

impl<'a, T: Linkable> List<'a, T> {
    const fn new() -> List<'a, T> {
        List {
            root: Node::new(),
            _marker: PhantomData
        }
    }

    fn init(&self) {
        let this = &self.root as *const Node<T>;
        self.root.links.set(Some((this, this)));
    }

    fn peek_head_node(&self) -> Option<&Node<T>> {
        match self.root.links.get() { 
            Some((_, next)) => 
                if next != &self.root { 
                    unsafe { Some(&*next) } 
                } else { 
                    None 
                },
            _ => None
        }
    }
    
    fn is_empty(&self) -> bool {
        self.peek_head_node().is_none()
    }
    
    fn enqueue(&self, wrapper: Ref<'a, T>) {
        let object = wrapper.release();
        let ptr: *mut T = object;
        let node = object.to_links();
        let (prev, next) = self.root.links.take().unwrap();
        node.links.set(Some((prev, &self.root)));
        node.payload.set(Some(ptr));
        self.root.links.set(Some((node, next)));
        unsafe { (*prev).set_next(node); }
    }

    fn dequeue(&self) -> Option<Ref<'a, T>> {
        if let Some(node) = self.peek_head_node() {
            node.unlink();
            Some(node.to_obj())
        } else {
            None
        }
    }
}

pub struct Message<'a, T> {
    parent: OnceCell<&'a Queue<'a, T>>,
    linkage: Node<Self>,
    payload: T
}

impl<'a, T> Message<'a, T> {
    pub const fn new(payload: T) -> Self {
        Self { 
            parent: OnceCell::new(), 
            linkage: Node::new(), 
            payload: payload
        }        
    }
}

impl<'a, T> Linkable for Message<'a, T> {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

pub struct Envelope<'a: 'static, T> {
    inner: Option<Ref<'a, Message<'a, T>>>
}

impl<'a, T> Envelope<'a, T> {    
    fn from_wrapper(msg: Option<Ref<'a, Message<'a, T>>>) -> Option<Self> {
        msg.map(|m| { Self { inner: Some(m) } })
    }
    
    fn into_wrapper(mut self) -> Ref<'a, Message<'a, T>> {
        self.inner.take().unwrap()
    }
}

impl<'a, T> Deref for Envelope<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner.as_ref().unwrap().0.payload
    }
}

impl<'a, T> DerefMut for Envelope<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner.as_mut().unwrap().0.payload
    }
}

impl<'a, T> Drop for Envelope<'a, T> {
    fn drop(&mut self) {
        if let Some(msg) = self.inner.take() {
            let parent = msg.0.parent.get().unwrap();
            parent.put_internal(msg);
        }
    }
}

pub struct Queue<'a, T: Sized> {
    msgs: List<'a, Message<'a, T>>,
    subscribers: List<'a, Actor>
}

impl<'a: 'static, T: Sized> Queue<'a, T> {
    pub const fn new() -> Self {
        Self { 
            msgs: List::new(), 
            subscribers: List::new(),
        }         
    }
    
    pub fn init(&self) {
        self.msgs.init();
        self.subscribers.init();
    }
        
    fn get(&self, actor: Ref<'a, Actor>) -> Option<Envelope<'a, T>> {
        let _lock = CriticalSection::new();
        if self.msgs.is_empty() {
            self.subscribers.enqueue(actor);
            None
        } else {
            Envelope::from_wrapper(self.msgs.dequeue())
        }
    }
    
    fn put_internal(&self, msg: Ref<'a, Message<'a, T>>) {
        let _lock = CriticalSection::new();
        if self.subscribers.is_empty() {
            self.msgs.enqueue(msg);
        } else {
            let actor = self.subscribers.dequeue().unwrap();
            Actor::set(actor, msg);
        }        
    }
    
    pub fn put(&self, msg: Envelope<'a, T>) {
        self.put_internal(msg.into_wrapper())
    }
}

pub struct Pool<'a, T: Sized, const N: usize> {
    pool: Queue<'a, T>,
    used: Cell<usize>,
    slice: Cell<Option<&'a mut [Message<'a, T>]>>
}

impl<'a: 'static, T: Sized, const N: usize> Pool<'a, T, N> {
    pub const fn new() -> Self {
        Self {
            pool: Queue::new(), 
            used: Cell::new(0), 
            slice: Cell::new(None)             
        }
    }
    
    pub fn init(&self, arr: &'a mut [Message<'a, T>; N]) {
        self.pool.init();
        self.slice.set(Some(&mut arr[0..N]));
    }
    
    pub fn alloc(&'a self) -> Option<Envelope<'a, T>> {
        let _lock = CriticalSection::new();
        let used = self.used.get();
        let msg = if used < N {
            let remaining_items = self.slice.take().unwrap();
            let (item, rest) = remaining_items.split_at_mut(1);
            self.used.set(used + 1);
            self.slice.set(Some(rest));
            item[0].parent.set(&self.pool).ok().unwrap();
            Some(Ref::new(&mut item[0]))
        } else {
            self.pool.msgs.dequeue()
        };
        
        Envelope::from_wrapper(msg)
    }
}

type DynFuture = dyn Future<Output=Infallible> + 'static;
type PinnedFuture = Pin<&'static mut DynFuture>;

pub struct Actor {
    prio: u8,
    vect: u16,
    future_id: OnceCell<usize>,
    mailbox: Option<&'static mut dyn Any>,
    context: Option<&'static Executor<'static>>,
    linkage: Node<Self>
}

impl Linkable for Actor {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

const fn noop_raw_waker(ptr: *mut Actor) -> RawWaker {
    const DUMMY_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |p| noop_raw_waker(p as *mut Actor), |_| {}, |_| {}, |_| {}
    );

    RawWaker::new(ptr as *const (), &DUMMY_VTABLE)
}

pub const NPRIO: u8 = 16;

impl Actor {
    pub const fn new(p: u8, v: u16) -> Self {
        assert!(p < NPRIO);
        
        Self {
            prio: p,
            vect: v,
            future_id: OnceCell::new(),
            mailbox: None, 
            context: None,
            linkage: Node::new() 
        }    
    }
     
    fn call(&mut self, f: &mut OnceCell<PinnedFuture>) {
        let ptr: *mut Actor = self;
        let raw_waker = noop_raw_waker(ptr);
        let waker = unsafe { Waker::from_raw(raw_waker) };
        let mut cx = core::task::Context::from_waker(&waker);
        let future = f.get_mut().unwrap();
        let _ = future.as_mut().poll(&mut cx);
    }
    
    fn set<'a: 'static, T>(this: Ref<'a, Actor>, msg: Ref<'a, Message<'a, T>>) {
        let actor = this.release();
        let exec = actor.context.take().unwrap();
        actor.mailbox = Some(msg.release());
        exec.activate(actor.prio, actor.vect, Ref::new(actor));
    }
}

impl<'q: 'static, T> Future for &Queue<'q, T> {
    type Output = Envelope<'q, T>;
    
    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let actor_ptr = cx.waker().as_raw().data() as *mut Actor;
        let actor = unsafe { &mut *actor_ptr };
        if let Some(any_msg) = actor.mailbox.take() {
            let msg = any_msg.downcast_mut::<Message<'q, T>>().map(
                |msg| Ref::new(msg)
            );
            let envelope = Envelope::from_wrapper(msg).unwrap();
            Poll::Ready(envelope)            
        } else {
            match self.get(Ref::new(actor)) {
                Some(msg) =>  Poll::Ready(msg),
                None => Poll::Pending
            }
        }
    }
}

struct ArrayAccessor<'a, T> {
    ptr: *mut T,
    len: usize,
    _marker: PhantomData<&'a mut T>
}

impl<'a, T> ArrayAccessor<'a, T> {
    fn new(slice: &'a mut [T]) -> ArrayAccessor<'a, T> {
        Self {
            ptr: slice.as_mut_ptr(),
            len: slice.len(),
            _marker: PhantomData
        }
    }

    unsafe fn as_mut_item(&self, i: usize) -> &'a mut T {
        assert!(i < self.len);
        &mut *self.ptr.add(i)
    }
}

pub struct Executor<'a> {
    runq: [List<'a, Actor>; NPRIO as usize ],
    futures: OnceCell<ArrayAccessor<'a, OnceCell<PinnedFuture>>>,
    ticket: AtomicUsize
}

impl<'a: 'static> Executor<'a> {
    const NPRIO_USIZE: usize = NPRIO as usize;
    const EMPTY_CELL: OnceCell<PinnedFuture> = OnceCell::new();
     
    pub const fn new() -> Self {
        const RUNQ_PROTO: List::<Actor> = List::<Actor>::new();
        
        Self { 
            runq: [ RUNQ_PROTO; Self::NPRIO_USIZE ], 
            futures: OnceCell::new(), 
            ticket: AtomicUsize::new(0) 
        } 
    }
    
    fn init(&self, arr: &'a mut [OnceCell<PinnedFuture>]) {
        self.futures.set(ArrayAccessor::new(arr)).ok().unwrap();
        for i in 0..Self::NPRIO_USIZE {
            self.runq[i].init()
        }
    }
    
    fn extract(&self, vect: u16) -> Option<Ref<'a, Actor>> {
        let _lock = CriticalSection::new();
        let runq = &self.runq[vect as usize];
        runq.dequeue()
    }

    pub fn schedule(&'a self, vect: u16) {       
        while let Some(wrapper) = self.extract(vect) {
            let actor = wrapper.release();
            let fut_id = actor.future_id.get().unwrap();
            let futures = self.futures.get().unwrap();
            let f = unsafe { futures.as_mut_item(*fut_id) };
            actor.context = Some(self);
            actor.call(f);
        }
    }
    
    fn activate(&'a self, prio: u8, vect: u16, wrapper: Ref<'a, Actor>) {
        let _lock = CriticalSection::new();
        self.runq[prio as usize].enqueue(wrapper);
        unsafe { interrupt_request(vect); }
    }
    
    unsafe fn into_static<T: ?Sized>(v: &mut T) -> &'static mut T {
        mem::transmute(v)
    } 
        
    unsafe fn spawn(&'a self, actor: &mut Actor, f: &mut DynFuture) {
        let static_fut = Self::into_static(f);
        let pinned_fut = Pin::new_unchecked(static_fut);
        let fut_id = self.ticket.fetch_add(1, Ordering::SeqCst);
        let futures = self.futures.get().unwrap();
        let f = futures.as_mut_item(fut_id);
        f.set(pinned_fut).ok().unwrap();
        actor.future_id.set(fut_id).ok().unwrap();
        actor.context = Some(self);
        actor.call(f);
    }
    
    pub fn run<const N: usize>(
        &'a self,
        mut list: [(&mut Actor, &mut DynFuture); N]) -> ! {

        let mut futures = [Self::EMPTY_CELL; N];
        let static_fut = unsafe { Self::into_static(futures.as_mut_slice()) };
        self.init(static_fut);
        let mut pairs = list.as_mut_slice();
        
        while let Some(pair) = pairs.take_first_mut() {
            unsafe { self.spawn(pair.0, pair.1); }
        }
        
        unsafe { let _ = interrupt_mask(0); }
        loop {}
    }    
}

unsafe impl Sync for Executor<'_> {}
unsafe impl<T> Sync for Queue<'_, T> where T: Send {}
unsafe impl<T, const N: usize> Sync for Pool<'_, T, N> where T: Send {}

//----------------------------------------------------------------------------

#[cfg(test)] 
fn interrupt_mask(_: u8) -> u8 { 0 }

#[cfg(test)] 
fn interrupt_request(_: u16) {}

#[cfg(test)]
mod tests {
    use super::*;
    
    type MsgQueue<'a> = Queue<'a, ExampleMsg>;

    struct ExampleMsg {
        n: u32
    }
    
    async fn proxy<'a: 'static>(q1: &MsgQueue<'a>, q2: &MsgQueue<'a>) -> Infallible {
        loop {            
            let msg = q1.await;
            q2.put(msg);
        }
    }
    
    async fn adder<'a: 'static>(q1: &MsgQueue<'a>, sum: &mut u32) -> Infallible {
        loop {            
            let msg = q1.await;
            *sum += msg.n;
        }
    }
        
    #[test]
    fn chain() {
        const MSG_PROTO: Message<ExampleMsg> = Message::new(ExampleMsg { n: 0 });
        static mut MSG_STORAGE: [Message<ExampleMsg>; 5] = [MSG_PROTO; 5];
        
        static POOL: Pool<ExampleMsg, 5> = Pool::new();
        static QUEUE1: Queue<ExampleMsg> = Queue::new();
        static QUEUE2: Queue<ExampleMsg> = Queue::new();
        static SCHED: Executor = Executor::new();
        
        static mut FUTURES: [OnceCell<PinnedFuture>; 2] = [Executor::EMPTY_CELL; 2];
        static mut ACTOR1: Actor = Actor::new(0, 0);
        static mut ACTOR2: Actor = Actor::new(0, 1);

        static mut SUM: u32 = 0;

        QUEUE1.init();
        QUEUE2.init();

        let mut f1 = proxy(&QUEUE1, &QUEUE2);
        
        unsafe {
            let mut f2 = adder(&QUEUE2, &mut SUM);
            
            POOL.init(&mut MSG_STORAGE);
            SCHED.init(FUTURES.as_mut_slice());
            SCHED.spawn(&mut ACTOR1, &mut f1);
            SCHED.spawn(&mut ACTOR2, &mut f2);
        }

        for _ in 0..10 {
            let mut msg = POOL.alloc().unwrap();
            msg.n = 1;
            QUEUE1.put(msg);
            
            SCHED.schedule(0);
        }
    
        unsafe { assert_eq!(SUM, 10); }
    }    
}

}
