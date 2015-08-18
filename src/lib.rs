
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

pub mod net;
pub mod sync;
pub mod scheduler;
pub mod options;
pub mod processor;
mod coroutine;
