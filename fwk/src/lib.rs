#![cfg_attr(not(test), no_std)]
#![allow(unused_unsafe)] /* for test stubs */

pub mod mg {
use core::mem::{MaybeUninit};
use core::mem;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker, RawWakerVTable, RawWaker};
use core::marker::PhantomData;
use core::cell::{Cell};
use core::ops::{Deref, DerefMut};
use core::convert::{Into, Infallible};
use core::cmp::min;

pub const NPRIO: u8 = 16; /* Number of available hw priorities. */

#[cfg(not(test))] /* Tests provide their own stubs for interrupt masking. */
extern "C" {
    fn interrupt_mask(new_mask: u8) -> u8;
    fn interrupt_request(vector: u16);
    fn interrupt_prio(vector: u16) -> u8;
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

struct Ref<'a, T>(&'a mut T); /* wrapper for muts to avoid reborrows */

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

    fn unlink<'a>(&self) -> Option<Ref<'a, T>> {
        self.links.take().map(|(prev, next)| {
            let ptr = self.payload.take().unwrap();
            unsafe {
                (*prev).set_next(next);
                (*next).set_prev(prev);
                Ref::new(&mut *ptr)
            }
        })
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

    fn enqueue(&self, wrapper: Ref<'a, T>) -> &Node<T> {
        let object = wrapper.release();
        let ptr: *mut T = object;
        let node = object.to_links();
        let (prev, next) = self.root.links.take().unwrap();
        node.links.set(Some((prev, &self.root)));
        node.payload.set(Some(ptr));
        self.root.links.set(Some((node, next)));
        unsafe { (*prev).set_next(node); }
        node
    }

    fn dequeue(&self) -> Option<Ref<'a, T>> {
        self.peek_head_node().map(|node| {
            node.unlink().unwrap()
        })
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
        &self.content.as_ref().unwrap().0.payload
    }
}

impl<'a, T> DerefMut for Envelope<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.content.as_mut().unwrap().0.payload
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

struct QWaitBlk<'a, T: Sized> {
    waker: MaybeUninit<Waker>,
    msg: Option<MsgRef<'a, T>>,
    linkage: Node<Self>
}

impl<'a, T> Linkable for QWaitBlk<'a, T> {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

type WbRef<'a, T> = Ref<'a, QWaitBlk<'a, T>>;

struct QueueFuture<'a, T: Sized> {
    source: &'a Queue<'a, T>,
    wb: Option<WbRef<'a, T>>
}

pub struct Queue<'a, T: Sized> {
    msgs: List<'a, Message<'a, T>>,
    subscribers: List<'a, QWaitBlk<'a, T>>
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

    fn get(&self, wb: WbRef<'a, T>) -> Option<(Envelope<'a, T>, WbRef<'a, T>)> {
        let _lock = CriticalSection::new();
        if self.msgs.is_empty() {
            self.subscribers.enqueue(wb);
            None
        } else {
            let msg = self.msgs.dequeue().map(Envelope::new).unwrap();
            Some((msg, wb)) /* Return wb back if there is a msg. */
        }
    }

    fn put_internal(&self, msg: MsgRef<'a, T>) {
        let _lock = CriticalSection::new();
        if self.subscribers.is_empty() {
            self.msgs.enqueue(msg);
        } else {
            let subscriber = self.subscribers.dequeue().unwrap().release();
            let waker = unsafe { subscriber.waker.assume_init_mut() };
            subscriber.msg = Some(msg);              
            waker.wake_by_ref();
        }
    }

    pub fn put(&self, msg: Envelope<'a, T>) {
        self.put_internal(msg.into())
    }
    
    pub async fn block_on(&'a self) -> Envelope<'a, T> {
        let mut wb = QWaitBlk { 
            waker: MaybeUninit::uninit(),
            msg: None,
            linkage: Node::new()
        };
        let r: &'static mut QWaitBlk<T> = unsafe { mem::transmute(&mut wb) };
        QueueFuture { source: self, wb: Some(Ref::new(r)) }.await;
        Envelope::new(wb.msg.take().unwrap())
    }
}

