//
// Minimal interface to SIO, NVIC and LED.
// Priority Grouping is assumed as [4:0], so priority bits are [7:5].
//
use core::cell::UnsafeCell;
use core::ptr::{with_exposed_provenance, with_exposed_provenance_mut};
use core::ptr::{read_volatile, write_volatile};
use core::arch::asm;

type SharedReg = UnsafeCell<u32>;

pub const SPARE_IRQ: u16 = 46;
pub const DOORBELL_IRQ: u16 = 26;
pub const CPU_NUM: usize = 2;

const IOBANK0_BASE: usize = 0x40028000;
const SIO_BASE: usize = 0xd0000000;
const PADSBANK0_BASE: usize = 0x40038000;
const SYSTICK_BASE: usize = 0xe000e010;
const NVIC_STIR_BASE: usize = 0xe000ef00;
const NVIC_ISER_BASE: usize = 0xe000e100;
const NVIC_IPR_BASE: usize = 0xe000e400;
const ACTLR_BASE: usize = 0xe000e008;
const LED_PIN: usize = 25;
const VECT_MAX: usize = 51;
const SUBPRIO_BITS: u8 = 5;
const EXTEXCLALL_MASK: u32 = 0x20000000;

#[repr(C)]
struct InterpHw {
    accum: [u32; 2],
    base: [u32; 3],
    pop: [u32; 3],
    peek: [u32; 3],
    ctrl: [u32; 2],
    add_raw: [u32; 2],
    base01: u32,
}

#[repr(C)]
struct SioHw {
    cpuid: SharedReg,
    gpio_in: SharedReg,
    gpio_hi_in: SharedReg,
    _pad0: SharedReg,
    gpio_out: SharedReg,
    gpio_hi_out: SharedReg,
    gpio_set: SharedReg,
    gpio_hi_set: SharedReg,
    gpio_clr: SharedReg,
    gpio_hi_clr: SharedReg,
    gpio_togl: SharedReg,
    gpio_hi_togl: SharedReg,
    gpio_oe: SharedReg,
    gpio_hi_oe: SharedReg,
    gpio_oe_set: SharedReg,
    gpio_hi_oe_set: SharedReg,
    gpio_oe_clr: SharedReg,
    gpio_hi_oe_clr: SharedReg,
    gpio_oe_togl: SharedReg,
    gpio_hi_oe_togl: SharedReg,
    fifo_st: SharedReg,
    fifo_wr: SharedReg,
    fifo_rd: SharedReg,
    spinlock_st: u32,
    _pad1: [u32; 8],
    interp: [InterpHw; 2],
    spinlock: [u32; 32],
    doorbell_out_set: SharedReg,
    doorbell_out_clr: SharedReg,
    doorbell_in_set: SharedReg,
    doorbell_in_clr: SharedReg,
}

#[repr(C)]
struct IoBank0StatusCtrlHw {
    status: u32,
    ctrl: u32,
}

#[repr(C)]
struct IoBank0IrqCtrlHw {
    inte: [u32; 6],
    intf: [u32; 6],
    ints: [u32; 6],
}

#[repr(C)]
struct IoBank0Hw {
    io: [IoBank0StatusCtrlHw; 48],
    _pad0: [u32; 32],
    irqsummary_proc0_secure: [u32; 2],
    irqsummary_proc0_nonsecure: [u32; 2],
    irqsummary_proc1_secure: [u32; 2],
    irqsummary_proc1_nonsecure: [u32; 2],
    irqsummary_dormant_wake_secure: [u32; 2],
    irqsummary_dormant_wake_nonsecure: [u32; 2],
    intr: [u32; 6],
    proc0_irq_ctrl: IoBank0IrqCtrlHw,
    proc1_irq_ctrl: IoBank0IrqCtrlHw,
    dormant_wake_irq_ctrl: IoBank0IrqCtrlHw,
}

#[repr(C)]
struct PadsBank0Hw {
    voltage_select: u32,
    io: [u32; 48],
}

#[repr(C)]
struct SysTickHw {
    ctrl: u32,
    load: u32,
    val: u32,
}

fn multicore_fifo_rvalid(sio: &SioHw) -> bool {
    unsafe {
        (read_volatile(sio.fifo_st.get()) & 1) != 0
    }
}

fn multicore_fifo_wready(sio: &SioHw) -> bool {
    unsafe {
        (read_volatile(sio.fifo_st.get()) & 2) != 0
    }
}

fn multicore_fifo_push_blocking(sio: &SioHw, data: u32) {
    while !multicore_fifo_wready(sio) {
        unsafe { asm!("nop") };
    }

    unsafe { 
        write_volatile(sio.fifo_wr.get(), data);
        asm!("sev");
    }
}

