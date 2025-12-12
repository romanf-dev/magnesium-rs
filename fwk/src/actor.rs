/*
 * Actor module isn't re-exported so its API is visible in the crate only.
 */

use core::future::Future;
use core::cell::{ Cell, UnsafeCell, OnceCell };
use core::pin::Pin;
use core::mem;
use core::convert::Infallible;
use core::task::{ Context, RawWaker, RawWakerVTable, Waker };
use crate::list::{ Node, Linkable };
use crate::executor::Dispatcher;

type DynFuture = dyn Future<Output = Infallible> + 'static;
type PinnedFuture = Pin<&'static mut DynFuture>;
pub struct RefWrapper<T>(pub &'static T) where T: 'static;

pub struct FutureStorage<const N: usize, const A: usize> {
    data: UnsafeCell<[u8; N]>,
}

impl<const N: usize, const A: usize> FutureStorage<N, A> {
    pub const fn new() -> Self {
        Self {
            data: UnsafeCell::new([0; N]),
        }
    }

    pub fn copy_from<F>(&'static self, f: impl FnOnce() -> F) -> PinnedFuture
        where F: Future<Output=Infallible> + 'static {
        const {
            assert!(mem::size_of::<F>() <= N - A);
        }
        let ptr = self.data.get();
        let offset = ptr.align_offset(A);
        unsafe {
            let future_storage = ptr.add(offset).cast::<F>();
            future_storage.write(f());
            Pin::static_mut(&mut *future_storage)
        }
    }
}

pub struct Actor {
    pub prio: u8,
    pub cpu: u8,
    pub vect: u16,
    future: Cell<Option<PinnedFuture>>,
    context: OnceCell<&'static dyn Dispatcher<'static, Self>>,
    linkage: Node<Self>,
}

impl Linkable for Actor {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

impl Actor {
    pub const fn new(cpu: u8, vect: u16, prio: u8) -> Self {
        Self {
            prio,
            cpu,
            vect,
            future: Cell::new(None),
            context: OnceCell::new(),
            linkage: Node::new(),
        }
    }

    pub fn bind(&self, fut: PinnedFuture) {
        self.future.set(Some(fut));
    }

    pub(crate) fn set_parent(&self, parent: &'static dyn Dispatcher<'static, Self>) {
        let err = self.context.set(parent);
        assert!(err.is_ok());
    }

    fn resume(actor: &'static Actor) {
        actor.context.get().unwrap().activate(actor);
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
    
    pub const fn size_of<F: Future>(_: &impl FnOnce() -> F) -> usize {
        mem::size_of::<F>()
    }

    pub const fn align_of<F: Future>(_: &impl FnOnce() -> F) -> usize {
        mem::align_of::<F>()
    }
}

unsafe impl Sync for Actor {}
unsafe impl<const N: usize, const A: usize> Sync for FutureStorage<N, A> {}

#[macro_export]
macro_rules! bind {
    ($task:ident, $vect:expr, $prio:expr, $cpu:expr) => {{
        const SZ: usize = $crate::Actor::size_of(&$task);
        const ALIGN: usize = $crate::Actor::align_of(&$task);
        static FUTURE: $crate::FutureStorage<{SZ + ALIGN}, {ALIGN}> = $crate::FutureStorage::new();
        static ACTOR: $crate::Actor = $crate::Actor::new($cpu, $vect, $prio);

        let pinned = FUTURE.copy_from($task);
        ACTOR.bind(pinned);
        $crate::RefWrapper(&ACTOR)
    }};
    ($task:ident, $vect:expr, $prio:expr) => { bind!($task, $vect, $prio, 0) };
}
