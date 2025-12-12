#![cfg_attr(not(test), no_std)]
#![allow(unused_unsafe)] /* for test stubs */

pub(crate) mod hw {
    #[cfg(all(not(test), target_os = "none", target_arch = "arm"))]
    pub unsafe fn interrupt_mask(mask: bool) {
        match mask {
            true => core::arch::asm!("cpsid i"),
            false => core::arch::asm!("cpsie i"),
        }
    }

    #[cfg(any(test, not(target_os = "none")))]
    pub fn interrupt_mask(_: bool) {}
}

mod utils;
mod list;
mod actor;
pub mod msg;
pub mod channel;
pub mod timer;
pub mod pool;
pub mod executor;

pub use crate::pool::Pool;
pub use crate::timer::Timer;
pub use crate::msg::Message;
pub use crate::actor::{ Actor, FutureStorage, RefWrapper };
pub use crate::channel::Channel;
pub use crate::executor::{ Pic, Executor };

#[cfg(test)]
mod tests {
    use crate::{ Timer, Channel, Pool, Message, Pic, Executor, bind };

    struct DummyPic;
    struct ExampleMsg(u32);
    static TIMER: Timer = Timer::new();
    static POOL: Pool<ExampleMsg> = Pool::new();
    static QUEUE: Channel<ExampleMsg> = Channel::new();

    async fn proxy() -> core::convert::Infallible {
        loop {
            TIMER.sleep_for(100).await;
            let mut msg = POOL.get().await;
            msg.0 = 1;
            QUEUE.put(msg);
        }
    }

    async fn adder() -> core::convert::Infallible {
        let mut sum = 0;
        loop {
            let msg = QUEUE.block_on().await;
            sum += msg.0;
            println!("adder got {} sum = {}", msg.0, sum);
        }
    }

    impl Pic for DummyPic {
        fn interrupt_request(_cpu: u8, _vector: u16) {}
        fn interrupt_prio(_vector: u16) -> u8 { 0 }
    }

    #[test]
    fn main() {
        static mut MSG_STORAGE: [Message<ExampleMsg>; 5] = [const { Message::new(ExampleMsg(0)) }; 5];
        static SCHED: Executor<DummyPic, 1> = Executor::new();
        const TEST_VECT: u16 = 0;

        QUEUE.init();
        TIMER.init();
        SCHED.init();

        unsafe {
            POOL.init(core::ptr::addr_of_mut!(MSG_STORAGE));
        }

        let actor1 = bind!(proxy, TEST_VECT, 0);
        let actor2 = bind!(adder, TEST_VECT, 0);
        SCHED.run([actor1, actor2]);
        
        for _ in 0..10 {
            for _ in 0..100 {
                TIMER.tick();
            }

            SCHED.schedule(TEST_VECT);
        }        
    }
}
