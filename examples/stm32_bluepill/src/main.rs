#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::ptr::read_volatile;
use core::ptr::write_volatile;
use core::convert::Infallible;
use mg::mg::{ Queue, Message, Pool, Actor, Executor };

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
    crl: u32,
    crh: u32,
    idr: u32,
    odr: u32,
    bsrr: u32,
    brr: u32,
    lckr: u32
}

#[repr(C)]
struct SysTick {
    ctrl: u32,
    load: u32,
    val: u32,
    calib: u32
}

const GPIO_CRH_CNF13_0: u32 = 1 << 22;
const GPIO_CRH_MODE13_1: u32 = 2 << 20;
const GPIO_BSRR_BR13: u32 =  1 << 29;
const GPIO_BSRR_BS13: u32 =  1 << 13;

const RCC_APB2ENR_IOPCEN: u32 = 1 << 4;
const RCC_CR_HSION: u32 = 1 << 0;
const RCC_CR_HSEON: u32 = 1 << 16;
const RCC_CR_HSERDY: u32 = 1 << 17;
const RCC_CR_PLLON: u32 = 1 << 24;
const RCC_CR_PLLRDY: u32 = 1 << 25;
const RCC_CFGR_SW_HSE: u32 = 1;
const RCC_CFGR_SW_PLL: u32 = 2;
const RCC_CFGR_PLLMULL9: u32 = 7 << 18;
const RCC_CFGR_PLLSRC: u32 = 1 << 16;
const RCC_CFGR_SWS_PLL: u32 = 8;

const FLASH_ACR_PRFTBE: u32 = 1 << 4;
const FLASH_ACR_LATENCY_1: u32 = 2 << 0;

const GPIO_ADDR: usize = 0x40011000;
const STIR_ADDR: usize = 0xe000ef00;
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

static POOL: Pool<ExampleMsg> = Pool::new();
static QUEUE: Queue<ExampleMsg> = Queue::new();
static SCHED: Executor = Executor::new();

fn led_control(state: bool) {
    unsafe {
        let gpio = &mut *(GPIO_ADDR as *mut GPIO);

        if state {
            write_volatile(&mut gpio.bsrr, GPIO_BSRR_BR13);
        } else {
            write_volatile(&mut gpio.bsrr, GPIO_BSRR_BS13);
        }
    }
}

async fn blinky() -> Infallible {
    let q = &QUEUE;
    loop {
        let _ = q.await;        
        led_control(true);
        let _ = q.await;
        led_control(false);
    }
}

#[no_mangle]
pub fn _systick() {
    let mut msg = POOL.alloc().unwrap();
    msg.n = 1;
    QUEUE.put(msg);
}

#[no_mangle]
pub fn _interrupt() {
    SCHED.schedule(0);
}

#[no_mangle]
pub fn interrupt_request(vect: u16) {
    unsafe {    
        let stir = &mut *(STIR_ADDR as *mut u32);
        write_volatile(stir, vect as u32);
    }
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
        while (read_volatile(&rcc.cr) & RCC_CR_HSERDY) == 0 {}

        //
        // Configure latency (0b010 should be used if sysclk > 48Mhz).
        //
        write_volatile(flash_acr, FLASH_ACR_PRFTBE | FLASH_ACR_LATENCY_1);

        //
        // Switch to HSE, configure PLL multiplier and set HSE as PLL source.
        //
        bit_set(&mut rcc.cfgr, RCC_CFGR_SW_HSE | RCC_CFGR_PLLMULL9 | RCC_CFGR_PLLSRC);

        //
        // Enable PLL and wait until it is ready.
        //
        bit_set(&mut rcc.cr, RCC_CR_PLLON);
        while (read_volatile(&rcc.cr) & RCC_CR_PLLRDY) == 0 {}

        //
        // Set PLL as clock source and wait until it switches.
        //
        let mut cfgr = read_volatile(&rcc.cfgr);

        bit_set(&mut cfgr, RCC_CFGR_SW_PLL);
        bit_clear(&mut cfgr, RCC_CFGR_SW_HSE);

        write_volatile(&mut rcc.cfgr, cfgr);
        while (read_volatile(&rcc.cfgr) & RCC_CFGR_SWS_PLL) == 0 {}

        //
        // The CPU is now running at 72MHz frequency.
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
        bit_set(&mut rcc.apb2enr, RCC_APB2ENR_IOPCEN);
        bit_set(&mut gpio.crh, GPIO_CRH_CNF13_0 | GPIO_CRH_MODE13_1);

        //
        // Configure SysTick for 100ms period.
        //
        let systick = &mut *(SYSTICK_ADDR as *mut SysTick);
        write_volatile(&mut systick.load, 72000 * 100);
        write_volatile(&mut systick.val, 0);
        write_volatile(&mut systick.ctrl, 7);
    }
    
    const PROTO: Message<ExampleMsg> = Message::new(ExampleMsg { n: 0 });
    static mut MSGS: [Message<ExampleMsg>; 5] = [PROTO; 5];
    static mut ACTOR: Actor = Actor::new(0, 0);

    QUEUE.init();
    let mut future = blinky();

    unsafe {
        POOL.init(&mut MSGS);
        SCHED.run([(&mut ACTOR, &mut future)]);
    }
}