pub struct Pool<'a, T: Sized> {
    pool: Queue<'a, T>,
    slice: Cell<Option<&'a mut [Message<'a, T>]>>,
}

impl<'a: 'static, T: Sized> Pool<'a, T> {
    pub const NEW: Self = Self {
        pool: Queue::new(),
        slice: Cell::new(None),
    };

    pub fn init<const N: usize>(&self, arr: &'a mut [Message<'a, T>; N]) {
        self.pool.init();
        self.slice.set(Some(&mut arr[0..N]));
    }

    pub async fn get(&'a self) -> Envelope<'a, T> {
        if let Some(msg) = self.alloc() {
            msg
        } else {
            self.pool.block_on().await
        }
    }

    pub fn alloc(&'a self) -> Option<Envelope<'a, T>> {
        let _lock = CriticalSection::new();
        if let Some(slice) = self.slice.take() {
            let (item, rest) = slice.split_first_mut().unwrap();
            if !rest.is_empty() {
                self.slice.set(Some(rest));
            }
            item.parent = Some(&self.pool);
            Some(Envelope::new(Ref::new(item)))
        } else {
            self.pool.msgs.dequeue().map(Envelope::new)
        }
    }
}

struct TWaitBlk {
    waker: MaybeUninit<Waker>,
    timeout: usize,
    linkage: Node<Self>
}

impl Linkable for TWaitBlk {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

pub struct Timer<'a, const N: usize> {
    timers: [List<'a, TWaitBlk>; N],
    len: [Cell<usize>; N], /* Length of the corresponding timer queue. */
    ticks: Cell<usize>
}

pub struct TimeoutFuture<'a, const N: usize> {
    container: &'a Timer<'a, N>,
    wb: Option<Ref<'a, TWaitBlk>>,
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
    
    fn subscribe(&self, delay: usize, subs: Ref<'a, TWaitBlk>) {
        let _lock = CriticalSection::new();
        let ticks = self.ticks.get();
        let timeout = ticks + delay;
        let q = Self::diff_msb(ticks, timeout);
        let len = self.len[q].get();
        subs.0.timeout = timeout;
        self.timers[q].enqueue(subs);
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
            let subscr = self.timers[q].dequeue().unwrap();
            let tout = subscr.0.timeout;
            if tout == new_ticks {
                let waker = unsafe { subscr.0.waker.assume_init_mut() };
                waker.wake_by_ref();
            } else {
                let qnext = Self::diff_msb(tout, new_ticks);
                let qnext_len = self.len[qnext].get();
                self.timers[qnext].enqueue(subscr);
                self.len[qnext].set(qnext_len + 1);
            }
            lock.window(); /* Timers processing preemption point. */
        }
    }
    
    pub async fn sleep_for(&'a self, t: u32) {
        let mut wb = TWaitBlk { 
            waker: MaybeUninit::uninit(),
            timeout: 0,
            linkage: Node::new()
        };
        let r: &'static mut TWaitBlk = unsafe { mem::transmute(&mut wb) };
        TimeoutFuture {
            container: self,
            wb: Some(Ref::new(r)),
            delay: Some(t as usize)
        }.await
    }
}

impl<'a: 'static, const N: usize> Future for TimeoutFuture<'a, N> {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        if let Some(delay) = self.delay.take() {
            if delay != 0 {
                let wb = self.wb.take().unwrap();
                wb.0.waker.write(cx.waker().clone());              
                self.container.subscribe(delay, wb);
                return Poll::Pending;
            }
        }
        Poll::Ready(())
    }
}

type DynFuture = dyn Future<Output=Infallible> + 'static;
type PinnedFuture = Pin<&'static mut DynFuture>;

struct Actor {
    prio: u8,
    vect: u16,
    future: Option<PinnedFuture>,
    context: Option<&'static Executor<'static>>,
    linkage: Node<Self>
}

impl Linkable for Actor {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

const VTABLE: RawWakerVTable = RawWakerVTable::new(
    |p| RawWaker::new(p, &VTABLE), 
    |_| {}, /* Wake is not used */ 
    |p| Actor::resume(Ref::new(unsafe { &mut *(p as *mut Actor) })), 
    |_| {} /* Drop is not used */
);

impl Actor {
    const NEW: Self = Self {
        prio: 0,
        vect: 0,
        future: None,
        context: None,
        linkage: Node::new()
    };

