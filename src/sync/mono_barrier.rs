use std::mem;
use std::ptr;
use std::sync::{Condvar, Mutex};
use std::sync::atomic::{AtomicPtr, Ordering};

use coroutine::{Coroutine, Handle};
use runtime::Processor;
use scheduler::Scheduler;

enum State {
    Empty,
    Ready,
    Thread,
    Coroutine(Handle),
}

pub struct MonoBarrier {
    lock: Mutex<State>,
    cond: Condvar,
}

unsafe impl Send for MonoBarrier {}
unsafe impl Sync for MonoBarrier {}

#[derive(Debug, PartialEq, Eq)]
pub enum MonoBarrierError {
    Occupied,
    PoisonError,
}

impl MonoBarrier {
    /// Create a new `MonoBarrier`
    pub fn new() -> MonoBarrier {
        MonoBarrier {
            lock: Mutex::new(State::Empty),
            cond: Condvar::new(),
        }
    }

    /// Try to wait the `MonoBarrier`, fail if someone is already waiting
    pub fn wait(&self) -> Result<(), MonoBarrierError> {
        let mut guard = match self.lock.lock() {
            Err(_) => return Err(MonoBarrierError::PoisonError),
            Ok(guard) => guard,
        };

        loop {
            match &*guard {
                &State::Ready => {
                    *guard = State::Empty;
                    return Ok(());
                }
                &State::Empty => {
                    match Processor::current() {
                        Some(p) => {
                            p.block_with(move |_, coro| {
                                *guard = State::Coroutine(coro);
                                drop(guard);
                            });

                            return Ok(());
                        }
                        None => {
                            *guard = State::Thread;
                            guard = match self.cond.wait(guard) {
                                Err(_) => return Err(MonoBarrierError::PoisonError),
                                Ok(guard) => guard,
                            };
                        }
                    };
                }
                _ => return Err(MonoBarrierError::Occupied),
            }
        }
    }

    /// Try to signal the waiting executor
    pub fn signal(&self) {
        let mut guard = self.lock.lock().unwrap();

        match mem::replace(&mut *guard, State::Empty) {
            State::Empty | State::Ready => {
                *guard = State::Ready;
            }
            State::Coroutine(coro) => {
                *guard = State::Empty;
                Scheduler::ready(coro);
            }
            State::Thread => {
                *guard = State::Ready;
                self.cond.notify_one();
            }
        };
    }
}

pub struct CoroMonoBarrier {
    lock: AtomicPtr<Coroutine>,
}

unsafe impl Send for CoroMonoBarrier {}
unsafe impl Sync for CoroMonoBarrier {}

#[derive(Debug, PartialEq, Eq)]
pub enum CoroMonoBarrierError {
    MissingProcessor,
    Occupied,
}

impl CoroMonoBarrier {
    pub fn new() -> CoroMonoBarrier {
        CoroMonoBarrier { lock: AtomicPtr::new(ptr::null_mut()) }
    }

    /// Try to wait for a signal().
    pub fn wait(&self) -> Result<(), CoroMonoBarrierError> {
        if let Some(p) = Processor::current() {
            let mut result = Ok(());
            {
                let result_ptr = &mut result;
                p.block_with(|p, mut coro| {
                    const EMPTY: *mut Coroutine = 0 as *mut Coroutine;
                    const READY: *mut Coroutine = 1 as *mut Coroutine;
                    const ORDER: Ordering = Ordering::SeqCst;

                    loop {
                        // Try to be optimistic and assume that the lock is READY
                        if self.lock.compare_and_swap(READY, EMPTY, ORDER) == READY {
                            // We stole READY and placed EMPTY ---> we're done
                            p.ready(coro);
                            return;
                        } else {
                            // The lock might be EMPTY instead ---> try to insert coro
                            let new = &mut *coro as *mut Coroutine;
                            let actual = self.lock.compare_and_swap(EMPTY, new, ORDER);

                            if actual == EMPTY {
                                // We replaced EMPTY with coro ---> signal() will ready() us
                                mem::forget(coro);
                                return;
                            } else if actual != READY {
                                // The lock is neither EMPTY nor READY
                                // ---> it must be occupied
                                // ---> abort
                                p.ready(coro);
                                *result_ptr = Err(CoroMonoBarrierError::Occupied);
                                return;
                            }
                        }
                    }
                });
            }
            result
        } else {
            Err(CoroMonoBarrierError::MissingProcessor)
        }
    }

