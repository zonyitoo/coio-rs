use std::fmt;
use std::mem;
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
            match *guard {
                State::Ready => {
                    *guard = State::Empty;
                    return Ok(());
                }
                State::Empty => {
                    match Processor::current() {
                        Some(p) => {
                            p.park_with(move |_, coro| {
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

    /// Try to notify the waiting executor
    pub fn notify(&self) {
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

impl fmt::Debug for MonoBarrier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let guard = self.lock.lock().unwrap();

        match *guard {
            State::Empty => write!(f, "MonoBarrier(Empty)"),
            State::Ready => write!(f, "MonoBarrier(Ready)"),
            State::Thread => write!(f, "MonoBarrier(Thread)"),
            State::Coroutine(ref coro) => write!(f, "MonoBarrier({:?})", coro),
        }
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

const CORO_EMPTY: *mut Coroutine = 0 as *mut Coroutine;
const CORO_READY: *mut Coroutine = 1 as *mut Coroutine;

impl CoroMonoBarrier {
    #[inline]
    pub fn new() -> CoroMonoBarrier {
        CoroMonoBarrier { lock: AtomicPtr::new(CORO_EMPTY) }
    }

    /// Try to wait for a notify().
    pub fn wait(&self) -> Result<(), CoroMonoBarrierError> {
        // wait() is the one potentially parking a Coroutine.
        // ---> Use Release ordering to flush possible previous writes to memory
        //      in case the Coroutine gets resumed on another thread in notify().
        const ORDERING: Ordering = Ordering::Release;

        if let Some(p) = Processor::current() {
            let mut result = Ok(());

            // Try to be optimistic and assume that the lock is CORO_READY
            let actual = self.lock.compare_and_swap(CORO_READY, CORO_EMPTY, ORDERING);

            if actual == CORO_EMPTY {
                p.park_with(|p, mut coro| {
                    loop {
                        // Try to be optimistic and assume that the lock is CORO_EMPTY.
                        let new = &mut *coro as *mut Coroutine;
                        let actual = self.lock.compare_and_swap(CORO_EMPTY, new, ORDERING);

                        if actual == CORO_EMPTY {
                            // We replaced CORO_EMPTY with coro ---> notify() will CORO_READY us.
                            mem::forget(coro);
                            break;
                        }

                        // Alternatively assume that the lock is CORO_READY.
                        let actual = self.lock.compare_and_swap(CORO_READY, CORO_EMPTY, ORDERING);

                        // If the lock is CORO_EMPTY try again above, otherwise...
                        if actual != CORO_EMPTY {
                            // If the lock is neither CORO_EMPTY nor CORO_READY,
                            // it has an invalid state ---> Error.
                            if actual != CORO_READY {
                                result = Err(CoroMonoBarrierError::Occupied);
                            }

                            // ---> In case of CORO_READY as well as an Error we are ready().
                            p.ready(coro);
                            break;
                        }
                    }
                });
            } else if actual != CORO_READY {
                result = Err(CoroMonoBarrierError::Occupied);
            }

            result
        } else {
            Err(CoroMonoBarrierError::MissingProcessor)
        }
    }

    /// Try to notify a possibly waiting Coroutine. Otherwise mark the barrier as ready.
    pub fn notify(&self) {
        // notify() is the one potentially waking up a parked Coroutine.
        // ---> Use Acquire ordering to sync with possible writes to memory from before wait().
        const ORDERING: Ordering = Ordering::Acquire;

        // Try to be optimistic though and assume that lock
        // is CORO_EMPTY and replace it with CORO_READY.
        // NOTE: This variable will never be CORO_READY due to the retry condition below.
        let mut current = CORO_EMPTY;

        loop {
            // The lock can either be CORO_EMPTY and thus should be marked CORO_READY,
            // or non-CORO_EMPTY which means that we're trying to extract a Coroutine
            // from the lock and thus should replace it with CORO_EMPTY.
            let new = if current > CORO_READY {
                CORO_EMPTY
            } else {
                CORO_READY
            };

            let actual = self.lock.compare_and_swap(current, new, ORDERING);

            // Did we win?
            if actual == current {
                // We won! Is it a Coroutine?
                if actual > CORO_READY {
                    // It's a Coroutine! ---> make it ready()
                    Scheduler::ready(unsafe { Handle::from_raw(actual) });
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
        // If drop() is called the current thread must be the last one holding onto this instance.
        // Thus we can simply swap out the `lock` value.
        let lock = self.lock.swap(CORO_EMPTY, Ordering::AcqRel);

        if lock > CORO_READY {
            let _ = unsafe { Handle::from_raw(lock) };
        }
    }
}

impl fmt::Debug for CoroMonoBarrier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let lock = self.lock.load(Ordering::Relaxed);

        if lock == CORO_EMPTY {
            write!(f, "CoroMonoBarrier(Empty)")
        } else if lock == CORO_EMPTY {
            write!(f, "CoroMonoBarrier(Ready)")
        } else {
            // It is unsafe to access a Coroutine for a debug description if we do not own it.
            write!(f, "CoroMonoBarrier({:p})", lock)
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
    fn test_mono_barrier_thread_notify() {
        let barrier = Arc::new(MonoBarrier::new());
        let state = Arc::new(AtomicUsize::new(0));

        let h = {
            let barrier = barrier.clone();
            let state = state.clone();

            thread::spawn(move || {
                state.store(1, Ordering::SeqCst);
                barrier.notify();

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
    fn test_mono_barrier_coroutine_notify() {
        Scheduler::new()
            .run(|| {
                let barrier = Arc::new(MonoBarrier::new());
                let state = Arc::new(AtomicUsize::new(0));

                let h = {
                    let barrier = barrier.clone();
                    let state = state.clone();

                    Scheduler::spawn(move || {
                        state.store(1, Ordering::SeqCst);
                        barrier.notify();
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
                barrier.notify();

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
                barrier.notify();

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
                Scheduler::instance().unwrap().sleep_ms(10);
                assert_eq!(state.load(Ordering::SeqCst), 2);

                barrier.notify();
                Scheduler::instance().unwrap().sleep_ms(10);
                assert_eq!(state.load(Ordering::SeqCst), 3);
            })
            .unwrap();
    }
}
