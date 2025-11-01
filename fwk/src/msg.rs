/*
 * Messages are represented as opaque 'envelope' object implementing Deref
 * and DerefMut traits. Message object is used for memory pool allocation
 * only.
 */

use core::ops::{ Deref, DerefMut };
use crate::channel::Channel;
use crate::list::{ Mut, Node, Linkable };

pub struct Message<'a, T> {
    pub(crate) parent: Option<&'a Channel<'a, T>>,
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

pub(crate) type MsgRef<'a, T> = Mut<'a, Message<'a, T>>;

pub struct Envelope<'a: 'static, T> {
    content: Option<MsgRef<'a, T>>, /* Msg wrapper with Drop impl. */
}

impl<'a, T> Envelope<'a, T> {
    pub fn new(msg: MsgRef<'a, T>) -> Self {
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
        &self.content.as_ref().unwrap().payload
    }
}

impl<T> DerefMut for Envelope<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.content.as_mut().unwrap().payload
    }
}

impl<T> Drop for Envelope<'_, T> {
    fn drop(&mut self) {
        if let Some(msg) = self.content.take() {
            let parent = msg.parent.as_ref().unwrap();
            parent.put_internal(msg);
        }
    }
}
