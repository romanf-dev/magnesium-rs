//
// This module implements Inter-processor-interrupts interface (IPI).
// RP2350 cannot send arbitrary interrupts from one core to another so
// the process is two-staged in case when requested interrupt is targeted to
// the another core:
// 1) vector is translated to the priority
// 2) corresponding priority bit is atomically marked as pending in static var
// 3) doorbell interrupt is sent to the target core
// 4) target core doorbell handler:
//      - reads previously marked priority
//      - translates it into local vector
//      - requests local interrupt using local NVIC
// Currently, it is assumed that priorities are mapped to vectors linearly.
//
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering;
use crate::periph;

static PENDING_REQ: [AtomicU32; periph::CPU_NUM] = [const { AtomicU32::new(0) }; periph::CPU_NUM];

fn prio2vect(prio: u16) -> u16 {
    prio + periph::SPARE_IRQ        /* WARNING! NVIC priorities have to be set like this! */
}

pub fn request(target_cpu: u8, vect: u16) {
    let cpu = target_cpu as usize;
    if cpu != periph::cpu_this() {
        let prio = periph::nvic_vect2prio(vect);
        PENDING_REQ[cpu as usize].fetch_or(1 << prio, Ordering::SeqCst);
        periph::doorbell_set();
    } else {
        periph::nvic_interrupt_request(vect);
    }
}

pub fn local_handler() {
    periph::doorbell_clr();
    let cpu = periph::cpu_this();
    let mut prev = PENDING_REQ[cpu].swap(0, Ordering::SeqCst);

    while prev != 0 {
        let prio = 31u16 - prev.leading_zeros() as u16;
        let vect = prio2vect(prio);
        prev &= !(1 << prio);
        periph::nvic_interrupt_request(vect);
    }
}