    /// Try to notify a possibly waiting Coroutine. Otherwise mark the barrier as ready.
    pub fn notify(&self) {
        const EMPTY: *mut Coroutine = 0 as *mut Coroutine;
        const READY: *mut Coroutine = 1 as *mut Coroutine;

        // Try to be optimistic though and assume that lock is EMPTY and replace it with READY.
        // NOTE: This variable will never be READY due to the retry condition below.
        let mut current = EMPTY;

        loop {
            // The lock can either be EMPTY and thus should be marked READY,
            // or non-EMPTY which means that we're trying to extract a Coroutine
            // from the lock and thus should replace it with EMPTY.
            let new = if current > READY {
                EMPTY
            } else {
                READY
            };

            let actual = self.lock.compare_and_swap(current, new, Ordering::SeqCst);

            // Did we win?
            if actual == current {
                // We won! Is it a Coroutine?
                if actual > READY {
                    // It's a Coroutine! ---> make it ready()
                    let coro: Handle = unsafe { Handle::from_raw(actual) };
                    Scheduler::ready(coro);
                }

                break;
            } else {
                // It's not the value we assumed ---> retry with the actual value
                current = actual;
            }
        }
    }
}

impl Drop for CoroMonoBarrier {
    fn drop(&mut self) {
        const EMPTY: *mut Coroutine = 0 as *mut Coroutine;
        const READY: *mut Coroutine = 1 as *mut Coroutine;

        let lock = self.lock.swap(EMPTY, Ordering::SeqCst);
        if lock > READY {
            let coro = unsafe { Handle::from_raw(lock) };
            drop(coro);
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use scheduler::Scheduler;

    #[test]
    fn test_mono_barrier_thread_signal() {
        let barrier = Arc::new(MonoBarrier::new());
        let state = Arc::new(AtomicUsize::new(0));

        let h = {
            let barrier = barrier.clone();
            let state = state.clone();

            thread::spawn(move || {
                state.store(1, Ordering::SeqCst);
                barrier.signal();

                for _ in 0..100 {
                    if state.load(Ordering::SeqCst) == 2 {
                        return;
                    }

                    thread::sleep(Duration::from_millis(10));
                }

                panic!("timeout");
            })
        };

        println!("a");
        barrier.wait().unwrap();
        println!("b");
        assert_eq!(state.load(Ordering::SeqCst), 1);

        state.store(2, Ordering::SeqCst);
        h.join().unwrap();
    }

    #[test]
    fn test_mono_barrier_coroutine_signal() {
        Scheduler::new()
            .run(|| {
                let barrier = Arc::new(MonoBarrier::new());
                let state = Arc::new(AtomicUsize::new(0));

                let h = {
                    let barrier = barrier.clone();
                    let state = state.clone();

                    Scheduler::spawn(move || {
                        state.store(1, Ordering::SeqCst);
                        barrier.signal();
                    })
                };

                barrier.wait().unwrap();
                assert_eq!(state.load(Ordering::SeqCst), 1);

                h.join().unwrap();
            })
            .unwrap();
    }

    #[test]
    fn test_mono_barrier_thread_wait() {
        Scheduler::new()
            .run(|| {
                let barrier = Arc::new(MonoBarrier::new());
                let state = Arc::new(AtomicUsize::new(0));

                let h = {
                    let barrier = barrier.clone();
                    let state = state.clone();

                    thread::spawn(move || {
                        barrier.wait().unwrap();
                        assert_eq!(state.load(Ordering::SeqCst), 1);
                    })
                };

                state.store(1, Ordering::SeqCst);
                barrier.signal();

                h.join().unwrap();
            })
            .unwrap();
    }

    #[test]
    fn test_mono_barrier_coroutine_wait() {
        Scheduler::new()
            .run(|| {
                let barrier = Arc::new(MonoBarrier::new());
                let state = Arc::new(AtomicUsize::new(0));

                let h = {
                    let barrier = barrier.clone();
                    let state = state.clone();

                    Scheduler::spawn(move || {
                        barrier.wait().unwrap();
                        assert_eq!(state.load(Ordering::SeqCst), 1);
                    })
                };

                state.store(1, Ordering::SeqCst);
                barrier.signal();

                h.join().unwrap();
            })
            .unwrap();
    }

    #[test]
    fn test_coro_mono_barrier() {
        Scheduler::new()
            .run(|| {
                let barrier = Arc::new(CoroMonoBarrier::new());
                let state = Arc::new(AtomicUsize::new(0));

                barrier.notify();
                barrier.notify();
                barrier.notify();

                assert_eq!(barrier.wait(), Ok(()));

                {
                    let barrier = barrier.clone();
                    let state = state.clone();

                    Scheduler::spawn(move || {
                        state.store(1, Ordering::SeqCst);
                        barrier.notify();

                        assert_eq!(barrier.wait(), Ok(()));
                        state.store(2, Ordering::SeqCst);

                        assert_eq!(barrier.wait(), Ok(()));
                        state.store(3, Ordering::SeqCst);
                    });
                }

                assert_eq!(barrier.wait(), Ok(()));
                assert_eq!(state.load(Ordering::SeqCst), 1);

                barrier.notify();
                Scheduler::instance().unwrap().sleep_ms(10).unwrap();
                assert_eq!(state.load(Ordering::SeqCst), 2);

                barrier.notify();
                Scheduler::instance().unwrap().sleep_ms(10).unwrap();
                assert_eq!(state.load(Ordering::SeqCst), 3);
            })
            .unwrap();
    }
}
