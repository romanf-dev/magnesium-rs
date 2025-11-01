/*
 * Multiprocessor synchronization primitives. Only used when feature=smp.
 */

#[cfg(not(feature = "smp"))] /* single-CPU case is implemented here... */
pub(crate) mod sync {
    pub const fn cpu_this() -> u8 {
        0
    }

    pub struct SmpProtection;
    pub struct CriticalSection;

    impl SmpProtection {
        pub const fn new() -> Self {
            Self
        }
    }

    impl CriticalSection {
        pub fn new(_: &SmpProtection) -> Self {
            unsafe { crate::hw::interrupt_mask(true) };
            Self
        }

        pub fn window(&self, func: impl FnOnce()) {
            unsafe {
                crate::hw::interrupt_mask(false);
                func();
                crate::hw::interrupt_mask(true);
            }
        }
    }

    impl Drop for CriticalSection {
        fn drop(&mut self) {
            unsafe { crate::hw::interrupt_mask(false) };
        }
    }
}

#[cfg(feature = "smp")]
pub(crate) mod sync {
    use core::sync::atomic::{ AtomicBool, Ordering };

    #[cfg(target_arch = "arm")]
    extern "C" {
        pub fn cpu_this() -> u8;
    }

    pub struct SmpProtection {
        spinlock: AtomicBool,
    }

    impl SmpProtection {
        pub const fn new() -> Self {
            Self {
                spinlock: AtomicBool::new(false),
            }
        }
    }

    pub struct CriticalSection<'a> {
        lock: &'a SmpProtection,
    }

    impl<'a> CriticalSection<'a> {
        fn acquire(lock: &'a SmpProtection) {
            let mut old = lock.spinlock.load(Ordering::Relaxed);
            loop {
                match lock.spinlock.compare_exchange_weak(
                    old,
                    true,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(x) => old = x,
                }
            }
        }

        fn release(lock: &'a SmpProtection) {
            lock.spinlock.store(false, Ordering::Release);
        }

        pub fn new(lock: &'a SmpProtection) -> Self {
            unsafe { crate::hw::interrupt_mask(true) };
            Self::acquire(lock);
            Self { lock }
        }

        pub fn window(&self, func: impl FnOnce()) {
            unsafe {
                Self::release(self.lock);
                crate::hw::interrupt_mask(false);
                func();
                crate::hw::interrupt_mask(true);
                Self::acquire(self.lock);
            }
        }
    }

    impl<'a> Drop for CriticalSection<'a> {
        fn drop(&mut self) {
            Self::release(self.lock);
            unsafe { crate::hw::interrupt_mask(false) };
        }
    }
}
