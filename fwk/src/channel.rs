/*
 * Channels do not have internal storage, they store messages and subscribers
 * as intrusive linked list.
 */

use core::cell::Cell;
use core::future::Future;
use core::mem;
use core::pin::Pin;
use core::task::{ Context, Poll, Waker };
use crate::msg::{ MsgRef, Message, Envelope };
use crate::list::{ Node, Linkable, QueueMut, QueueRef };
use crate::utils::sync;

struct CWaitBlock<'a, T: Sized> {
    waker: Cell<Option<Waker>>,
    msg: Cell<Option<MsgRef<'a, T>>>,
    linkage: Node<Self>,
}

impl<T> Linkable for CWaitBlock<'_, T> {
    fn to_links(&self) -> &Node<Self> {
        &self.linkage
    }
}

pub struct Channel<'a, T: Sized> {
    pub(crate) protect: sync::SmpProtection,
    pub(crate) msgs: QueueMut<'a, Message<'a, T>>,
    subscribers: QueueRef<'a, CWaitBlock<'a, T>>,
}

impl<'a: 'static, T: Sized> Channel<'a, T> {
    pub const fn new() -> Self {
        Self {
            msgs: QueueMut::new(),
            subscribers: QueueRef::new(),
            protect: sync::SmpProtection::new(),
        }
    }

    pub fn init(&self) {
        self.msgs.init();
        self.subscribers.init();
    }

    fn get(&self, wb: &'a CWaitBlock<'a, T>) -> Option<MsgRef<'a, T>> {
        let _lock = sync::CriticalSection::new(&self.protect);
        self.msgs.dequeue().or_else(|| {
            self.subscribers.enqueue(wb);
            None
        })
    }

    pub(crate) fn put_internal(&self, msg: MsgRef<'a, T>) {
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
        let wb: CWaitBlock<'_, T> = CWaitBlock {
            waker: Cell::new(None),
            msg: Cell::new(None),
            linkage: Node::new(),
        };
        let ref_wb: &'a CWaitBlock<T> = unsafe { mem::transmute(&wb) };
        let future = ChannelFuture {
            source: self,
            wb: ref_wb,
        };
        let msg = future.await;
        Envelope::new(msg)
    }
}

struct ChannelFuture<'a, T: Sized> {
    source: &'a Channel<'a, T>,
    wb: &'a CWaitBlock<'a, T>,
}

impl<'a: 'static, T> Future for ChannelFuture<'a, T> {
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
    
unsafe impl<T: Send> Sync for Channel<'_, T> {}
