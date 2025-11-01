/*
 * Message pool manages user-allocated memory as a source of messages.
 * When a message is dropped it is returned into the parent pool.
 * While Drop trait is not implemented for Message it is implemented for
 * Envelope object which wraps messages in the user API. 
 */

use core::cell::Cell;
use crate::channel::Channel;
use crate::msg::{ Message, Envelope };
use crate::list::Mut;
use crate::utils::sync;

pub struct Pool<'a, T: Sized> {
    pool: Channel<'a, T>,
    slice: Cell<Option<&'a mut [Message<'a, T>]>>,
}

unsafe impl<T: Send> Sync for Pool<'_, T> {}

impl<'a: 'static, T: Sized> Pool<'a, T> {
    pub const fn new() -> Self {
        Self {
            pool: Channel::new(),
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
