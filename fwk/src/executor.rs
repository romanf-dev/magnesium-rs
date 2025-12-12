/*
 * Scheduling data structures are CPU-local.
 */

use core::marker::PhantomData;
use crate::actor::{Actor, RefWrapper};
use crate::list::{ QueueRef, Linkable };
use crate::utils::sync;

pub(crate) trait Dispatcher<'a, T> {
    fn activate(&'a self, actor: &'a T);
}

pub trait Pic {
    fn interrupt_request(cpu: u8, vector: u16);
    fn interrupt_prio(vector: u16) -> u8;
}

struct PerCpuContainer<'a, const NPRIO: usize, T: Linkable> {
    runq: [QueueRef<'a, T>; NPRIO],
    protect: sync::SmpProtection,
}

impl<'a, const NPRIO: usize, T: Linkable> PerCpuContainer<'a, NPRIO, T> {
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

    fn insert(&self, item: &'a T, prio: usize) {
        let _lock = sync::CriticalSection::new(&self.protect);
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

    fn spawn(&'a self, this_cpu: u8, actor: &'a Actor) {
        let prio = IC::interrupt_prio(actor.vect);
        assert!(prio == actor.prio);
        assert!(this_cpu == actor.cpu);
        actor.set_parent(self);
        self.activate(actor);
    }

    pub fn run<const N: usize>(&'a self, actors: [RefWrapper<Actor>; N]) {
        let cpu = unsafe { sync::cpu_this() };
        let runq_lock = &self.per_cpu_data[cpu as usize].protect;
        let _lock = sync::CriticalSection::new(runq_lock);

        for i in 0..N {
            self.spawn(cpu, actors[i].0);
        }
    }
}

impl<'a: 'static, IC: Pic, const NPRIO: usize, const NCPUS: usize>Dispatcher<'a, Actor> for Executor<'a, IC, NPRIO, NCPUS> {
    fn activate(&'a self, actor: &'a Actor) {
        let container = &self.per_cpu_data[actor.cpu as usize];
        container.insert(actor, actor.prio as usize);
        IC::interrupt_request(actor.cpu, actor.vect);
    }
}

unsafe impl<IC: Pic, const NPRIO: usize, const NCPUS: usize> Sync for Executor<'_, IC, NPRIO, NCPUS> {}
