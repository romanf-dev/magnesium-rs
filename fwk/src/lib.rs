#![cfg_attr(not(test), no_std)]
#![feature(const_mut_refs)]
#![feature(waker_getters)]
#![allow(unused_unsafe)]

pub mod mg {
use core::mem;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker, RawWakerVTable, RawWaker};
use core::marker::PhantomData;
use core::any::Any;
use core::cell::{Cell, OnceCell};
use core::ops::{Deref, DerefMut};
use core::convert::{Into, Infallible};
use core::cmp::min;

pub const NPRIO: u8 = 16; /* Number of available hw priorities. */

#[cfg(not(test))] /* Tests provide their own stubs for interrupt masking. */
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

    fn window(&self) {
        assert!(self.old_mask == 0);
        unsafe {
            interrupt_mask(0);
            interrupt_mask(1);
        }
    }
}

impl Drop for CriticalSection {
    fn drop(&mut self) {
        unsafe { interrupt_mask(self.old_mask) };
    }
}

struct Ref<'a, T>(&'a mut T); /* non-Copy wrapper for references. */

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

    fn to_object<'a>(&self) -> Ref<'a, T> {
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
    const UNINITIALIZED: Self = Self::new();

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
        let (_, next) = self.root.links.get().unwrap();
        if next != &self.root {
            unsafe { Some(& *next) }
        } else {
            None
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
            Some(node.to_object())
        } else {
            None
        }
    }
}

pub struct Message<'a, T> {
    parent: Option<&'a Queue<'a, T>>,
    linkage: Node<Self>,
    payload: T
}

impl<'a, T> Message<'a, T> {
    pub const fn new(data: T) -> Self {
        Self {
            parent: None,
            linkage: Node::new(),
            payload: data
        }
    }
}

impl<'a, T> Linkable for Message<'a, T> {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

type MsgRef<'a, T> = Ref<'a, Message<'a, T>>;

pub struct Envelope<'a: 'static, T> {   /* Msg wrapper with Drop impl. */
    content: Option<MsgRef<'a, T>>
}

impl<'a, T> Envelope<'a, T> {
    fn new(msg: MsgRef<'a, T>) -> Self {
        Self { 
            content: Some(msg)
        }
    }
}

impl<'a, T> Into<MsgRef<'a, T>> for Envelope<'a, T> {
    fn into(mut self) -> MsgRef<'a, T> {
        self.content.take().unwrap()
    }
}

impl<'a, T> Deref for Envelope<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        let msg_ref = self.content.as_ref().unwrap();
        &msg_ref.0.payload
    }
}

impl<'a, T> DerefMut for Envelope<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        let msg_ref = self.content.as_mut().unwrap();
        &mut msg_ref.0.payload
    }
}

impl<'a, T> Drop for Envelope<'a, T> {
    fn drop(&mut self) {
        if let Some(msg) = self.content.take() {
            let parent = msg.0.parent.as_ref().unwrap();
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
            self.msgs.dequeue().map(Envelope::new)
        }
    }

    fn put_internal(&self, msg: MsgRef<'a, T>) {
        let _lock = CriticalSection::new();
        if self.subscribers.is_empty() {
            self.msgs.enqueue(msg);
        } else {
            let actor = self.subscribers.dequeue().unwrap();
            actor.0.mailbox = Some(msg.release());
            Actor::resume(actor);
        }
    }

    pub fn put(&self, msg: Envelope<'a, T>) {
        self.put_internal(msg.into())
    }
}

pub struct Pool<'a, T: Sized> {
    pool: Queue<'a, T>,
    slice: Cell<Option<&'a mut [Message<'a, T>]>>
}

impl<'a: 'static, T: Sized> Pool<'a, T> {
    pub const fn new() -> Self {
        Self {
            pool: Queue::new(),
            slice: Cell::new(None)
        }
    }

    pub fn init<const N: usize>(&self, arr: &'a mut [Message<'a, T>; N]) {
        self.pool.init();
        self.slice.set(Some(&mut arr[0..N]));
    }

    pub async fn get(&'a self) -> Envelope<'a, T> {
        if let Some(msg) = self.alloc() {
            msg
        } else {
            (&self.pool).await
        }
    }

    pub fn alloc(&'a self) -> Option<Envelope<'a, T>> {
        let _lock = CriticalSection::new();
        if let Some(slice) = self.slice.take() {
            let (item, rest) = slice.split_first_mut().unwrap();
            if rest.len() > 0 {
                self.slice.set(Some(rest));
            }
            item.parent = Some(&self.pool);
            Some(Envelope::new(Ref::new(item)))
        } else {
            self.pool.msgs.dequeue().map(Envelope::new)
        }
    }
}

pub struct Timer<'a, const N: usize> {
    timers: [List<'a, Actor>; N],
    len: [Cell<usize>; N], /* Length of the corresponding timer queue. */
    ticks: Cell<usize>
}

pub struct TimeoutFuture<'a, const N: usize> {
    container: &'a Timer<'a, N>,
    delay: Option<usize>
}

