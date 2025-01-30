/*
 * Multiprocessor synchronization primitives. Only used when feature=smp.
 */

pub(crate) mod sync {
    use core::sync::atomic::{AtomicBool, Ordering};

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
        old_mask: u8,
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
            let old_mask = unsafe { crate::hw::interrupt_mask(1) };
            Self::acquire(lock);
            Self { old_mask, lock }
        }

        pub fn window(&self, func: impl FnOnce()) {
            assert!(self.old_mask == 0);
            unsafe {
                Self::release(self.lock);
                crate::hw::interrupt_mask(0);
                func();
                crate::hw::interrupt_mask(1);
                Self::acquire(self.lock);
            }
        }
    }

    impl<'a> Drop for CriticalSection<'a> {
        fn drop(&mut self) {
            Self::release(self.lock);
            unsafe { crate::hw::interrupt_mask(self.old_mask) };
        }
    }
}
