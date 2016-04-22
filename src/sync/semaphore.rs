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

//! Semaphore for Coroutines

use coroutine::HandleList;
use scheduler::Scheduler;
use runtime::Processor;

use super::spinlock::Spinlock;

/// Semaphore
pub struct Semaphore(Spinlock<(usize, HandleList)>);

impl Semaphore {
    /// Create a semaphore by providing an initial count.
    pub fn new(count: usize) -> Semaphore {
        Semaphore(Spinlock::new((count, HandleList::new())))
    }

    /// Semaphore acquire (or down, P). If the counter is 0, block the current coroutine.
    pub fn acquire(&self) {
        let mut inner = self.0.lock();

        if inner.0 > 0 {
            inner.0 -= 1;
        } else {
            match Processor::current() {
                Some(p) => {
                    p.park_with(|_, coro| {
                        inner.1.push_back(coro);
                        drop(inner); // We _must_ to hold the lock until here
                    });
                }
                None => {
                    // Path for normal thread environment
                    // Fallback to a normal spin lock
                    panic!("Semaphore will not work in thread environment");
                }
            }
        }
    }

    /// Semaphore acquire (or down, P). Return immediately no matter success or not.
    pub fn try_acquire(&self) -> bool {
        let mut inner = match self.0.try_lock() {
            Some(inner) => inner,
            None => return false,
        };

        if inner.0 > 0 {
            inner.0 -= 1;
            true
        } else {
            false
        }
    }

    /// Semaphore release (or up, V).
    pub fn release(&self) {
        let mut inner = self.0.lock();

        if let Some(h) = inner.1.pop_front() {
            Scheduler::ready(h);
        } else {
            inner.0 += 1;
        }
    }
}

unsafe impl Send for Semaphore {}
unsafe impl Sync for Semaphore {}

#[cfg(test)]
mod test {
    use super::*;

    use std::sync::Arc;

    use scheduler::Scheduler;

    #[test]
    fn binary_semaphore() {
        Scheduler::new()
            .run(|| {
                let sema = Arc::new(Semaphore::new(1));

                let mut hlist = Vec::new();

                for id in 0..10 {
                    let sema = sema.clone();
                    let h = Scheduler::spawn(move || {
                        sema.acquire();
                        trace!("{} Acquired", id);
                        Scheduler::sched();
                        sema.release();
                        trace!("{} Released", id);
                    });
                    hlist.push(h);
                }

                for h in hlist {
                    h.join().unwrap();
                }
            })
            .unwrap();
    }

    #[test]
    fn basic_semaphore() {
        Scheduler::new()
            .run(|| {
                let sema = Arc::new(Semaphore::new(5));

                let mut hlist = Vec::new();

                for id in 0..10 {
                    let sema = sema.clone();
                    let h = Scheduler::spawn(move || {
                        sema.acquire();
                        trace!("{} Acquired", id);
                        Scheduler::sched();
                        sema.release();
                        trace!("{} Released", id);
                    });
                    hlist.push(h);
                }

                for h in hlist {
                    h.join().unwrap();
                }
            })
            .unwrap();
    }
}
