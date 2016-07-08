// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Coroutine synchronization

pub mod condvar;
pub mod mono_barrier;
pub mod mpsc;
pub mod mutex;
pub mod semaphore;
pub mod spinlock;

pub use self::condvar::Condvar;
pub use self::spinlock::{Spinlock, TicketSpinlock};
pub use self::mutex::Mutex;

use std::sync;

pub trait Lock<'a> {
    type Guard;
    fn lock(&'a self) -> Self::Guard;
}

impl<'a, T: 'a + ?Sized> Lock<'a> for sync::Mutex<T> {
    type Guard = sync::MutexGuard<'a, T>;

    fn lock(&'a self) -> Self::Guard {
        self.lock().unwrap()
    }
}
