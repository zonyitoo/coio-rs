use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::ptr::Shared;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use coroutine::{Handle, HandleList};
use runtime::processor::Processor;
use runtime::timer::Timeout;
use scheduler::Scheduler;
use sync::Lock;
use sync::spinlock::Spinlock;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaiterState {
    Empty,
    Succeeded,
    Timeout,
    Error,
}

struct SharedWaiter {
    handle: Option<Handle>,
    state: WaiterState,
    timeout: Option<Timeout>,
}

// NOTE:
//   This Waiter will always be allocated on the stack. It can be referenced by:
//     1. Notifier's WaiterList
//     2. Timer's TimerWaitType::Waiter
//
//   So you have to ensure that all the raw pointer references
//   to this struct has been removed before you drop the struct.
pub struct Waiter {
    // The following fields are owned by Condvar and only accessed when it's lock is acquired:
    prev: Option<Shared<Waiter>>,
    next: Option<Shared<Waiter>>,

    // The following fields are shared between  Condvar, Scheduler and Waiter:
    shared: Spinlock<SharedWaiter>,
}

impl Waiter {
    pub fn new() -> Waiter {
        Waiter {
            prev: None,
            next: None,

            shared: Spinlock::new(SharedWaiter {
                handle: None,
                state: WaiterState::Empty,
                timeout: None,
            }),
        }
    }

    #[must_use]
    pub fn notify(&self, t: WaiterState) -> Option<Handle> {
        debug_assert!(t != WaiterState::Empty);

        let mut shared = self.shared.lock();

        match shared.handle.take() {
            None => None,
            Some(hdl) => {
                shared.state = t;
                Some(hdl)
            }
        }
    }

    pub fn state(&self) -> WaiterState {
        self.shared.lock().state
    }

    pub fn try_wait(&self, coro: Handle) -> Option<Handle> {
        let mut shared = self.shared.lock();

        match shared.state {
            WaiterState::Empty => {
                shared.handle = Some(coro);
                None
            }
            _ => Some(coro),
        }
    }

    pub fn set_timeout(&mut self, timeout: Timeout) {
        self.shared.lock().timeout = Some(timeout)
    }

    pub fn take_timeout(&mut self) -> Option<Timeout> {
        self.shared.lock().timeout.take()
    }
}

struct WaiterList {
    head: Option<Shared<Waiter>>,
    tail: Option<Shared<Waiter>>,
}

impl WaiterList {
    fn push_back(&mut self, waiter: &mut Waiter) {
        match self.tail {
            None => {
                // Since head is None the list must be empty => set head and tail to hdl
                let node = Some(unsafe { Shared::new(waiter) });
                self.head = node;
                self.tail = node;
            }
            Some(tail) => {
                let node = Some(unsafe { Shared::new(waiter) });
                let tail_ref = unsafe { &mut **tail };

                self.tail = node;
                tail_ref.next = node;
                waiter.prev = Some(tail);
            }
        }
    }

    fn pop_front(&mut self) -> Option<Shared<Waiter>> {
        match self.head.take() {
            None => None,
            Some(head) => {
                match unsafe { &mut **head }.next.take() {
                    None => self.tail = None,
                    Some(next) => {
                        unsafe { &mut **next }.prev = None;
                        self.head = Some(next);
                    }
                }

                Some(head)
            }
        }
    }

    fn remove(&mut self, waiter: &mut Waiter) {
        let prev = waiter.prev.take();
        let next = waiter.next.take();

        if let Some(prev) = prev {
            let prev = unsafe { &mut **prev };
            prev.next = next;
        } else {
            self.head = next;
        }

        if let Some(next) = next {
            let next = unsafe { &mut **next };
            next.prev = prev;
        } else {
            self.tail = prev;
        }
    }
}

impl Default for WaiterList {
    fn default() -> WaiterList {
        WaiterList {
            head: None,
            tail: None,
        }
    }
}

/// A Condition variable
pub struct Condvar<'a, L: 'a + Lock<'a>> {
    waiter_list: UnsafeCell<WaiterList>,
    lock: Option<&'a L>,
    _guard: PhantomData<L::Guard>,
}

impl<'a, L: 'a + Lock<'a>> Condvar<'a, L> {
    pub fn new(lock: &'a L) -> Condvar<'a, L> {
        Condvar {
            waiter_list: Default::default(),
            lock: Some(lock),
            _guard: PhantomData,
        }
    }

    pub fn new_unbound() -> Condvar<'a, L> {
        Condvar {
            waiter_list: Default::default(),
            lock: None,
            _guard: PhantomData,
        }
    }

    pub fn set_lock(&mut self, lock: &'a L) {
        assert!(self.lock.is_none(),
                "Cannot reinitialize Condvar with a different Lock");
        self.lock = Some(lock);
    }

