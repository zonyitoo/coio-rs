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

//! Coroutine scheduling with asynchronous I/O support

#![feature(
    arc_counts,
    fnbox,
    panic_handler,
    panic_propagate,
    recover,
    reflect_marker,
    std_panic,
)]

#[macro_use]
extern crate log;

extern crate context;
extern crate deque;
extern crate mio;
extern crate rand;
extern crate slab;
extern crate linked_hash_map;

pub mod join_handle;
pub mod net;
pub mod options;
pub mod promise;
pub mod scheduler;
pub mod sync;

pub use options::Options;
pub use promise::Promise;
pub use scheduler::{Scheduler, JoinHandle};

mod coroutine;
mod runtime;

use std::panic;
use std::thread;
use std::time::Duration;
use std::sync::atomic::{AtomicUsize, ATOMIC_USIZE_INIT, Ordering};

/// Spawn a new Coroutine
#[inline]
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
    where F: FnOnce() -> T + Send + 'static,
          T: Send + 'static
{
    Scheduler::spawn(f)
}

/// Spawn a new Coroutine with options
#[inline]
pub fn spawn_opts<F, T>(f: F, opts: Options) -> JoinHandle<T>
    where F: FnOnce() -> T + Send + 'static,
          T: Send + 'static
{
    Scheduler::spawn_opts(f, opts)
}

/// Give up the CPU
#[inline]
pub fn sched() {
    Scheduler::sched()
}

/// Put the current coroutine to sleep for the specific amount of time
#[inline]
pub fn sleep_ms(ms: u64) {
    sleep(Duration::from_millis(ms))
}

/// Put the current coroutine to sleep for the specific amount of time
#[inline]
pub fn sleep(dur: Duration) {
    match Scheduler::instance() {
        Some(s) => s.sleep(dur).unwrap(),
        None => thread::sleep(dur),
    }
}

/// Coroutine configuration. Provides detailed control over
/// the properties and behavior of new coroutines.
pub struct Builder {
    opts: Options,
}

impl Builder {
    /// Generates the base configuration for spawning a coroutine,
    // from which configuration methods can be chained.
    #[inline]
    pub fn new() -> Builder {
        Builder { opts: Options::new() }
    }

    /// Sets the size of the stack for the new coroutine.
    #[inline]
    pub fn stack_size(mut self, stack_size: usize) -> Builder {
        self.opts.stack_size = stack_size;
        self
    }

    /// Names the coroutine-to-be. Currently the name
    // is used for identification only in panic messages.
    #[inline]
    pub fn name(mut self, name: String) -> Builder {
        self.opts.name = Some(name);
        self
    }

    /// Spawn a new coroutine
    #[inline]
    pub fn spawn<F, T>(self, f: F) -> JoinHandle<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        Scheduler::spawn_opts(f, self.opts)
    }
}


#[cfg(debug_assertions)]
static GLOBAL_WORK_COUNT: AtomicUsize = ATOMIC_USIZE_INIT;

#[inline]
#[cfg(debug_assertions)]
fn global_work_count_add() {
    GLOBAL_WORK_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[inline]
#[cfg(debug_assertions)]
fn global_work_count_sub() {
    GLOBAL_WORK_COUNT.fetch_sub(1, Ordering::Relaxed);
}

#[inline]
#[cfg(debug_assertions)]
fn global_work_count_get() -> usize {
    GLOBAL_WORK_COUNT.load(Ordering::Relaxed)
}

#[inline]
#[cfg(not(debug_assertions))]
fn global_work_count_add() {}

#[inline]
#[cfg(not(debug_assertions))]
fn global_work_count_sub() {}

#[inline]
#[cfg(not(debug_assertions))]
fn global_work_count_get() -> usize {
    0
}

unsafe fn try<R, F: FnOnce() -> R>(f: F) -> thread::Result<R> {
    let mut f = Some(f);
    let f = &mut f as *mut Option<F> as usize;

    panic::recover(move || (*(f as *mut Option<F>)).take().unwrap()())
}


#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_sleep_ms() {
        Scheduler::new()
            .run(|| {
                sleep_ms(1000);
            })
            .unwrap();
    }
}
