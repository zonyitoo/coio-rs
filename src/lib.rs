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
extern crate mio;
extern crate deque;
extern crate rand;

use std::thread;
use std::panic;
use std::time::Duration;
use std::any::Any;

pub use scheduler::JoinHandle;
pub use options::Options;
pub use promise::Promise;

use runtime::Processor;
use coroutine::{Handle, State};

pub mod net;
pub mod sync;
pub mod scheduler;
pub mod options;
pub mod promise;
pub mod join_handle;
mod runtime;
mod coroutine;

/// Spawn a new Coroutine
#[inline(always)]
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
    where F: FnOnce() -> T + Send + 'static,
          T: Send + 'static
{
    Scheduler::spawn(f)
}

/// Spawn a new Coroutine with options
#[inline(always)]
pub fn spawn_opts<F, T>(f: F, opts: Options) -> JoinHandle<T>
    where F: FnOnce() -> T + Send + 'static,
          T: Send + 'static
{
    Scheduler::spawn_opts(f, opts)
}

/// Giveup the CPU
#[inline(always)]
pub fn sched() {
    Scheduler::sched()
}

/// Put the current coroutine to sleep for the specific amount of time
#[inline]
pub fn sleep_ms(ms: u64) {
    Scheduler::sleep(Duration::from_millis(ms))
}

/// Put the current coroutine to sleep for the specific amount of time
#[inline]
pub fn sleep(duration: Duration) {
    Scheduler::sleep(duration)
}

/// Coroutine configuration. Provides detailed control over the properties and behavior of new coroutines.
pub struct Builder {
    opts: Options
}

impl Builder {
    /// Generates the base configuration for spawning a coroutine, from which configuration methods can be chained.
    pub fn new() -> Builder {
        Builder {
            opts: Options::new()
        }
    }

    /// Sets the size of the stack for the new coroutine.
    #[inline]
    pub fn stack_size(mut self, stack_size: usize) -> Builder {
        self.opts.stack_size = stack_size;
        self
    }

    /// Names the coroutine-to-be. Currently the name is used for identification only in panic messages.
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

/// Public API for a Coroutine
pub struct Coroutine(Handle);

impl Coroutine {
    #[inline(always)]
    pub fn set_name(&mut self, name: String) {
        self.0.set_name(name);
    }

    #[inline(always)]
    pub fn name(&self) -> Option<&str> {
        self.0.name()
    }
}

/// Public API for a Coroutine reference
pub struct CoroutineRef<'a>(&'a mut Handle);

impl<'a> CoroutineRef<'a> {
    #[inline(always)]
    pub fn set_name(&mut self, name: String) {
        self.0.set_name(name);
    }

    #[inline(always)]
    pub fn name(&self) -> Option<&str> {
        self.0.name()
    }

    /// Yield the current Coroutine
    #[inline]
    pub fn sched(&mut self) {
        // The same as Processor::sched
        self.0.yield_with(State::Suspended, 0);
    }
}

/// Coroutine Scheduler
pub struct Scheduler(scheduler::Scheduler);

impl Scheduler {
    /// Create a new Scheduler with default configuration
    pub fn new() -> Scheduler {
        Scheduler(scheduler::Scheduler::new())
    }

    /// Set the number of workers
    #[inline(always)]
    pub fn with_workers(self, workers: usize) -> Scheduler {
        Scheduler(self.0.with_workers(workers))
    }

    /// Set the default stack size
    #[inline(always)]
    pub fn default_stack_size(self, default_stack_size: usize) -> Scheduler {
        Scheduler(self.0.default_stack_size(default_stack_size))
    }

    /// Get the total work count
    #[inline(always)]
    pub fn work_count(&self) -> usize {
        self.0.work_count()
    }

    /// Spawn a new coroutine with default options
    pub fn spawn<F, T>(f: F) -> JoinHandle<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        scheduler::Scheduler::spawn(f)
    }

    /// Spawn a new coroutine with options
    pub fn spawn_opts<F, T>(f: F, opts: Options) -> JoinHandle<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        scheduler::Scheduler::spawn_opts(f, opts)
    }

    /// Run the scheduler
    pub fn run<F, T>(&mut self, f: F) -> Result<T, Box<Any + Send>>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        self.0.run(f)
    }

    /// Yield the current coroutine
    #[inline]
    pub fn sched() {
        scheduler::Scheduler::sched()
    }

    /// Put the current coroutine to sleep for the specific amount of time
    #[inline]
    pub fn sleep(duration: Duration) {
        match scheduler::Scheduler::instance() {
            None => thread::sleep(duration),
            Some(s) => s.sleep(duration).unwrap(),
        }
    }

    /// Block the current coroutine
    #[inline]
    pub fn block_with<'scope, F>(f: F)
        where F: FnOnce(&mut Processor, Coroutine) + 'scope,
    {
        Processor::current().map(|x| x.block_with(|p, coro| f(p, Coroutine(coro)))).unwrap()
    }

    /// Run a Coroutine in this scheduler
    pub fn ready(coro: Coroutine) {
        scheduler::Scheduler::ready(coro.0)
    }

    /// Get the current Coroutine
    pub fn current() -> Option<CoroutineRef<'static>> {
        Processor::instance().and_then(|p| p.current_coroutine().map(CoroutineRef))
    }
}

unsafe fn try<R, F: FnOnce() -> R>(f: F) -> thread::Result<R> {
    let mut f = Some(f);
    let f = &mut f as *mut Option<F> as usize;
    panic::recover(move || {
        (*(f as *mut Option<F>)).take().unwrap()()
    })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_sleep_ms() {
        Scheduler::new()
            .run(|| {
                sleep_ms(1000);
            }).unwrap();
    }
}