fn multicore_fifo_drain(sio: &SioHw) {
    while multicore_fifo_rvalid(sio) {
        let _ = unsafe { read_volatile(sio.fifo_rd.get()) };
    }
}

fn multicore_fifo_pop_blocking(sio: &SioHw) -> u32 {
    while !multicore_fifo_rvalid(sio) {
        unsafe { asm!("wfe") };
    }

    unsafe { read_volatile(sio.fifo_rd.get()) }
}

fn sio_get_ref<'a>() -> &'a SioHw {
    let sio_ptr = with_exposed_provenance::<SioHw>(SIO_BASE);
    unsafe { & *sio_ptr }
}

pub fn core1_start(func: fn() -> !) {
    extern "C" {
        static mut vector_table: u8;
        static mut _estack1: u8;
    }

    let sio = sio_get_ref();
    let vectors = &raw const vector_table as *const u8 as usize as u32;
    let stack_top = &raw const _estack1 as *const u8 as usize as u32;
    let fn_ptr: usize = (func as *const ()) as usize;
    let cmd_sequence = [0, 0, 1, vectors, stack_top, fn_ptr as u32];
    let mut seq = 0usize;

    loop {
        let cmd = cmd_sequence[seq];

        if cmd == 0 {
            multicore_fifo_drain(sio);
            unsafe { asm!("sev") };
        }

        multicore_fifo_push_blocking(sio, cmd);
        let response = multicore_fifo_pop_blocking(sio);
        
        if cmd == response {
            seq += 1;
        } else {
            seq = 0
        }
        
        if seq == cmd_sequence.len() {
            break;
        }
    }
}

pub fn led_config() {
    let sio = sio_get_ref();
    let iobank0_ptr = with_exposed_provenance_mut::<IoBank0Hw>(IOBANK0_BASE);
    let padsbank0_ptr = with_exposed_provenance_mut::<PadsBank0Hw>(PADSBANK0_BASE);

    unsafe {
        let iobank0 = &mut *iobank0_ptr;
        let padsbank0 = &mut *padsbank0_ptr;
        
        write_volatile(&mut iobank0.io[LED_PIN].ctrl, 5);
        write_volatile(&mut padsbank0.io[LED_PIN], 0x34);
        write_volatile(sio.gpio_oe_set.get(), 1 << LED_PIN);
    }
}

pub fn led_toggle() {
    let sio = sio_get_ref();
    unsafe { write_volatile(sio.gpio_togl.get(), 1 << LED_PIN) };
}

pub fn cpu_this() -> usize {
    let sio = sio_get_ref();
    (unsafe { read_volatile(sio.cpuid.get()) }) as usize
}

pub fn doorbell_set() {
    let sio = sio_get_ref();
    unsafe { write_volatile(sio.doorbell_out_set.get(), 1) }
}

pub fn doorbell_clr() {
    let sio = sio_get_ref();
    unsafe { write_volatile(sio.doorbell_in_clr.get(), 1) }
}

pub fn nvic_interrupt_request(vect: u16) {
    let stir = with_exposed_provenance_mut::<u32>(NVIC_STIR_BASE);
    unsafe { write_volatile(stir, vect as u32) }
}

pub fn nvic_interrupt_enable(vect: u16) {
    const LOG2_IRQ_PER_REG: u8 = 5;
    let index = (vect >> LOG2_IRQ_PER_REG) as usize;
    let bit = vect & ((1 << LOG2_IRQ_PER_REG) - 1);    
    unsafe {
        let iser = &mut *with_exposed_provenance_mut::<[u32; 2]>(NVIC_ISER_BASE);
        write_volatile(&mut iser[index], 1 << bit);
    }
}

pub fn nvic_vect2prio(vect: u16) -> u8 {
    unsafe {
        let ipr = &*with_exposed_provenance::<[UnsafeCell<u8>; VECT_MAX]>(NVIC_IPR_BASE);
        read_volatile(ipr[vect as usize].get()) >> SUBPRIO_BITS
    }
}

pub fn actlr_init() {
    let actlr = with_exposed_provenance_mut::<u32>(ACTLR_BASE);
    unsafe {
        let prev = read_volatile(actlr);
        write_volatile(actlr, prev | EXTEXCLALL_MASK);
    }
}

pub fn systick_config(ticks: u32) {
    unsafe {
        let systick = &mut *with_exposed_provenance_mut::<SysTickHw>(SYSTICK_BASE);
        write_volatile(&mut systick.load, ticks);
        write_volatile(&mut systick.val, 0);
        write_volatile(&mut systick.ctrl, 7);
    }
}

