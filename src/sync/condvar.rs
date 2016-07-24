use std::fmt;
use std::mem;
use std::ptr::Shared;
use std::time::Duration;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::cmp;

use coroutine::{Handle, HandleList};
use runtime::processor::Processor;
use runtime::timer::Timeout;
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

        if shared.state == WaiterState::Empty {
            shared.state = t;
        }

        shared.handle.take()
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
pub struct Condvar {
    lock: Spinlock<WaiterList>,
    token: AtomicUsize,
    notified: AtomicUsize,
}

impl Condvar {
    pub fn new() -> Condvar {
        Condvar {
            lock: Spinlock::new(Default::default()),
            token: AtomicUsize::new(0),
            notified: AtomicUsize::new(0),
        }
    }

    fn alloc_token(&self) -> usize {
        self.token.fetch_add(1, Ordering::SeqCst)
    }

    fn check_token(&self, token: usize) -> bool {
        token < self.notified.load(Ordering::SeqCst)
    }

    fn notify_token(&self, count: usize) {
        self.notified.fetch_add(count, Ordering::SeqCst);
    }

    pub fn wait(&self) {
        let token = self.alloc_token();
        if self.check_token(token) {
            return;
        }

        let mut waiter = Waiter::new();

        {
            let waiter = &mut waiter;
            let p = Processor::current_required();
            p.park_with(move |p, coro| {
                let mut guard = self.lock.lock();
                if self.check_token(token) {
                    trace!("wait: Wake up {:?} after token check", coro);
                    p.ready(coro);
                    return;
                }

                guard.push_back(waiter);
                if let Some(coro) = waiter.try_wait(coro) {
                    trace!("wait: Wake up {:?} immediately", coro);
                    p.ready(coro);
                }
            });
        }

    }

    pub fn wait_timeout(&self, dur: Duration) -> Result<(), WaitTimeoutResult> {
        let token = self.alloc_token();
        if self.check_token(token) {
            return Ok(());
        }

        let mut waiter = Waiter::new();

        {
            let waiter = &mut waiter;
            let p = Processor::current_required();
            p.park_with(move |p, coro| {
                let mut guard = self.lock.lock();
                if self.check_token(token) {
                    trace!("wait_timeout: Waken up {:?} after token check", coro);
                    p.ready(coro);
                    return;
                }

                guard.push_back(waiter);
                let timeout = p.scheduler().timeout(::duration_to_ms(dur), waiter);
                trace!("wait_timeout: {}ms", ::duration_to_ms(dur));
                waiter.set_timeout(timeout);
                if let Some(coro) = waiter.try_wait(coro) {
                    trace!("wait_timeout: Waken up {:?} immediately", coro);
                    p.ready(coro);
                }
            });
        }

        self.lock.lock().remove(&mut waiter);

        trace!("wait_timeout: resuming");

        let p = Processor::current_required();

        match waiter.state() {
            WaiterState::Empty => panic!("WaiterState is Empty"),
            WaiterState::Succeeded => {
                if let Some(timeout) = waiter.take_timeout() {
                    trace!("wait_timeout: Wake up succeeded, cancelling timeout");
                    p.scheduler().cancel_timeout(timeout);
                }

                Ok(())
            }
            WaiterState::Timeout => Err(WaitTimeoutResult(true)),
            WaiterState::Error => unimplemented!(),
        }
    }

    pub fn notify_one(&self, hdl_list: &mut HandleList) {
        let waiter = {
            let mut guard = self.lock.lock();
            guard.pop_front()
        };

        if let Some(waiter) = waiter {
            let waiter = unsafe { &mut **waiter };

            if let Some(hdl) = waiter.notify(WaiterState::Succeeded) {
                hdl_list.push_back(hdl);
            }
        }

        self.notify_token(1);
    }

    pub fn notify_all(&self, hdl_list: &mut HandleList) {
        let mut lst: WaiterList = {
            let mut guard = self.lock.lock();
            mem::replace(&mut *guard, Default::default())
        };

        let mut count = 0;
        while let Some(waiter) = lst.pop_front() {
            let waiter = unsafe { &mut **waiter };

            if let Some(hdl) = waiter.notify(WaiterState::Succeeded) {
                hdl_list.push_back(hdl);
            }

            count += 1;
        }

        self.notify_token(cmp::max(count, 1));
    }
}

impl Drop for Condvar {
    fn drop(&mut self) {
        let mut guard = self.lock.lock();
        let processor = Processor::current();

        while let Some(waiter) = guard.pop_front() {
            let waiter = unsafe { &mut **waiter };

            if let Some(timeout) = waiter.take_timeout() {
                if let Some(ref p) = processor {
                    p.scheduler().cancel_timeout(timeout);
                }
            }
        }
    }
}

impl fmt::Debug for Condvar {
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


// #[cfg(test)]
// mod test {
//     use std::sync::Arc;
//     use std::time::Duration;
//     use std::collections::VecDeque;
//
//     use scheduler::Scheduler;
//
//     use super::*;
//     use sync::mutex::Mutex;
//
//     #[test]
//     fn condvar_basic() {
//         let pair = Arc::new((Mutex::new(false), Condvar::new()));
//         Scheduler::new()
//             .with_workers(1)
//             .run(move || {
//                 let cloned = pair.clone();
//                 Scheduler::spawn(move || {
//                     let mut guard = cloned.0.lock().unwrap();
//                     *guard = true;
//                     cloned.1.notify_one();
//                 });
//
//                 let mut guard = pair.0.lock().unwrap();
//                 while !*guard {
//                     guard = pair.1.wait(guard).unwrap();
//                 }
//             })
//             .unwrap();
//     }
//
//     #[test]
//     fn condvar_timeout() {
//         let pair = Arc::new((Mutex::new(false), Condvar::new()));
//         Scheduler::new()
//             .with_workers(1)
//             .run(move || {
//                 let guard = pair.0.lock().unwrap();
//                 let (_, t) = pair.1.wait_timeout(guard, Duration::from_millis(1)).unwrap();
//                 assert!(t.timed_out());
//             })
//             .unwrap();
//     }
//
//     #[test]
//     fn condvar_protected_queue() {
//         let pair = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
//         Scheduler::new()
//             .with_workers(10)
//             .run(move || {
//
//                 let cloned_pair = pair.clone();
//                 let producer = Scheduler::spawn(move || {
//                     for i in 0..10 {
//                         let mut queue = cloned_pair.0.lock().unwrap();
//                         queue.push_back(i);
//                         cloned_pair.1.notify_one();
//                         Scheduler::sched();
//                     }
//                 });
//
//                 let mut cons = Vec::with_capacity(10);
//
//                 for _ in 0..10 {
//                     let pair = pair.clone();
//                     let consumer = Scheduler::spawn(move || {
//                         let mut queue = pair.0.lock().unwrap();
//                         while queue.is_empty() {
//                             queue = pair.1.wait(queue).unwrap();
//                         }
//
//                         queue.pop_front().unwrap()
//                     });
//                     cons.push(consumer);
//                 }
//
//                 let mut sum = 0;
//
//                 let _ = producer.join();
//                 for h in cons {
//                     sum += h.join().unwrap();
//                 }
//
//                 assert_eq!(sum, 45);
//             })
//             .unwrap();
//     }
// }