impl<'a: 'static, const N: usize> Timer<'a, N> {
    pub const fn new() -> Self {
        const ZERO: Cell<usize> = Cell::new(0);
        Self {
            timers: [List::UNINITIALIZED; N],
            len: [ZERO; N],
            ticks: Cell::new(0)
        }
    }
    
    pub fn init(&self) {
        for i in 0..N {
            self.timers[i].init()
        }
    }

    fn diff_msb(x: usize, y: usize) -> usize {
        assert!(x != y); /* Since x != y at least one bit is different. */
        let xor = x ^ y;
        let msb = (usize::BITS - xor.leading_zeros() - 1) as usize;
        min(msb, N - 1)
    }
    
    fn subscribe(&self, delay: usize, actor: Ref<'a, Actor>) {
        let _lock = CriticalSection::new();
        let ticks = self.ticks.get();
        let timeout = ticks + delay;
        let q = Self::diff_msb(ticks, timeout);
        let len = self.len[q].get();
        actor.0.timeout = Some(timeout);
        self.timers[q].enqueue(actor);
        self.len[q].set(len + 1);
    }
    
    pub fn tick(&self) {
        let lock = CriticalSection::new();
        let old_ticks = self.ticks.get();
        let new_ticks = old_ticks + 1;
        let q = Self::diff_msb(old_ticks, new_ticks);
        let len = self.len[q].replace(0);
        self.ticks.set(new_ticks);

        for _ in 0..len {
            let actor = self.timers[q].dequeue().unwrap();
            let tout = actor.0.timeout.as_ref().unwrap();
            if *tout == new_ticks {
                actor.0.timeout = None;
                Actor::resume(actor);
            } else {
                let qnext = Self::diff_msb(*tout, new_ticks);
                let qnext_len = self.len[qnext].get();
                self.timers[qnext].enqueue(actor);
                self.len[qnext].set(qnext_len + 1);
            }
            lock.window(); /* Timers processing preemption point. */
        }
    }
    
    pub fn sleep_for(&'a self, t: u32) -> TimeoutFuture<'a, N> {
        TimeoutFuture {
            container: self,
            delay: Some(t as usize)
        }
    }
}

impl<'a: 'static, const N: usize> Future for TimeoutFuture<'a, N> {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let actor_ptr = cx.waker().as_raw().data() as *mut Actor;
        let actor = unsafe { &mut *actor_ptr };
        if let Some(delay) = self.delay.take() {
            if delay != 0 {
                self.container.subscribe(delay, Ref::new(actor));
                return Poll::Pending;
            }
        }
        Poll::Ready(())
    }
}

type DynFuture = dyn Future<Output=Infallible> + 'static;
type PinnedFuture = Pin<&'static mut DynFuture>;

pub struct Actor {
    prio: u8,
    vect: u16,
    future_id: OnceCell<usize>,
    timeout: Option<usize>,
    mailbox: Option<&'static mut dyn Any>,
    context: Option<&'static Executor<'static>>,
    linkage: Node<Self>
}

impl Linkable for Actor {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

const fn waker(ptr: *mut Actor) -> RawWaker {
    const DUMMY_VTABLE: RawWakerVTable = 
        RawWakerVTable::new(|p| waker(p as *mut Actor), |_| {}, |_| {}, |_| {});
    RawWaker::new(ptr as *const (), &DUMMY_VTABLE)
}

impl Actor {
    pub const fn new(p: u8, v: u16) -> Self {
        assert!(p < NPRIO);
        Self {
            prio: p,
            vect: v,
            future_id: OnceCell::new(),
            timeout: None,
            mailbox: None,
            context: None,
            linkage: Node::new()
        }
    }

    fn call(&mut self, future: &mut PinnedFuture) {
        let raw_waker = waker(self);
        let waker = unsafe { Waker::from_raw(raw_waker) };
        let mut cx = Context::from_waker(&waker);
        let _ = future.as_mut().poll(&mut cx);
    }

    fn resume<'a: 'static>(actor: Ref<'a, Actor>) {
        let executor = actor.0.context.take().unwrap();
        executor.activate(actor.0.prio, actor.0.vect, actor);
    }
}

impl<'a: 'static, T> Future for &Queue<'a, T> {
    type Output = Envelope<'a, T>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let actor_ptr = cx.waker().as_raw().data() as *mut Actor;
        let actor = unsafe { &mut *actor_ptr };
        if let Some(any_msg) = actor.mailbox.take() {
            let msg = any_msg.downcast_mut::<Message<'a, T>>().map(Ref::new);
            let envelope = msg.map(Envelope::new).unwrap();
            Poll::Ready(envelope)
        } else {
            match self.get(Ref::new(actor)) {
                Some(msg) => Poll::Ready(msg),
                None => Poll::Pending
            }
        }
    }
}

struct ArrayAccessor<'a, T> { /* Mutable access to different array items. */
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
    ticket: Cell<usize>
}

