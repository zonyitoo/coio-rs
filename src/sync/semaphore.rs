// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

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
