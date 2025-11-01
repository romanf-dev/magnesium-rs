/*
 * Scheduling data structures are CPU-local.
 */

use core::marker::PhantomData;
use core::mem;
use core::pin::Pin;
use crate::actor::{ Actor, DynFuture };
use crate::list::{ QueueRef, Linkable };
use crate::utils::sync;

pub(crate) trait Dispatcher<'a, T> {
    fn activate(&'a self, actor: &'a T);
}

pub(crate) trait Prioritized {
    fn get_prio(&self) -> usize;
}

pub trait Pic {
    fn interrupt_request(cpu: u8, vector: u16);
    fn interrupt_prio(vector: u16) -> u8;
}

struct PerCpuContainer<'a, const NPRIO: usize, T: Linkable + Prioritized> {
    runq: [QueueRef<'a, T>; NPRIO],
    protect: sync::SmpProtection,
}

impl<'a, const NPRIO: usize, T: Linkable + Prioritized> PerCpuContainer<'a, NPRIO, T> {
    const fn new() -> Self {
        Self {
            runq: [const { QueueRef::new() }; NPRIO],
            protect: sync::SmpProtection::new(),
        }
    }

    fn init(&self) {
        for i in 0..NPRIO {
            self.runq[i].init()
        }
    }

    fn extract(&self, prio: u8) -> Option<&'a T> {
        let _lock = sync::CriticalSection::new(&self.protect);
        self.runq[prio as usize].dequeue()
    }

    fn insert(&self, item: &'a T) {
        let _lock = sync::CriticalSection::new(&self.protect);
        let prio = item.get_prio();
        self.runq[prio].enqueue(item);
    }
}

pub struct Executor<'a, IC: Pic, const NPRIO: usize, const NCPUS: usize = 1> {
    per_cpu_data: [PerCpuContainer<'a, NPRIO, Actor>; NCPUS],
    _pic: PhantomData<IC>,
}

impl<'a: 'static, IC: Pic, const NPRIO: usize, const NCPUS: usize> Executor<'a, IC, NPRIO, NCPUS> {
    pub const fn new() -> Self {
        Self {
            per_cpu_data: [const { PerCpuContainer::<NPRIO, Actor>::new() }; NCPUS],
            _pic: PhantomData,
        }
    }

    pub fn init(&self) {
        for cpu in 0..NCPUS {
            self.per_cpu_data[cpu].init();
        }
    }

    pub fn schedule(&self, vect: u16) {
        let this_cpu = unsafe { sync::cpu_this() as usize };
        let prio = IC::interrupt_prio(vect);
        while let Some(actor) = self.per_cpu_data[this_cpu].extract(prio) {
            actor.call();
        }
    }

    pub(crate) unsafe fn spawn(&'a self, cpu: u8, vect: u16, actor: &mut Actor, f: &mut DynFuture) {
        let static_fut: &'a mut DynFuture = mem::transmute(f);
        let pinned_fut = Pin::new_unchecked(static_fut);
        let actor: &'a mut Actor = mem::transmute(actor);
        let prio = IC::interrupt_prio(vect);
        actor.init(cpu, prio, vect, pinned_fut, self);
        self.activate(actor);
    }

    pub fn run<const N: usize>(&'a self, mut list: [(u16, &mut DynFuture); N]) -> ! {
        #[cfg(not(feature = "smp"))]
        self.init();
        let mut actors = [const { Actor::new() }; N];
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

impl<'a: 'static, IC: Pic, const NPRIO: usize, const NCPUS: usize> Dispatcher<'a, Actor> for Executor<'a, IC, NPRIO, NCPUS> {
    fn activate(&'a self, actor: &'a Actor) {
        let container = &self.per_cpu_data[actor.cpu as usize];
        container.insert(actor);
        IC::interrupt_request(actor.cpu, actor.vect);
    }
}

unsafe impl<IC: Pic, const NPRIO: usize, const NCPUS: usize> Sync for Executor<'_, IC, NPRIO, NCPUS> {}
