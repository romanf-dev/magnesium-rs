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
pub use crate::actor::Actor;
pub use crate::channel::Channel;
pub use crate::executor::{ Pic, Executor };

#[cfg(test)]
mod tests {
    use crate::{ Timer, Channel, Pool, Message, Actor, Pic, Executor };

    type MsgChannel = Channel<'static, ExampleMsg>;

    struct ExampleMsg(u32);
    static TIMER: Timer = Timer::new();
    static POOL: Pool<ExampleMsg> = Pool::new();

    async fn proxy(q: &'static MsgChannel) -> core::convert::Infallible {
        loop {
            TIMER.sleep_for(100).await;
            let mut msg = POOL.get().await;
            msg.0 = 1;
            q.put(msg);
        }
    }

    async fn adder(q: &'static MsgChannel) -> core::convert::Infallible {
        let mut sum = 0;
        loop {
            let msg = q.block_on().await;
            sum += msg.0;
            println!("adder got {} sum = {}", msg.0, sum);
        }
    }

    struct DummyPic;

    impl Pic for DummyPic {
        fn interrupt_request(_cpu: u8, _vector: u16) {}
        fn interrupt_prio(_vector: u16) -> u8 { 0 }
    }

    #[test]
    fn main() {
        static mut MSG_STORAGE: [Message<ExampleMsg>; 5] = [const { Message::new(ExampleMsg(0)) }; 5];
        static QUEUE: Channel<ExampleMsg> = Channel::new();
        static SCHED: Executor<DummyPic, 1> = Executor::new();
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
