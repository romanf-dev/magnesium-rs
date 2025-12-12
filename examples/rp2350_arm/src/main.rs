#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::convert::Infallible;
use core::ptr::addr_of_mut;
use mg::{ Pic, Executor, Timer, Pool, Channel, Message, bind };

mod periph;
mod ipi;

const CPU_MAX: usize = periph::CPU_NUM;
const SCHED_VECT: u16 = periph::SPARE_IRQ;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".exceptions")]
pub fn hard_fault_isr() -> ! {
    loop {}
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".exceptions")]
pub fn systick_isr() {
    TIMER.tick();
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".exceptions")]
pub fn scheduling_isr() {
    SCHED.schedule(SCHED_VECT);
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".exceptions")]
pub fn doorbell_isr() {
    ipi::local_handler();
}

#[unsafe(no_mangle)]
pub fn cpu_this() -> u8 {
    periph::cpu_this() as u8
}
        
fn cpu_init() {
    periph::actlr_init();
    periph::systick_config(12000);
    periph::nvic_interrupt_enable(SCHED_VECT);
    periph::nvic_interrupt_enable(periph::DOORBELL_IRQ);
}

struct Nvic;

impl Pic for Nvic {
    fn interrupt_request(cpu: u8, vect: u16) {
        ipi::request(cpu, vect);
    }

    fn interrupt_prio(vector: u16) -> u8 {
        periph::nvic_vect2prio(vector)
    }
}

struct ExampleMsg(u32);

static POOL: Pool<ExampleMsg> = Pool::new();
static CHAN: Channel<ExampleMsg> = Channel::new();
static SCHED: Executor<Nvic, 4, CPU_MAX> = Executor::new();
static TIMER: Timer<10, CPU_MAX> = Timer::new();

async fn sender() -> Infallible {
    loop {
        let _ = TIMER.sleep_for(1000).await;
        let mut msg = POOL.get().await;
        msg.0 = 0;
        CHAN.put(msg);
    }
}

async fn receiver() -> Infallible {
    let q = &CHAN;
    loop {
        let _ = q.block_on().await;
        periph::led_toggle();
    }
}

fn core1_entry() -> ! {
    cpu_init();
    let actor = mg::bind!(receiver, SCHED_VECT, 0, 1);
    SCHED.run([ actor ]);
    loop {}
}

#[unsafe(no_mangle)]
pub fn _start() -> ! {
    periph::led_config();
    SCHED.init();
    TIMER.init();
    CHAN.init();

    static mut MSGS: [Message<ExampleMsg>; 5] = [const { Message::new(ExampleMsg(0)) }; 5];
    unsafe { POOL.init(addr_of_mut!(MSGS)); }

    let actor = bind!(sender, SCHED_VECT, 0);

    cpu_init();
    periph::core1_start(core1_entry);
    SCHED.run([ actor ]);
    loop {}
}

