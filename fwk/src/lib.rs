#![cfg_attr(not(test), no_std)]
#![allow(unused_unsafe)] /* for test stubs */

#[cfg(all(not(test), target_os = "none"))]
pub(crate) mod hw {
    extern "C" {
        pub fn interrupt_request(cpu: u8, vector: u16);
        pub fn interrupt_prio(vector: u16) -> u8;
    }

    #[cfg(target_arch = "arm")]
    pub unsafe fn interrupt_mask(mask: bool) {
        if mask {
            core::arch::asm!("cpsid i");
        } else {
            core::arch::asm!("cpsie i");
        }
    }
}

#[cfg(any(test, not(target_os = "none")))]
pub(crate) mod hw {
    pub fn interrupt_mask(_: bool) {}
    pub fn interrupt_request(_: u8, _: u16) {}

    pub fn interrupt_prio(_: u16) -> u8 {
        0
    }
}

#[cfg(not(feature = "smp"))] /* single-CPU case is implemented here... */
mod utils {
    pub mod sync {
        pub const fn cpu_this() -> u8 {
            0
        }

        pub struct SmpProtection;
        pub struct CriticalSection;

        impl SmpProtection {
            pub const fn new() -> Self {
                Self
            }
        }

        impl CriticalSection {
            pub fn new(_: &SmpProtection) -> Self {
                unsafe { crate::hw::interrupt_mask(true) };
                Self
            }

            pub fn window(&self, func: impl FnOnce()) {
                unsafe {
                    crate::hw::interrupt_mask(false);
                    func();
                    crate::hw::interrupt_mask(true);
                }
            }
        }

        impl Drop for CriticalSection {
            fn drop(&mut self) {
                unsafe { crate::hw::interrupt_mask(false) };
            }
        }
    }
}

#[cfg(feature = "smp")]
mod utils; /* multi-CPU sync is in separate file... */

