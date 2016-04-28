// The MIT License (MIT)

// Copyright (c) 2015 Y. T. Chung <zonyitoo@gmail.com>

// Permission is hereby granted, free of charge, to any person obtaining a copy of
// this software and associated documentation files (the "Software"), to deal in
// the Software without restriction, including without limitation the rights to
// use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software is furnished to do so,
// subject to the following conditions:

// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.

// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
// FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
// COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
// IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
// CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use std::ptr::Shared;
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::fmt;

use sync::spinlock::Spinlock;
use coroutine::{Handle, HandleList};
use runtime::processor::Processor;
use runtime::timer::{Timeout, TimerError};

#[repr(u8)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum WaiterState {
    Empty,
    Succeeded,
    Timedout,
    Error,
}

impl Default for WaiterState {
    fn default() -> WaiterState {
        WaiterState::Empty
    }
}

// NOTE: This Waiter will always be allocated on the stack. It can be referenced by:
//
//  1. Notifier's WaiterList
//  2. Timer's TimerWaitType::Waiter
//
//  So you have to ensure that all the raw pointer references to this struct has been removed before
//  you drop the struct.
pub struct Waiter {
    // Not Sync part
    prev: Option<Shared<Waiter>>,
    next: Option<Shared<Waiter>>,
    notifier: Option<*const Notifier>,
    timeout: Option<Timeout>,

    // Sync part
    // These fields can be called from multiple threads
    state: Spinlock<(WaiterState, Option<Handle>)>,
}

impl Waiter {
    pub fn new() -> Waiter {
        Waiter {
            prev: None,
            next: None,
            notifier: None,
            timeout: None,

            state: Spinlock::default(),
        }
    }

    pub fn bind(&mut self, notifier: &Notifier) {
        debug_assert!(self.notifier.is_none(), "Waiter is already binded");
        let mut lst = notifier.wait_list.lock();
        lst.push(self);
        self.notifier = Some(notifier as *const _);
    }

    #[must_use]
    pub fn notify(&self, t: WaiterState) -> Option<Handle> {
        debug_assert!(t != WaiterState::Empty);
        let mut state = self.state.lock();
        match state.1.take() {
            None => None,
            Some(hdl) => {
                state.0 = t;
                Some(hdl)
            }
        }
    }

    pub fn state(&self) -> WaiterState {
        self.state.lock().0
    }

    pub fn try_wait(&self, coro: Handle) -> Option<Handle> {
        let mut state = self.state.lock();
        match state.0 {
            WaiterState::Empty => {
                state.1 = Some(coro);
                None
            }
            _ => Some(coro),
        }
    }

    pub fn set_timeout(&mut self, timeout: Timeout) {
        self.timeout = Some(timeout)
    }

    pub fn take_timeout(&mut self) -> Option<Timeout> {
        self.timeout.take()
    }

    unsafe fn unbind(&mut self) {
        match self.notifier {
            None => {}
            Some(noti) => {
                // Force remove
                let noti = &*noti;
                let mut lst = noti.wait_list.lock();
                lst.remove(self);
            }
        }
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        unsafe { self.unbind(); }

        if let Some(timeout) = self.timeout.take() {
            let p = Processor::current().expect("Notifier is dropping outside of a Processor");
            p.scheduler().cancel_timeout(timeout);
        }
    }
}

struct WaiterList {
    head: Option<Shared<Waiter>>,
}

impl WaiterList {
    fn push(&mut self, waiter: &mut Waiter) {
        match self.head {
            None => self.head = Some(unsafe { Shared::new(waiter) }),
            Some(head) => unsafe {
                let head = &mut **head;
                head.prev = Some(Shared::new(waiter));
                waiter.next = Some(Shared::new(waiter));
                mem::swap(&mut self.head, &mut waiter.next);
            },
        }
    }

    fn pop(&mut self) -> Option<Shared<Waiter>> {
        match self.head {
            None => None,
            Some(head) => unsafe {
                let head_ref = &mut **head;
                match head_ref.next.take() {
                    None => self.head = None,
                    Some(next) => {
                        let next_ref = &mut **next;
                        next_ref.prev = None;
                        self.head = Some(next);
                    }
                }

                Some(head)
            },
        }
    }

