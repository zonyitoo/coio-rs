
#![feature(rt, libc, reflect_marker, box_raw, fnbox, result_expect)]

#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate context;
extern crate mio;
extern crate bytes;
extern crate hyper;
extern crate url;
extern crate deque;
extern crate rand;
extern crate libc;
extern crate chrono;

pub use scheduler::{Scheduler, JoinHandle};
pub use options::Options;

pub mod net;
pub mod sync;
pub mod scheduler;
pub mod options;
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

/// Run the scheduler with threads
#[inline(always)]
pub fn run(threads: usize) {
    Scheduler::run(threads)
}

/// Put the current coroutine to sleep for the specific amount of time
#[inline]
pub fn sleep_ms(ms: u32) {
    use chrono::*;
    let target = Local::now() + Duration::milliseconds(ms as i64);

    runtime::Processor::current()
        .wait_until(target.naive_local());
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
    pub fn name(mut self, name: Option<String>) -> Builder {
        self.opts.name = name;
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_sleep_ms() {
        spawn(|| {
            sleep_ms(1000);
        });

        run(1);
    }
}