impl<'a: 'static> Executor<'a> {
    const EMPTY: OnceCell<PinnedFuture> = OnceCell::new();

    pub const fn new() -> Self {
        Self {
            runq: [ List::UNINITIALIZED; NPRIO as usize ],
            futures: OnceCell::new(),
            ticket: Cell::new(0)
        }
    }

    fn init(&self, arr: &'a mut [OnceCell<PinnedFuture>]) {
        self.futures.set(ArrayAccessor::new(arr)).ok().unwrap();
        for i in 0..NPRIO as usize {
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
            let fut_array = self.futures.get().unwrap();
            let fut_cell = unsafe { fut_array.as_mut_item(*fut_id) };
            let future = fut_cell.get_mut().unwrap();
            actor.context = Some(self);
            actor.call(future);
        }
    }

    fn activate(&'a self, prio: u8, vect: u16, wrapper: Ref<'a, Actor>) {
        let _lock = CriticalSection::new();
        self.runq[prio as usize].enqueue(wrapper);
        unsafe { interrupt_request(vect); }
    }

    unsafe fn spawn(&'a self, actor: &mut Actor, f: &mut DynFuture) {
        let static_fut: &'static mut DynFuture = unsafe { mem::transmute(f) };
        let pinned_fut = Pin::new_unchecked(static_fut);
        let fut_id = self.ticket.get();
        let fut_array = self.futures.get().unwrap();
        let fut_cell = fut_array.as_mut_item(fut_id);
        self.ticket.set(fut_id + 1);
        fut_cell.set(pinned_fut).ok().unwrap();
        actor.future_id.set(fut_id).ok().unwrap();
        actor.context = Some(self);
        actor.call(fut_cell.get_mut().unwrap());
    }

    pub fn run<const N: usize>(
        &'a self,
        mut list: [(&mut Actor, &mut DynFuture); N]
    ) -> ! {
        let mut futures = [Self::EMPTY; N];
        let static_fut = unsafe { mem::transmute(futures.as_mut_slice()) };
        let mut pairs = list.as_mut_slice();
        self.init(static_fut);

        while let Some((pair, rest)) = pairs.split_first_mut() {
            unsafe { self.spawn(pair.0, pair.1); }
            pairs = rest;
        }

        unsafe { 
            let prev_mask = interrupt_mask(0);
            assert!(prev_mask == 1);
        }

        loop {} /* TODO: sleep... */
    }
}

unsafe impl Sync for Executor<'_> {}
unsafe impl<T: Send> Sync for Queue<'_, T> {}
unsafe impl<T: Send> Sync for Pool<'_, T> {}
unsafe impl<const N: usize> Sync for Timer<'_, N> {}

#[cfg(test)]
fn interrupt_mask(_: u8) -> u8 { 0 }

#[cfg(test)]
fn interrupt_request(_: u16) {}

#[cfg(test)]
mod tests {
use super::*;
type MsgQueue = Queue<'static, ExampleMsg>;
struct ExampleMsg(u32);
static TIMER: Timer<10> = Timer::new();
static POOL: Pool<ExampleMsg> = Pool::new();

async fn proxy(q: &MsgQueue) -> Infallible {
    loop {
        TIMER.sleep_for(100).await;
        println!("woken up");
        let mut msg = POOL.get().await;
        println!("alloc");
        msg.0 = 1;
        q.put(msg);
    }
}

async fn adder(q: &MsgQueue, sum: &mut u32) -> Infallible {
    loop {
        let msg = q.await;
        println!("adder got {}", msg.0);
        *sum += msg.0;
    }
}

#[test]
fn main() {
    const MSG_PROTOTYPE: Message<ExampleMsg> = Message::new(ExampleMsg(0));
    static mut MSG_STORAGE: [Message<ExampleMsg>; 5] = [MSG_PROTOTYPE; 5];

    static QUEUE: Queue<ExampleMsg> = Queue::new();
    static SCHED: Executor = Executor::new();
    static mut ACTOR1: Actor = Actor::new(0, 0);
    static mut ACTOR2: Actor = Actor::new(0, 1);
    static mut SUM: u32 = 0;

    POOL.init(unsafe {&mut MSG_STORAGE});
    QUEUE.init();
    TIMER.init();
    let mut f1 = proxy(&QUEUE);
    let mut f2 = adder(&QUEUE, unsafe {&mut SUM});
    
    unsafe {
        static mut FUTURES: [OnceCell<PinnedFuture>; 2] = [Executor::EMPTY; 2];
        SCHED.init(FUTURES.as_mut_slice());
        SCHED.spawn(&mut ACTOR1, &mut f1);
        SCHED.spawn(&mut ACTOR2, &mut f2);
    }

    for _ in 0..10 {
        for _ in 0..100 {
            TIMER.tick();
        }
        
        SCHED.schedule(0);
    }

    assert_eq!(unsafe {SUM}, 10);
}
}
}
