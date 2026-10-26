//! Experimental parking_lot adapter for State only. Preserve the production
//! fail-closed poisoning contract, including reacquisition after Condvar::wait.
use std::{
    ops::{Deref, DerefMut},
    sync::{
        LockResult, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct Mutex<T> {
    inner: parking_lot::Mutex<T>,
    poisoned: AtomicBool,
}
impl<T> Mutex<T> {
    pub(super) fn new(value: T) -> Self {
        Self {
            inner: parking_lot::Mutex::new(value),
            poisoned: AtomicBool::new(false),
        }
    }
    pub(super) fn lock(&self) -> LockResult<Guard<'_, T>> {
        let guard = Guard {
            inner: self.inner.lock(),
            mutex: self,
            panicking: std::thread::panicking(),
        };
        guard.checked()
    }
    pub(super) fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }
}

pub(super) struct Guard<'a, T> {
    inner: parking_lot::MutexGuard<'a, T>,
    mutex: &'a Mutex<T>,
    panicking: bool,
}
impl<T> Guard<'_, T> {
    fn checked(self) -> LockResult<Self> {
        if self.mutex.is_poisoned() {
            Err(PoisonError::new(self))
        } else {
            Ok(self)
        }
    }
}
impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}
impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}
impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        if !self.panicking && std::thread::panicking() {
            self.mutex.poisoned.store(true, Ordering::Release);
        }
    }
}

pub(super) struct Condvar(parking_lot::Condvar);
impl Condvar {
    pub(super) fn new() -> Self {
        Self(parking_lot::Condvar::new())
    }
    pub(super) fn wait<'a, T>(&self, mut guard: Guard<'a, T>) -> LockResult<Guard<'a, T>> {
        self.0.wait(&mut guard.inner);
        guard.checked()
    }
    pub(super) fn notify_all(&self) {
        self.0.notify_all();
    }
    pub(super) fn notify_one(&self) {
        self.0.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, mpsc},
        thread,
    };

    #[test]
    fn panic_marks_state_poisoned_before_unlock() {
        let lock = Mutex::new(0);
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let mut guard = lock.lock().unwrap();
                *guard = 1;
                panic!("partial mutation");
            }))
            .is_err()
        );
        assert!(lock.is_poisoned());
        assert!(lock.lock().is_err());
    }

    #[test]
    fn condvar_reacquisition_detects_another_threads_panic() {
        let state = Arc::new((Mutex::new(false), Condvar::new()));
        let (ready, entered) = mpsc::channel();
        let waiting = state.clone();
        let waiter = thread::spawn(move || {
            let guard = waiting.0.lock().unwrap();
            ready.send(()).unwrap();
            waiting.1.wait(guard).is_err()
        });
        entered.recv().unwrap();
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let mut guard = state.0.lock().unwrap();
                *guard = true;
                state.1.notify_all();
                panic!("partial mutation while waiter sleeps");
            }))
            .is_err()
        );
        assert!(waiter.join().unwrap());
    }
}
