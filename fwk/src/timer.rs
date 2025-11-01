/*
 * Timer-related data structures are CPU-local so each CPU has to have the
 * tick source.
 */

use core::cell::Cell;
use core::cmp::min;
use core::mem;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use crate::list::{Node, Linkable, QueueRef};
use crate::utils::sync;
    
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
    timers: [QueueRef<'a, TWaitBlock>; N],
    len: [Cell<u32>; N], /* Length of the corresponding timer queue. */
    ticks: Cell<u32>,
    protect: sync::SmpProtection,
}

impl<'a, const N: usize> PerCpuTimer<'a, N> {
    const fn new() -> Self {
        Self {
            timers: [const { QueueRef::new() }; N],
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
        subs.timeout.set(timeout);
        self.timers[q].enqueue(subs);
        self.len[q].update(|length| length + 1);
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
                self.timers[qnext].enqueue(wait_blk);
                self.len[qnext].update(|len| len + 1);
                lock.window(|| {});
            }
        }
    }
}

pub struct Timer<'a, const N: usize = 10, const NCPUS: usize = 1> {
    per_cpu_data: [PerCpuTimer<'a, N>; NCPUS],
}

struct TimeoutFuture<'a, const N: usize> {
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

unsafe impl<const N: usize, const NCPUS: usize> Sync for Timer<'_, N, NCPUS> {}