pub mod mg {
    use crate::hw;
    use crate::utils::sync;
    use core::cell::Cell;
    use core::cmp::min;
    use core::convert::{Infallible, Into};
    use core::future::Future;
    use core::marker::PhantomData;
    use core::mem;
    use core::ops::{Deref, DerefMut};
    use core::pin::Pin;
    use core::ptr;
    use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    struct Mut<'a, T>(&'a mut T); /* wrapper for muts to avoid reborrows */

    impl<'a, T> Mut<'a, T> {
        fn new(r: &'a mut T) -> Self {
            Self(r)
        }

        fn release(self) -> &'a mut T {
            self.0
        }
    }

    struct Node<T> {
        links: Cell<(*const Node<T>, *const Node<T>)>,
        payload: Cell<Option<*const T>>,
    }

    impl<T> Node<T> {
        const fn new() -> Self {
            Node {
                links: Cell::new((ptr::null(), ptr::null())),
                payload: Cell::new(None),
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
            self.payload.take().map(|ptr| {
                let (prev, next) = self.links.replace((ptr::null(), ptr::null()));
                (*prev).set_next(next);
                (*next).set_prev(prev);
                ptr
            })
        }
    }

    trait Linkable: Sized {
        fn to_links(&self) -> &Node<Self>;
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
            let (prev, next) = self.root.links.replace((ptr::null(), ptr::null()));
            node.links.set((prev, &self.root));
            self.root.links.set((node, next));
            unsafe {
                (*prev).set_next(node);
            }
            node
        }
    }

    struct ListRef<'a, T: Linkable> {
        list: GenericList<'a, T>, /* may only contain shared refs to T. */
    }

    impl<'a, T: Linkable> ListRef<'a, T> {
        const fn new() -> Self {
            Self {
                list: GenericList::new(),
            }
        }

        fn init(&self) {
            self.list.init();
        }

        fn enqueue(&self, object: &'a T) -> &'a Node<T> {
            let ptr: *const T = object;
            let node = object.to_links();
            node.payload.set(Some(ptr));
            self.list.append(node)
        }

        fn dequeue(&self) -> Option<&'a T> {
            self.list
                .peek_head()
                .map(|node| unsafe { &*node.unlink().unwrap() })
        }
    }

    struct ListMut<'a, T: Linkable> {
        list: GenericList<'a, T>, /* may only contain mutable refs to T. */
    }

    impl<'a, T: Linkable> ListMut<'a, T> {
        const fn new() -> Self {
            Self {
                list: GenericList::new(),
            }
        }

        fn init(&self) {
            self.list.init();
        }

        fn enqueue(&self, wrapper: Mut<'a, T>) -> &'a Node<T> {
            let object = wrapper.release();
            let ptr: *mut T = object;
            let node = object.to_links();
            node.payload.set(Some(ptr));
            self.list.append(node)
        }

        fn dequeue(&self) -> Option<Mut<'a, T>> {
            self.list
                .peek_head()
                .map(|node| unsafe { Mut::new(&mut *(node.unlink().unwrap() as *mut T)) })
        }
    }

    pub struct Message<'a, T> {
        parent: Option<&'a Queue<'a, T>>,
        linkage: Node<Self>,
        payload: T,
    }

    impl<T> Message<'_, T> {
        pub const fn new(data: T) -> Self {
            Self {
                parent: None,
                linkage: Node::new(),
                payload: data,
            }
        }
    }

    impl<T> Linkable for Message<'_, T> {
        fn to_links(&self) -> &Node<Self> {
            &self.linkage
        }
    }

    type MsgRef<'a, T> = Mut<'a, Message<'a, T>>;

    pub struct Envelope<'a: 'static, T> {
        content: Option<MsgRef<'a, T>>, /* Msg wrapper with Drop impl. */
    }

    impl<'a, T> Envelope<'a, T> {
        fn new(msg: MsgRef<'a, T>) -> Self {
            Self { content: Some(msg) }
        }
    }

    impl<'a, T> From<Envelope<'a, T>> for MsgRef<'a, T> {
        fn from(mut val: Envelope<'a, T>) -> Self {
            val.content.take().unwrap()
        }
    }

    impl<T> Deref for Envelope<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.content.as_ref().unwrap().0.payload
        }
    }

    impl<T> DerefMut for Envelope<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            &mut self.content.as_mut().unwrap().0.payload
        }
    }

    impl<T> Drop for Envelope<'_, T> {
        fn drop(&mut self) {
            if let Some(msg) = self.content.take() {
                let parent = msg.0.parent.as_ref().unwrap();
                parent.put_internal(msg);
            }
        }
    }

    struct QWaitBlock<'a, T: Sized> {
        waker: Cell<Option<Waker>>,
        msg: Cell<Option<MsgRef<'a, T>>>,
        linkage: Node<Self>,
    }

    impl<T> Linkable for QWaitBlock<'_, T> {
        fn to_links(&self) -> &Node<Self> {
            &self.linkage
        }
    }

    struct QueueFuture<'a, T: Sized> {
        source: &'a Queue<'a, T>,
        wb: &'a QWaitBlock<'a, T>,
    }

    pub struct Queue<'a, T: Sized> {
        msgs: ListMut<'a, Message<'a, T>>,
        subscribers: ListRef<'a, QWaitBlock<'a, T>>,
        protect: sync::SmpProtection,
    }

    impl<'a: 'static, T: Sized> Queue<'a, T> {
        pub const fn new() -> Self {
            Self {
                msgs: ListMut::new(),
                subscribers: ListRef::new(),
                protect: sync::SmpProtection::new(),
            }
        }

        pub fn init(&self) {
            self.msgs.init();
            self.subscribers.init();
        }

        fn get(&self, wb: &'a QWaitBlock<'a, T>) -> Option<MsgRef<'a, T>> {
            let _lock = sync::CriticalSection::new(&self.protect);
            self.msgs.dequeue().or_else(|| {
                self.subscribers.enqueue(wb);
                None
            })
        }

        fn put_internal(&self, msg: MsgRef<'a, T>) {
            let lock = sync::CriticalSection::new(&self.protect);
            if let Some(wait_blk) = self.subscribers.dequeue() {
                wait_blk.msg.set(Some(msg));
                lock.window(|| {
                    wait_blk.waker.take().unwrap().wake();
                });
            } else {
                self.msgs.enqueue(msg);
            }
        }

        pub fn put(&self, msg: Envelope<'a, T>) {
            self.put_internal(msg.into())
        }

        pub async fn block_on(&'a self) -> Envelope<'a, T> {
            let wb: QWaitBlock<'_, T> = QWaitBlock {
                waker: Cell::new(None),
                msg: Cell::new(None),
                linkage: Node::new(),
            };
            let ref_wb: &'a QWaitBlock<T> = unsafe { mem::transmute(&wb) };
            let future = QueueFuture {
                source: self,
                wb: ref_wb,
            };
            let msg = future.await;
            Envelope::new(msg)
        }
    }

    pub struct Pool<'a, T: Sized> {
        pool: Queue<'a, T>,
        slice: Cell<Option<&'a mut [Message<'a, T>]>>,
    }

    impl<'a: 'static, T: Sized> Pool<'a, T> {
        pub const fn new() -> Self {
            Self {
                pool: Queue::new(),
                slice: Cell::new(None),
            }
        }

        pub unsafe fn init<const N: usize>(&self, arr: *mut [Message<'a, T>; N]) {
            self.pool.init();
            let msgs = &mut *arr;
            self.slice.set(Some(msgs.as_mut_slice()));
        }

        pub async fn get(&'a self) -> Envelope<'a, T> {
            if let Some(msg) = self.alloc() {
                msg
            } else {
                self.pool.block_on().await
            }
        }

        pub fn alloc(&'a self) -> Option<Envelope<'a, T>> {
            let _lock = sync::CriticalSection::new(&self.pool.protect);
            if let Some(slice) = self.slice.take() {
                let (item, rest) = slice.split_first_mut().unwrap();
                if !rest.is_empty() {
                    self.slice.set(Some(rest));
                }
                item.parent = Some(&self.pool);
                Some(Envelope::new(Mut::new(item)))
            } else {
                self.pool.msgs.dequeue().map(Envelope::new)
            }
        }
    }

    struct TWaitBlock {
        waker: Cell<Option<Waker>>,
        timeout: Cell<u32>,
        linkage: Node<Self>,
    }

    impl Linkable for TWaitBlock {
        fn to_links(&self) -> &Node<Self> {
            &self.linkage
        }
    }

    struct PerCpuTimer<'a, const N: usize> {
        timers: [ListRef<'a, TWaitBlock>; N],
        len: [Cell<u32>; N], /* Length of the corresponding timer queue. */
        ticks: Cell<u32>,
        protect: sync::SmpProtection,
    }

    impl<'a, const N: usize> PerCpuTimer<'a, N> {
        const fn new() -> Self {
            Self {
                timers: [const { ListRef::new() }; N],
                len: [const { Cell::new(0) }; N],
                ticks: Cell::new(0),
                protect: sync::SmpProtection::new(),
            }
        }

        fn init(&self) {
            for i in 0..N {
                self.timers[i].init()
            }
        }

        fn diff_msb(x: u32, y: u32) -> usize {
            assert!(x != y); /* Since x != y at least one bit is different. */
            let msb = u32::BITS - (x ^ y).leading_zeros() - 1;
            min(msb as usize, N - 1)
        }

        fn subscribe(&self, delay: u32, subs: &'a TWaitBlock) {
            let _lock = sync::CriticalSection::new(&self.protect);
            let ticks = self.ticks.get();
            let timeout = ticks + delay;
            let q = Self::diff_msb(ticks, timeout);
            let len = self.len[q].get();
            subs.timeout.set(timeout);
            self.timers[q].enqueue(subs);
            self.len[q].set(len + 1);
        }

        fn tick(&self) {
            let lock = sync::CriticalSection::new(&self.protect);
            let old_ticks = self.ticks.get();
            let new_ticks = old_ticks + 1;
            let q = Self::diff_msb(old_ticks, new_ticks);
            let len = self.len[q].replace(0);
            self.ticks.set(new_ticks);

            for _ in 0..len {
                let wait_blk = self.timers[q].dequeue().unwrap();
                let tout = wait_blk.timeout.get();
                if tout == new_ticks {
                    lock.window(|| wait_blk.waker.take().unwrap().wake());
                } else {
                    let qnext = Self::diff_msb(tout, new_ticks);
                    let qnext_len = self.len[qnext].get();
                    self.timers[qnext].enqueue(wait_blk);
                    self.len[qnext].set(qnext_len + 1);
                    lock.window(|| {});
                }
            }
        }
    }

    pub struct Timer<'a, const N: usize, const NCPUS: usize> {
        per_cpu_data: [PerCpuTimer<'a, N>; NCPUS],
    }

    pub struct TimeoutFuture<'a, const N: usize> {
        container: &'a PerCpuTimer<'a, N>,
        wb: &'a TWaitBlock,
        delay: Option<u32>,
    }

    impl<'a: 'static, const N: usize, const NCPUS: usize> Timer<'a, N, NCPUS> {
        pub const fn new() -> Self {
            Self {
                per_cpu_data: [const { PerCpuTimer::new() }; NCPUS],
            }
        }

        pub fn init(&self) {
            for cpu in 0..NCPUS {
                self.per_cpu_data[cpu].init()
            }
        }

        pub fn tick(&self) {
            let this_cpu = unsafe { sync::cpu_this() };
            let context = &self.per_cpu_data[this_cpu as usize];
            context.tick();
        }

        pub async fn sleep_for(&'a self, t: u32) {
            let wb = TWaitBlock {
                waker: Cell::new(None),
                timeout: Cell::new(0),
                linkage: Node::new(),
            };
            let this_cpu = unsafe { sync::cpu_this() };
            let context = &self.per_cpu_data[this_cpu as usize];
            let ref_wb: &'a TWaitBlock = unsafe { mem::transmute(&wb) };
            let future = TimeoutFuture {
                container: context,
                wb: ref_wb,
                delay: Some(t),
            };
            future.await
        }
    }

    impl<'a: 'static, const N: usize> Future for TimeoutFuture<'a, N> {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
            if let Some(delay) = self.delay.take() {
                if delay != 0 {
                    self.wb.waker.set(Some(cx.waker().clone()));
                    self.container.subscribe(delay, self.wb);
                } else {
                    cx.waker().clone().wake();
                }
                return Poll::Pending;
            }
            Poll::Ready(())
        }
    }

    type DynFuture = dyn Future<Output = Infallible> + 'static;
    type PinnedFuture = Pin<&'static mut DynFuture>;
    type Runqueue<'a> = ListRef<'a, Actor>;
    type ActivationData<'a> = (&'a Runqueue<'a>, &'a sync::SmpProtection);

    struct Actor {
        prio: u8,
        cpu: u8,
        vect: u16,
        future: Cell<Option<PinnedFuture>>,
        context: Option<ActivationData<'static>>,
        linkage: Node<Self>,
    }

    impl Linkable for Actor {
        fn to_links(&self) -> &Node<Self> {
            &self.linkage
        }
    }

    impl Actor {
        const fn new() -> Self {
            Self {
                prio: 0,
                cpu: 0,
                vect: 0,
                future: Cell::new(None),
                context: None,
                linkage: Node::new(),
            }
        }

        fn init(
            &mut self,
            cpu: u8,
            prio: u8,
            vect: u16,
            fut: PinnedFuture,
            target: ActivationData<'static>,
        ) {
            self.vect = vect;
            self.cpu = cpu;
            self.prio = prio;
            self.future.set(Some(fut));
            self.context = Some(target);
        }

        fn call(&self) {
            const VTABLE: RawWakerVTable = RawWakerVTable::new(
                |p| RawWaker::new(p, &VTABLE),
                |p| Actor::resume(unsafe { &*(p as *const Actor) }),
                |_| {}, /* Wake by ref is not used */
                |_| {}, /* Drop is not used */
            );
            let raw = RawWaker::new(self as *const Actor as *const (), &VTABLE);
            let waker = unsafe { Waker::from_raw(raw) };
            let mut cx = Context::from_waker(&waker);
            let fut = unsafe { &mut *self.future.as_ptr() };
            let _ = fut.as_mut().unwrap().as_mut().poll(&mut cx);
        }

        fn resume<'a: 'static>(actor: &'a Actor) {
            activate(actor, actor.context.as_ref().unwrap());
            unsafe { hw::interrupt_request(actor.cpu, actor.vect) }
        }
    }

    impl<'a: 'static, T> Future for QueueFuture<'a, T> {
        type Output = MsgRef<'a, T>;
        fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
            if let Some(msg) = self.wb.msg.take() {
                Poll::Ready(msg)
            } else {
                self.wb.waker.set(Some(cx.waker().clone()));
                match self.source.get(self.wb) {
                    Some(msg) => Poll::Ready(msg),
                    _ => Poll::Pending,
                }
            }
        }
    }

    struct PerCpuExecutor<'a, const NPRIO: usize> {
        runq: [Runqueue<'a>; NPRIO],
        protect: sync::SmpProtection,
    }

    impl<'a, const NPRIO: usize> PerCpuExecutor<'a, NPRIO> {
        const fn new() -> Self {
            Self {
                runq: [const { ListRef::new() }; NPRIO],
                protect: sync::SmpProtection::new(),
            }
        }

        fn init(&self) {
            for i in 0..NPRIO {
                self.runq[i].list.init()
            }
        }

        fn extract(&self, prio: u8) -> Option<&'a Actor> {
            let _lock = sync::CriticalSection::new(&self.protect);
            self.runq[prio as usize].dequeue()
        }
    }

    fn activate<'a>(actor: &'a Actor, target: &ActivationData<'a>) {
        let (target_runq, protect) = target;
        let _lock = sync::CriticalSection::new(protect);
        target_runq.enqueue(actor);
    }

    pub struct Executor<'a, const NPRIO: usize, const NCPUS: usize> {
        per_cpu_data: [PerCpuExecutor<'a, NPRIO>; NCPUS],
    }

    impl<'a: 'static, const NPRIO: usize, const NCPUS: usize> Executor<'a, NPRIO, NCPUS> {
        pub const fn new() -> Self {
            Self {
                per_cpu_data: [const { PerCpuExecutor::<NPRIO>::new() }; NCPUS],
            }
        }

        pub fn init(&self) {
            for cpu in 0..NCPUS {
                self.per_cpu_data[cpu].init();
            }
        }

        pub fn schedule(&self, vect: u16) {
            let this_cpu = unsafe { sync::cpu_this() as usize };
            let prio = unsafe { hw::interrupt_prio(vect) };
            while let Some(actor) = self.per_cpu_data[this_cpu].extract(prio) {
                actor.call();
            }
        }

        unsafe fn spawn(&'a self, cpu: u8, vect: u16, actor: &mut Actor, f: &mut DynFuture) {
            let static_fut: &'a mut DynFuture = mem::transmute(f);
            let actor: &'a mut Actor = mem::transmute(actor);
            let pinned_fut = Pin::new_unchecked(static_fut);
            let prio = hw::interrupt_prio(vect);
            let cpu_data = &self.per_cpu_data[cpu as usize];
            let activation_runq = &cpu_data.runq[prio as usize];
            let activation_data = (activation_runq, &cpu_data.protect);
            actor.init(cpu, prio, vect, pinned_fut, activation_data);
            activation_runq.enqueue(actor);
            hw::interrupt_request(cpu, vect);
        }

        pub fn run<const N: usize>(&'a self, mut list: [(u16, &mut DynFuture); N]) -> ! {
            #[cfg(not(feature = "smp"))]
            self.init();
            let mut actors: [Actor; N] = [const { Actor::new() }; N];
            let mut actors = actors.as_mut_slice();
            let mut pairs = list.as_mut_slice();
            let cpu = unsafe { sync::cpu_this() };
            let runq_lock = &self.per_cpu_data[cpu as usize].protect;
            let lock = sync::CriticalSection::new(runq_lock);

            while let Some((pair, rest)) = pairs.split_first_mut() {
                let (actor, remaining) = actors.split_first_mut().unwrap();
                unsafe {
                    self.spawn(cpu, pair.0, actor, pair.1);
                }
                pairs = rest;
                actors = remaining;
            }

            drop(lock);
            loop {}
        }
    }

    pub type SingleCpuExecutor<'a, const NPRIO: usize> = Executor<'a, NPRIO, 1>;
    pub type SingleCpuTimer<'a, const N: usize> = Timer<'a, N, 1>;
    unsafe impl<const NPRIO: usize, const NCPUS: usize> Sync for Executor<'_, NPRIO, NCPUS> {}
    unsafe impl<T: Send> Sync for Queue<'_, T> {}
    unsafe impl<T: Send> Sync for Pool<'_, T> {}
    unsafe impl<const N: usize, const NCPUS: usize> Sync for Timer<'_, N, NCPUS> {}

    #[cfg(test)]
    mod tests {
        use super::*;
        type MsgQueue = Queue<'static, ExampleMsg>;
        struct ExampleMsg(u32);
        static TIMER: SingleCpuTimer<10> = Timer::new();
        static POOL: Pool<ExampleMsg> = Pool::new();

        async fn proxy(q: &'static MsgQueue) -> Infallible {
            loop {
                TIMER.sleep_for(100).await;
                let mut msg = POOL.get().await;
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
            static mut MSG_STORAGE: [Message<ExampleMsg>; 5] =
                [const { Message::new(ExampleMsg(0)) }; 5];
            static QUEUE: Queue<ExampleMsg> = Queue::new();
            static SCHED: SingleCpuExecutor<1> = Executor::new();
            let mut actor1: Actor = Actor::new();
            let mut actor2: Actor = Actor::new();
            const TEST_VECT: u16 = 0;

            QUEUE.init();
            TIMER.init();
            SCHED.init();
            let mut f1 = proxy(&QUEUE);
            let mut f2 = adder(&QUEUE);

            unsafe {
                POOL.init(core::ptr::addr_of_mut!(MSG_STORAGE));
                SCHED.spawn(0, TEST_VECT, &mut actor1, &mut f1);
                SCHED.spawn(0, TEST_VECT, &mut actor2, &mut f2);
            }

            for _ in 0..10 {
                for _ in 0..100 {
                    TIMER.tick();
                }

                SCHED.schedule(TEST_VECT);
            }
        }
    }
}
