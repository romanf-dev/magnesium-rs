/*
 * Actor module isn't re-exported so its API is visible in the crate only.
 */

use core::future::Future;
use core::cell::Cell;
use core::pin::Pin;
use core::convert::Infallible;
use core::task::{Context, RawWaker, RawWakerVTable, Waker};
use crate::list::{ Node, Linkable };
use crate::executor::{ Prioritized, Dispatcher };

pub type DynFuture = dyn Future<Output = Infallible> + 'static;
type PinnedFuture = Pin<&'static mut DynFuture>;

pub struct Actor {
    pub prio: u8,
    pub cpu: u8,
    pub vect: u16,
    future: Cell<Option<PinnedFuture>>,
    context: Option<&'static dyn Dispatcher<'static, Self>>,
    linkage: Node<Self>,
}

impl Linkable for Actor {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

impl Prioritized for Actor {
    fn get_prio(&self) -> usize {
        self.prio as usize
    }
}

impl Actor {
    pub const fn new() -> Self {
        Self {
            prio: 0,
            cpu: 0,
            vect: 0,
            future: Cell::new(None),
            context: None,
            linkage: Node::new(),
        }
    }

    pub(crate) fn init(
        &mut self,
        cpu: u8,
        prio: u8,
        vect: u16,
        fut: PinnedFuture,
        parent: &'static dyn Dispatcher<'static, Self>,
    ) {
        self.vect = vect;
        self.cpu = cpu;
        self.prio = prio;
        self.future.set(Some(fut));
        self.context = Some(parent);
    }

    fn resume(actor: &'static Actor) {
        actor.context.as_ref().unwrap().activate(actor);
    }

    pub(crate) fn call(&self) {
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
}
