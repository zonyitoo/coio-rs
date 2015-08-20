
#![feature(rt, reflect_marker, box_raw)]

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

pub use scheduler::Scheduler;
pub use options::Options;

pub mod net;
pub mod sync;
pub mod scheduler;
pub mod options;
pub mod processor;
mod coroutine;

/// Spawn a new Coroutine
pub fn spawn<F>(f: F)
    where F: FnOnce() + Send + 'static
{
    Scheduler::spawn(f)
}

/// Spawn a new Coroutine with options
pub fn spawn_opts<F>(f: F, opts: Options)
    where F: FnOnce() + Send + 'static
{
    Scheduler::spawn_opts(f, opts)
}

/// Giveup the CPU
pub fn sched() {
    Scheduler::sched()
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

    pub fn spawn<F>(self, f: F)
        where F: FnOnce() + Send + 'static
    {
        Scheduler::spawn_opts(f, self.opts)
    }
}