    fn remove(&mut self, waiter: &mut Waiter) {
        if let Some(prev) = waiter.prev {
            let prev = unsafe { &mut **prev };
            prev.next = waiter.next;
        }

        if let Some(next) = waiter.next {
            let next = unsafe { &mut **next };
            next.prev = waiter.prev;
        }

        if let Some(head) = self.head {
            if *head == waiter as *mut _ {
                self.head = waiter.next;
            }
        }

        waiter.prev = None;
        waiter.next = None;
    }
}

impl Default for WaiterList {
    fn default() -> WaiterList {
        WaiterList { head: None }
    }
}

impl Drop for WaiterList {
    fn drop(&mut self) {
        while let Some(_) = self.pop() {}
    }
}

pub struct Notifier {
    token: AtomicUsize,
    notified: AtomicUsize,
    wait_list: Spinlock<WaiterList>,
}

impl Notifier {
    pub fn notify_one(&self, t: WaiterState) -> Option<Handle> {
        let mut hdl = None;
        if let Some(waiter) = self.wait_list.lock().pop() {
            let waiter = unsafe { &mut **waiter };

            // NOTE: Must unbind the notifier here!
            waiter.notifier = None;
            hdl = waiter.notify(t);
        }

        self.notified.fetch_add(1, Ordering::SeqCst);
        hdl
    }

    pub fn notify_all(&self, t: WaiterState, hdl_list: &mut HandleList) {
        let mut count = 1usize;
        let mut lst = self.wait_list.lock();
        while let Some(waiter) = lst.pop() {
            let waiter = unsafe { &mut **waiter };

            // NOTE: Must unbind the notifier here!
            waiter.notifier = None;

            // FIXME: Deadlock! WHY?
            if let Some(hdl) = waiter.notify(t) {
                hdl_list.push_back(hdl);
            }

            count += 1;
        }

        self.notified.fetch_add(count, Ordering::SeqCst);
    }

    pub fn wait(&self) -> WaiterState {
        let p = Processor::current().expect("Notifier::wait should be run in a Coroutine");

        // Short path
        let token = self.token.fetch_add(1, Ordering::SeqCst);
        let notified = self.notified.load(Ordering::SeqCst);
        if token < notified {
            trace!("Already notified for token {}, no need to wait", token);
            return WaiterState::Succeeded;
        }

        let mut waiter = Waiter::new();
        waiter.bind(self);
        p.park_with(|p, coro| {
            if let Some(coro) = waiter.try_wait(coro) {
                p.ready(coro);
            }
        });
        waiter.state()
    }

    pub fn wait_timeout(&self, dur: Duration) -> Result<WaiterState, TimerError> {
        let p = Processor::current().expect("Notifier::wait should be run in a Coroutine");

        let mut waiter = Waiter::new();

        let wait_ms = ::duration_to_ms(dur);
        let timeout = try!(p.scheduler().timeout(wait_ms, &mut waiter));
        waiter.set_timeout(timeout);

        waiter.bind(self);
        p.park_with(|p, coro| {
            if let Some(coro) = waiter.try_wait(coro) {
                p.ready(coro);
            }
        });

        // NOTE: Wake up!
        let p = Processor::current().expect("Notifier::wait is waken up without a Processor");

        let state = waiter.state();
        match state {
            WaiterState::Error | WaiterState::Succeeded => {
                p.scheduler().cancel_timeout(waiter.take_timeout().unwrap());
            }
            WaiterState::Timedout => {}
            _ => panic!("Waiter is waken up with invalid state"),
        }
        Ok(state)
    }
}

impl Default for Notifier {
    fn default() -> Notifier {
        Notifier {
            wait_list: Spinlock::default(),
            token: AtomicUsize::new(0),
            notified: AtomicUsize::new(0),
        }
    }
}

impl fmt::Debug for Notifier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Notifier {{ .. }}")
    }
}

unsafe impl Send for Notifier {}
unsafe impl Sync for Notifier {}

impl Drop for Notifier {
    fn drop(&mut self) {
        let mut lst = self.wait_list.lock();
        while let Some(_) = lst.pop() {}
    }
}
