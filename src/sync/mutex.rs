// The MIT License (MIT)

// Copyright (c) 2015 Y. T. Chung <zonyitoo@gmail.com>

//  Permission is hereby granted, free of charge, to any person obtaining a
//  copy of this software and associated documentation files (the "Software"),
//  to deal in the Software without restriction, including without limitation
//  the rights to use, copy, modify, merge, publish, distribute, sublicense,
//  and/or sell copies of the Software, and to permit persons to whom the
//  Software is furnished to do so, subject to the following conditions:
//
//  The above copyright notice and this permission notice shall be included in
//  all copies or substantial portions of the Software.
//
//  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//  OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//  FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//  AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//  LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//  FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//  DEALINGS IN THE SOFTWARE.

use std::cell::UnsafeCell;
use std::fmt;
use std::error::Error;
use std::marker::Reflect;
use std::ops::{Deref, DerefMut};
use std::time::Duration;

use sync::semaphore::Semaphore;
use runtime::notifier::{Notifier, WaiterState};
use coroutine::HandleList;
use scheduler::Scheduler;

pub type LockResult<G> = Result<G, PoisonError<G>>;
pub type TryLockResult<G> = Result<G, PoisonError<G>>;

/// A mutual exclusion primitive useful for protecting shared data
pub struct Mutex<T> {
    data: UnsafeCell<T>,
    sema: Semaphore,
}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub fn new(data: T) -> Mutex<T> {
        Mutex {
            data: UnsafeCell::new(data),
            sema: Semaphore::new(1),
        }
    }

    /// Acquires a mutex, blocking the current thread until it is able to do so.
    pub fn lock(&self) -> LockResult<Guard<T>> {
        self.sema.acquire();
        Ok(Guard::new(unsafe { &mut *self.data.get() }, self))
    }

    /// Try to acquire a mutex, will return immediately
    pub fn try_lock(&self) -> TryLockResult<Guard<T>> {
        if self.sema.try_acquire() {
            Err(PoisonError::new(Guard::new(unsafe { &mut *self.data.get() }, self)))
        } else {
            Ok(Guard::new(unsafe { &mut *self.data.get() }, self))
        }
    }
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Sync> Sync for Mutex<T> {}

/// An RAII implementation of "scoped lock" of a mutex. When this structure is dropped,
/// the lock will be unlocked.
#[must_use]
pub struct Guard<'a, T: 'a> {
    data: &'a mut T,
    mutex: &'a Mutex<T>,
}

impl<'a, T: 'a> Guard<'a, T> {
    fn new(data: &'a mut T, mutex: &'a Mutex<T>) -> Guard<'a, T> {
        Guard {
            data: data,
            mutex: mutex,
        }
    }
}

impl<'a, T: 'a> Drop for Guard<'a, T> {
    fn drop(&mut self) {
        self.mutex.sema.release();
    }
}

impl<'a, T: 'a> Deref for Guard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.data
    }
}

impl<'a, T: 'a> DerefMut for Guard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}

/// A type of error which can be returned whenever a lock is acquired.
///
/// Currently this error does not act just like the
/// [`PoisonError`](rust:#std::sync::PoisonError) does.
pub struct PoisonError<T> {
    guard: T,
}

impl<T> PoisonError<T> {
    pub fn new(guard: T) -> PoisonError<T> {
        PoisonError { guard: guard }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.guard
    }

    #[inline]
    pub fn get_ref(&self) -> &T {
        &self.guard
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> fmt::Debug for PoisonError<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        "PoisonError { inner: .. }".fmt(f)
    }
}

impl<T> fmt::Display for PoisonError<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        "poisoned lock: another task failed inside".fmt(f)
    }
}

impl<T: Send + Reflect> Error for PoisonError<T> {
    fn description(&self) -> &str {
        "poisoned lock: another task failed inside"
    }
}

pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    pub fn timed_out(&self) -> bool {
        self.0
    }
}

/// A Condition variable
pub struct Condvar {
    notifier: Notifier,
}

impl Condvar {
    /// Creates a new condition variable which is ready to be waited on and notified.
    pub fn new() -> Condvar {
        Condvar { notifier: Notifier::default() }
    }

    /// Blocks the current coroutine until this condition variable receives a notification.
    pub fn wait<'a, T>(&self, guard: Guard<'a, T>) -> LockResult<Guard<'a, T>> {
        let mutex = guard.mutex;
        drop(guard);
        self.notifier.wait();
        mutex.lock()
    }

    /// Waits on this condition variable for a notification, timing out after a specified duration.
    pub fn wait_timeout<'a, T>(&self,
                               guard: Guard<'a, T>,
                               dur: Duration)
                               -> LockResult<(Guard<'a, T>, WaitTimeoutResult)> {
        let mutex = guard.mutex;
        drop(guard);
        let r = match self.notifier.wait_timeout(dur) {
            WaiterState::Succeeded => WaitTimeoutResult(false),
            WaiterState::Timedout => WaitTimeoutResult(true),
            _ => panic!("Invalid state"),
        };

        match mutex.lock() {
            Ok(g) => Ok((g, r)),
            Err(pg) => Err(PoisonError { guard: (pg.guard, r) }),
        }
    }

    /// Wakes up one blocked coroutine on this condvar.
    pub fn notify_one(&self) {
        if let Some(hdl) = self.notifier.notify_one(WaiterState::Succeeded) {
            Scheduler::ready(hdl);
        }
    }

    /// Wakes up all blocked coroutine on this condvar.
    pub fn notify_all(&self) {
        let mut hlist = HandleList::new();
        self.notifier.notify_all(WaiterState::Succeeded, &mut hlist);

        while let Some(h) = hlist.pop_front() {
            Scheduler::ready(h);
        }
    }
}

impl Default for Condvar {
    fn default() -> Condvar {
        Condvar::new()
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::time::Duration;

    use scheduler::Scheduler;

    use super::*;

    #[test]
    fn test_mutex() {
        let num = Arc::new(Mutex::new(0));

        let num_cloned = num.clone();
        Scheduler::new()
            .with_workers(1)
            .run(move || {
                let mut handlers = Vec::new();

                for _ in 0..100 {
                    let num = num_cloned.clone();
                    let hdl = Scheduler::spawn(move || {
                        for _ in 0..10 {
                            let mut guard = num.lock().unwrap();
                            *guard += 1;
                            Scheduler::sched();
                        }
                    });
                    handlers.push(hdl);
                }

                for hdl in handlers {
                    hdl.join().unwrap();
                }
            })
            .unwrap();

        assert_eq!(*num.lock().unwrap(), 1000);
    }

    #[test]
    fn condvar_basic() {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        Scheduler::new()
            .with_workers(1)
            .run(move || {
                let cloned = pair.clone();
                Scheduler::spawn(move || {
                    let mut guard = cloned.0.lock().unwrap();
                    *guard = true;
                    cloned.1.notify_one();
                });

                let mut guard = pair.0.lock().unwrap();
                while !*guard {
                    guard = pair.1.wait(guard).unwrap();
                }
            })
            .unwrap();
    }

    #[test]
    fn condvar_timeout() {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        Scheduler::new()
            .with_workers(1)
            .run(move || {
                let guard = pair.0.lock().unwrap();
                let (_, t) = pair.1.wait_timeout(guard, Duration::from_millis(1)).unwrap();
                assert!(t.timed_out());
            })
            .unwrap();
    }
}
