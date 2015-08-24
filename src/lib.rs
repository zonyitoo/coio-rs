
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

pub use scheduler::{Scheduler, JoinHandle};
pub use options::Options;

pub mod net;
pub mod sync;
pub mod scheduler;
pub mod options;
pub mod processor;
mod coroutine;

/// Spawn a new Coroutine
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
    where F: FnOnce() -> T + Send + 'static,
          T: Send + 'static
{
    Scheduler::spawn(f)
}

/// Spawn a new Coroutine with options
pub fn spawn_opts<F, T>(f: F, opts: Options) -> JoinHandle<T>
    where F: FnOnce() -> T + Send + 'static,
          T: Send + 'static
{
    Scheduler::spawn_opts(f, opts)
}

/// Giveup the CPU
pub fn sched() {
    Scheduler::sched()
}

/// Run the scheduler with threads
pub fn run(threads: usize) {
    Scheduler::run(threads)
}

pub struct Builder {
    opts: Options
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            opts: Options::new()
        }
    }

    pub fn stack_size(mut self, stack_size: usize) -> Builder {
        self.opts.stack_size = stack_size;
        self
    }

    pub fn name(mut self, name: Option<String>) -> Builder {
        self.opts.name = name;
        self
    }

    pub fn spawn<F, T>(self, f: F) -> JoinHandle<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        Scheduler::spawn_opts(f, self.opts)
    }
}