    pub fn wait(&self, guard: L::Guard) -> L::Guard {
        let lock = self.get_lock();
        let p = Processor::current_required();
        let mut waiter = Waiter::new();

        self.get_waiter_list().push_back(&mut waiter);
        drop(guard);

        p.park_with(|p, coro| {
            if let Some(coro) = waiter.try_wait(coro) {
                p.ready(coro);
            }
        });

        lock.lock()
    }

    pub fn wait_timeout(&self,
                        guard: L::Guard,
                        dur: Duration)
                        -> Result<L::Guard, WaitTimeoutResult> {
        let lock = self.get_lock();
        let p = Processor::current_required();
        let mut waiter = Waiter::new();

        self.get_waiter_list().push_back(&mut waiter);
        drop(guard);

        let timeout = p.scheduler().timeout(::duration_to_ms(dur), &mut waiter);
        waiter.set_timeout(timeout);

        p.park_with(|p, coro| {
            if let Some(coro) = waiter.try_wait(coro) {
                p.ready(coro);
            }
        });

        let p = Processor::current_required();

        match waiter.state() {
            WaiterState::Empty => panic!("WaiterState is Empty"),
            WaiterState::Succeeded => {
                if let Some(timeout) = waiter.take_timeout() {
                    p.scheduler().cancel_timeout(timeout);
                }

                Ok(lock.lock())
            }
            WaiterState::Timeout => Err(WaitTimeoutResult(true)),
            WaiterState::Error => unimplemented!(),
        }
    }

    pub fn notify_one(&self) {
        let lock = self.get_lock();
        let waiter = {
            let guard = lock.lock();
            self.get_waiter_list().pop_front()
        };

        if let Some(waiter) = waiter {
            let waiter = unsafe { &mut **waiter };

            if let Some(hdl) = waiter.notify(WaiterState::Succeeded) {
                // TODO: Optimize for batches
                Scheduler::ready(hdl);
            }
        }
    }

    pub fn notify_all(&self) {
        let lock = self.get_lock();
        let lst: WaiterList = unsafe {
            let guard = lock.lock();
            mem::replace(self.get_waiter_list(), Default::default())
        };

        while let Some(waiter) = lst.pop_front() {
            let waiter = unsafe { &mut **waiter };

            if let Some(hdl) = waiter.notify(WaiterState::Succeeded) {
                // TODO: Optimize for batches
                Scheduler::ready(hdl);
            }
        }
    }

    fn get_lock(&self) -> &'a L {
        self.lock.expect("requires a lock")
    }

    fn get_waiter_list(&self) -> &mut WaiterList {
        unsafe { &mut *self.waiter_list.get() }
    }
}

impl<'a, L: 'a + Lock<'a>> Drop for Condvar<'a, L> {
    fn drop(&mut self) {
        let guard = self.lock.map(|lock| lock.lock());
        let waiter_list = self.get_waiter_list();
        let processor = Processor::current();

        while let Some(waiter) = waiter_list.pop_front() {
            let waiter = unsafe { &mut **waiter };

            if let Some(timeout) = waiter.take_timeout() {
                if let Some(p) = processor {
                    p.scheduler().cancel_timeout(timeout);
                }
            }
        }
    }
}

impl<'a, L: 'a + Lock<'a>> fmt::Debug for Condvar<'a, L> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Condvar {{ .. }}")
    }
}

pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    pub fn timed_out(&self) -> bool {
        self.0
    }
}


#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::time::Duration;
    use std::collections::VecDeque;

    use scheduler::Scheduler;

    use super::*;

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

    #[test]
    fn condvar_protected_queue() {
        let pair = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        Scheduler::new()
            .with_workers(10)
            .run(move || {

                let cloned_pair = pair.clone();
                let producer = Scheduler::spawn(move || {
                    for i in 0..10 {
                        let mut queue = cloned_pair.0.lock().unwrap();
                        queue.push_back(i);
                        cloned_pair.1.notify_one();
                        Scheduler::sched();
                    }
                });

                let mut cons = Vec::with_capacity(10);

                for _ in 0..10 {
                    let pair = pair.clone();
                    let consumer = Scheduler::spawn(move || {
                        let mut queue = pair.0.lock().unwrap();
                        while queue.is_empty() {
                            queue = pair.1.wait(queue).unwrap();
                        }

                        queue.pop_front().unwrap()
                    });
                    cons.push(consumer);
                }

                let mut sum = 0;

                let _ = producer.join();
                for h in cons {
                    sum += h.join().unwrap();
                }

                assert_eq!(sum, 45);
            })
            .unwrap();
    }
}