    fn call(&mut self) {
        let raw_waker = RawWaker::new(self as *mut Actor as *const (), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw_waker) };
        let mut cx = Context::from_waker(&waker);
        let _ = self.future.as_mut().unwrap().as_mut().poll(&mut cx);
    }

    fn resume<'a: 'static>(actor: Ref<'a, Actor>) {
        let executor = actor.0.context.take().unwrap();
        executor.activate(actor.0.prio, actor.0.vect, actor);
    }
}

impl<'a: 'static, T> Future for QueueFuture<'a, T> {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        if let Some(wb) = self.wb.take() {
            wb.0.waker.write(cx.waker().clone());
            if let Some((msg, oldwb)) = self.source.get(wb) {
                oldwb.0.msg = Some(msg.into());
            } else {
                return Poll::Pending
            }
        }
        Poll::Ready(())
    }
}

pub struct Executor<'a> {
    runq: [List<'a, Actor>; NPRIO as usize ],
}

impl<'a: 'static> Executor<'a> {
    pub const fn new() -> Self {
        Self {
            runq: [ List::UNINITIALIZED; NPRIO as usize ],
        }
    }

    fn init(&self) {
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
            actor.context = Some(self);
            actor.call();
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
        actor.future = Some(pinned_fut);
        actor.context = Some(self);
        actor.call();
    }

    pub fn run<const N: usize>(&'a self, mut list: [(u16, &mut DynFuture); N]) -> ! {
        self.init();
        let mut actors: [Actor; N] = [Actor::NEW; N];
        let mut actors_pool = actors.as_mut_slice();
        let mut pairs = list.as_mut_slice();

        while let Some((pair, rest)) = pairs.split_first_mut() {
            let (actor, remaining) = actors_pool.split_first_mut().unwrap();
            actor.vect = pair.0;
            actor.prio = unsafe { interrupt_prio(pair.0) };
            unsafe { self.spawn(actor, pair.1); }
            pairs = rest;
            actors_pool = remaining;
        }

        let prev_mask = unsafe { interrupt_mask(0) };
        assert!(prev_mask == 1);
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
fn interrupt_prio(_: u16) -> u8 { 0 }

#[cfg(test)]
mod tests {
use super::*;
type MsgQueue = Queue<'static, ExampleMsg>;
struct ExampleMsg(u32);
static TIMER: Timer<10> = Timer::new();
static POOL: Pool<ExampleMsg> = Pool::NEW;

async fn proxy(q: &'static MsgQueue) -> Infallible {
    loop {
        TIMER.sleep_for(100).await;
        println!("woken up");
        let mut msg = POOL.get().await;
        println!("alloc");
        msg.0 = 1;
        q.put(msg);
    }
}

async fn adder(q: &'static MsgQueue) -> Infallible {
    let mut sum = 0;
    loop {
        let msg = q.block_on().await;
        sum += msg.0;
        println!("adder got {} sum = {}", msg.0, sum);
    }
}

#[test]
fn main() {
    const MSG_PROTOTYPE: Message<ExampleMsg> = Message::new(ExampleMsg(0));
    static mut MSG_STORAGE: [Message<ExampleMsg>; 5] = [MSG_PROTOTYPE; 5];

    static QUEUE: Queue<ExampleMsg> = Queue::new();
    static SCHED: Executor = Executor::new();
    let mut actor1: Actor = Actor::NEW;
    let mut actor2: Actor = Actor::NEW;
    
    POOL.init(unsafe {&mut MSG_STORAGE});
    QUEUE.init();
    TIMER.init();
    SCHED.init();
    let mut f1 = proxy(&QUEUE);
    let mut f2 = adder(&QUEUE);
    
    unsafe {
        SCHED.spawn(&mut actor1, &mut f1);
        SCHED.spawn(&mut actor2, &mut f2);
    }

    for _ in 0..10 {
        for _ in 0..100 {
            TIMER.tick();
        }
        
        SCHED.schedule(0);
    }
}
}
}
