
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Condvar};
use std::cell::UnsafeCell;
use std::thread;
use std::time::Duration;

use coroutine::Handle;
use scheduler::Scheduler;
use runtime::Processor;

enum Executor {
    Thread(Arc<(Mutex<bool>, Condvar)>),
    Coroutine(Handle),
}

pub struct MonoBarrier {
    lock: AtomicUsize,
    waiter: UnsafeCell<Option<Executor>>,
    detached: AtomicBool,
}

unsafe impl Send for MonoBarrier {}
unsafe impl Sync for MonoBarrier {}

#[derive(Debug)]
pub enum LockError {
    Occupied,
}

const EMPTY: usize = 0;
const PROCESSING: usize = 1;
const OCCUPIED: usize = 2;

impl MonoBarrier {
    /// Create a new `MonoBarrier`
    pub fn new() -> MonoBarrier {
        MonoBarrier {
            lock: AtomicUsize::new(0),
            waiter: UnsafeCell::new(None),
            detached: AtomicBool::new(false),
        }
    }

    /// Detach this mono barrier, which will make the `signal` return immediately if no one is
    /// waiting on this `MonoBarrier`.
    pub fn detach(&self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    /// Try to wait the `MonoBarrier`, fail if someone is already waiting
    pub fn wait(&self) -> Result<(), LockError> {
        loop {
            if let Some(p) = Processor::current() {
                match self.lock.compare_and_swap(EMPTY, PROCESSING, Ordering::SeqCst) {
                    // If it is EMPTY, then lock the barrier and yield myself
                    EMPTY => {
                        p.block_with(|_, coro| {
                            let waiter = unsafe { &mut *self.waiter.get() };
                            *waiter = Some(Executor::Coroutine(coro));
                            self.lock.store(OCCUPIED, Ordering::SeqCst);
                        });
                        self.lock.store(EMPTY, Ordering::SeqCst);
                        return Ok(());
                    }
                    // Someone is modifying on this barrier, check it again later
                    PROCESSING => p.sched(),
                    // The barrier is occupied by other, return Err
                    OCCUPIED => return Err(LockError::Occupied),
                    _ => unreachable!(),
                }
            } else {
                match self.lock.compare_and_swap(EMPTY, PROCESSING, Ordering::SeqCst) {
                    EMPTY => {
                        let waiter = unsafe { &mut *self.waiter.get() };

                        {
                            let locker = Arc::new((Mutex::new(false), Condvar::new()));
                            *waiter = Some(Executor::Thread(locker.clone()));
                            let &(ref mutex, ref condvar) = &*locker;
                            let unlocked = mutex.lock().unwrap();
                            self.lock.store(OCCUPIED, Ordering::SeqCst);
                            let (_lock, _) = condvar.wait_timeout_with(unlocked,
                                                                       Duration::from_millis(1),
                                                                       |lock| *lock.unwrap())
                                                    .unwrap();
                            self.lock.store(EMPTY, Ordering::SeqCst);
                        }
                        return Ok(());
                    }
                    PROCESSING => thread::yield_now(),
                    OCCUPIED => return Err(LockError::Occupied),
                    _ => unreachable!(),
                }
            }
        }
    }

    /// Try to signal the waiting executor
    pub fn signal(&self) {
        loop {
            match self.lock.compare_and_swap(OCCUPIED, PROCESSING, Ordering::SeqCst) {
                // No one is waiting on this barrier
                EMPTY => {
                    // Ensure there must be someone waiting on this
                    if self.detached.load(Ordering::SeqCst) {
                        return;
                    }
                    Scheduler::sched();
                }
                // Someone is modifying the barrier, check it again later
                PROCESSING => Scheduler::sched(),
                // Someone is waiting on this barrier, wake him up
                OCCUPIED => {
                    let waiter = unsafe { &mut *self.waiter.get() };
                    match waiter.take().expect("Impossible! Nothing is waiting!") {
                        Executor::Coroutine(coro) => {
                            Scheduler::ready(coro);
                        }
                        Executor::Thread(locker) => {
                            let &(ref mutex, ref condvar) = &*locker;
                            let mut guard = mutex.lock().unwrap();
                            *guard = true;
                            condvar.notify_one();
                        }
                    }
                    return;
                }
                _ => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::sync::Arc;
    use std::thread;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use scheduler::Scheduler;

    #[test]
    fn test_mono_barrier_thread_basic() {
        let barrier = Arc::new(MonoBarrier::new());
        let protected_data = Arc::new(AtomicUsize::new(0));

        {
            let barrier = barrier.clone();
            let protected_data = protected_data.clone();
            thread::spawn(move || {
                barrier.signal();
                protected_data.fetch_add(1, Ordering::SeqCst);
            });
        }

        assert_eq!(protected_data.load(Ordering::SeqCst), 0);
        barrier.wait().unwrap();
        assert_eq!(protected_data.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_mono_barrier_coroutine_basic() {
        Scheduler::new()
            .run(|| {
                let barrier = Arc::new(MonoBarrier::new());
                let protected_data = Arc::new(AtomicUsize::new(0));

                {
                    let barrier = barrier.clone();
                    let protected_data = protected_data.clone();
                    Scheduler::spawn(move || {
                        barrier.signal();
                        protected_data.fetch_add(1, Ordering::SeqCst);
                    });
                }

                assert_eq!(protected_data.load(Ordering::SeqCst), 0);
                barrier.wait().unwrap();
                assert_eq!(protected_data.load(Ordering::SeqCst), 1);
            })
            .unwrap();
    }

    #[test]
    fn test_mono_barrier_thread_wait_coroutine() {
        Scheduler::new()
            .run(|| {
                let barrier = Arc::new(MonoBarrier::new());
                let protected_data = Arc::new(AtomicUsize::new(0));

                let h = {
                    let barrier = barrier.clone();
                    let protected_data = protected_data.clone();
                    thread::spawn(move || {
                        barrier.wait().unwrap();
                        protected_data.fetch_add(1, Ordering::SeqCst);
                    })
                };

                assert_eq!(protected_data.load(Ordering::SeqCst), 0);

                barrier.signal();
                h.join().unwrap();

                assert_eq!(protected_data.load(Ordering::SeqCst), 1);
            })
            .unwrap();
    }

    #[test]
    fn test_mono_barrier_coroutine_wait_thread() {
        Scheduler::new()
            .run(|| {
                let barrier = Arc::new(MonoBarrier::new());
                let protected_data = Arc::new(AtomicUsize::new(0));

                {
                    let barrier = barrier.clone();
                    let protected_data = protected_data.clone();
                    thread::spawn(move || {
                        barrier.signal();
                        protected_data.fetch_add(1, Ordering::SeqCst);
                    });
                }

                assert_eq!(protected_data.load(Ordering::SeqCst), 0);
                barrier.wait().unwrap();
                assert_eq!(protected_data.load(Ordering::SeqCst), 1);
            })
            .unwrap();
    }
}
