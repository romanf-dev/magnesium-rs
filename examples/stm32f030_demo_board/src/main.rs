#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::ptr::read_volatile;
use core::ptr::write_volatile;
use core::convert::Infallible;
use core::ptr::addr_of_mut;
use mg::mg::{ Executor, Timer, Pool, Queue, Message };

#[repr(C)]
struct RCC {
    cr: u32,
    cfgr: u32,
    cir: u32,
    apb2rstr: u32,
    apb1rstr: u32,
    ahbenr: u32,
    apb2enr: u32,
    apb1enr: u32,
    bdcr: u32,
    csr: u32
}

#[repr(C)]
struct GPIO {
    moder: u32,
    otyper: u32,
    ospeedr: u32,
    pupdr: u32,
    idr: u32,
    odr: u32,
    bsrr: u32,
}

#[repr(C)]
struct SysTick {
    ctrl: u32,
    load: u32,
    val: u32,
    calib: u32
}

const GPIO_MODER_MODER4_0: u32 = 1 << 8;
const GPIO_BSRR_BR4: u32 =  1 << 20;
const GPIO_BSRR_BS4: u32 =  1 << 4;

const RCC_CR_HSION: u32 = 1 << 0;
const RCC_CR_HSEON: u32 = 1 << 16;
const RCC_CR_HSERDY: u32 = 1 << 17;
const RCC_CR_PLLON: u32 = 1 << 24;
const RCC_CR_PLLRDY: u32 = 1 << 25;
const RCC_CFGR_SW_HSE: u32 = 1;
const RCC_CFGR_SW_PLL: u32 = 2;
const RCC_CFGR_PLLMUL6: u32 = 4 << 18;
const RCC_CFGR_PLLSRC_1: u32 = 1 << 16;
const RCC_CFGR_SWS_PLL: u32 = 8;
const RCC_AHBENR_GPIOAEN: u32 = 1 << 17;

const FLASH_ACR_PRFTBE: u32 = 1 << 4;
const FLASH_ACR_LATENCY: u32 = 1 << 0;

const GPIO_ADDR: usize = 0x48000000;
const ISPR_ADDR: usize = 0xe000e200;
const SYSTICK_ADDR: usize = 0xe000e010;
const ISER0_ADDR: usize = 0xe000e100;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
pub fn _exception() -> ! {
    loop {}
}

#[no_mangle]
pub fn _pendsv() -> ! {
    loop {}
}

struct ExampleMsg {
    n: u32
}

static POOL: Pool<ExampleMsg> = Pool::NEW;
static QUEUE: Queue<ExampleMsg> = Queue::new();
static SCHED: Executor = Executor::new();
static TIMER: Timer<10> = Timer::new();

fn led_control(state: bool) {
    unsafe {
        let gpio = &mut *(GPIO_ADDR as *mut GPIO);

        if state {
            write_volatile(&mut gpio.bsrr, GPIO_BSRR_BR4);
        } else {
            write_volatile(&mut gpio.bsrr, GPIO_BSRR_BS4);
        }
    }
}

async fn sender() -> Infallible {
    loop {
        let _ = TIMER.sleep_for(100).await;
        let mut msg = POOL.get().await;
        msg.n = 1;
        QUEUE.put(msg);
        let _ = TIMER.sleep_for(100).await;
        let mut msg = POOL.get().await;
        msg.n = 0;
        QUEUE.put(msg);
    }
}

async fn receiver() -> Infallible {
    let q = &QUEUE;
    loop {
        let msg = q.block_on().await;
        led_control(msg.n == 1);
    }
}

#[no_mangle]
pub fn _systick() {
    TIMER.tick();    
}

#[no_mangle]
pub fn _interrupt() {
    // TODO: read IPSR
    SCHED.schedule(0);
}

#[no_mangle]
pub fn interrupt_request(vect: u16) {
    unsafe {    
        let ispr = &mut *(ISPR_ADDR as *mut u32);
        write_volatile(ispr, 1u32 << vect);
    }
}

#[no_mangle]
pub fn interrupt_prio(_vect: u16) -> u8 {
    0
}

unsafe fn bit_set(addr: &mut u32, bits: u32) -> () {
    let value = read_volatile(addr);
    write_volatile(addr, value | bits);
}

unsafe fn bit_clear(addr: &mut u32, bits: u32) -> () {
    let value = read_volatile(addr);
    write_volatile(addr, value & !bits);
}

#[no_mangle]
pub fn _start() -> ! {
    let rcc_raw = 0x40021000 as *mut RCC;
    let flash_acr = 0x40022000 as *mut u32;

    unsafe {
        let rcc = &mut *rcc_raw;

        //
        // Enable HSE and wait until it is ready.
        //
        bit_set(&mut rcc.cr, RCC_CR_HSEON);
        while (read_volatile(&rcc.cr) & RCC_CR_HSERDY) == 0 {
        }

        //
        // Configure latency (1 should be used if sysclk = 48Mhz).
        //
        write_volatile(flash_acr, FLASH_ACR_PRFTBE | FLASH_ACR_LATENCY);

        //
        // Switch to HSE, configure PLL multiplier and set HSE as PLL source.
        //
        bit_set(&mut rcc.cfgr, RCC_CFGR_SW_HSE | RCC_CFGR_PLLMUL6 | RCC_CFGR_PLLSRC_1);

        //
        // Enable PLL and wait until it is ready.
        //
        bit_set(&mut rcc.cr, RCC_CR_PLLON);
        while (read_volatile(&rcc.cr) & RCC_CR_PLLRDY) == 0 {
        }

        //
        // Set PLL as clock source and wait until it switches.
        //
        let mut cfgr = read_volatile(&rcc.cfgr);

        bit_set(&mut cfgr, RCC_CFGR_SW_PLL);
        bit_clear(&mut cfgr, RCC_CFGR_SW_HSE);

        write_volatile(&mut rcc.cfgr, cfgr);
        while (read_volatile(&rcc.cfgr) & RCC_CFGR_SWS_PLL) == 0 {
        }

        //
        // The CPU is now running at 48MHz frequency.
        // It is safe to disable HSI.
        //
        bit_clear(&mut rcc.cr, RCC_CR_HSION);

        //
        // Enable IRQ0 to be used as actor's vector.
        //
        let iser0 = &mut *(ISER0_ADDR as *mut u32);
        write_volatile(iser0, 1);

        //
        // Configure the LED.
        //
        let gpio = &mut *(GPIO_ADDR as *mut GPIO);
        bit_set(&mut rcc.ahbenr, RCC_AHBENR_GPIOAEN);
        bit_set(&mut gpio.moder, GPIO_MODER_MODER4_0);

        //
        // Configure SysTick for 100ms period.
        //
        let systick = &mut *(SYSTICK_ADDR as *mut SysTick);
        write_volatile(&mut systick.load, 48000u32);
        write_volatile(&mut systick.val, 0);
        write_volatile(&mut systick.ctrl, 7);
    }
    
    TIMER.init();
    const PROTO: Message<ExampleMsg> = Message::new(ExampleMsg { n: 0 });
    static mut MSGS: [Message<ExampleMsg>; 5] = [PROTO; 5];

    QUEUE.init();
    unsafe { POOL.init(addr_of_mut!(MSGS)); }
    let mut snd = sender();
    let mut rcv = receiver();

    SCHED.run([
        (0, &mut snd),
        (0, &mut rcv),
    ]);
}

