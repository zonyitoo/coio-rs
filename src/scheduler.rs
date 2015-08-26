// The MIT License (MIT)

// Copyright (c) 2015 Rustcc Developers

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

//! Global coroutine scheduler

use std::thread;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::mpsc::Sender;
use std::default::Default;
use std::any::Any;
use std::rt;

use deque::Stealer;

use runtime::processor::{Processor, ProcMessage};
use coroutine::{Coroutine, SendableCoroutinePtr};
use options::Options;

lazy_static! {
    static ref SCHEDULER: Scheduler = Scheduler::new();
}

/// A handle that could join the coroutine
pub struct JoinHandle<T> {
    result: ::sync::mpsc::Receiver<Result<T, Box<Any + Send + 'static>>>,
}

impl<T> JoinHandle<T> {
    /// Join the coroutine until it finishes.
    ///
    /// If it already finished, this method will return immediately.
    pub fn join(&self) -> Result<T, Box<Any + Send + 'static>> {
        self.result.recv().expect("Failed to receive from the channel")
    }
}

unsafe impl<T: Send> Send for JoinHandle<T> {}

/// Coroutine scheduler
pub struct Scheduler {
    work_counts: AtomicUsize,
    proc_handles: Mutex<Vec<(Sender<ProcMessage>, Stealer<SendableCoroutinePtr>)>>,
}

unsafe impl Send for Scheduler {}
unsafe impl Sync for Scheduler {}

impl Scheduler {
    fn new() -> Scheduler {
        Scheduler {
            work_counts: AtomicUsize::new(0),
            proc_handles: Mutex::new(Vec::new()),
        }
    }

    /// Get the global Scheduler
    #[inline]
    pub fn instance() -> &'static Scheduler {
        &SCHEDULER
    }

    /// A coroutine is ready for schedule
    #[doc(hidden)]
    #[inline]
    pub unsafe fn ready(coro: *mut Coroutine) {
        Processor::current().ready(coro);
    }

    /// A coroutine is finished
    ///
    /// The coroutine will be destroy, make sure that the coroutine pointer is unique!
    #[doc(hidden)]
    #[inline]
    pub unsafe fn finished(coro_ptr: *mut Coroutine) {
        Scheduler::instance().work_counts.fetch_sub(1, Ordering::SeqCst);

        let boxed = Box::from_raw(coro_ptr);
        drop(boxed);
    }

    /// Total works
    pub fn work_count(&self) -> usize {
        Scheduler::instance().work_counts.load(Ordering::SeqCst)
    }

    /// Spawn a new coroutine
    pub fn spawn<F, T>(f: F) -> JoinHandle<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        Scheduler::spawn_opts(f, Default::default())
    }

    /// Spawn a new coroutine with options
    pub fn spawn_opts<F, T>(f: F, opts: Options) -> JoinHandle<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        Scheduler::instance().work_counts.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = ::sync::mpsc::channel();
        let wrapper = move|| unsafe {
            let mut output = None;
            let ret = {
                let ptr = &mut output;
                rt::unwind::try(move|| *ptr = Some(f()))
            };

            // No matter whether it is panicked or not, the result will be sent to the channel
            let _ = tx.send(ret.map(|()| output.unwrap())); // Just ignore if it failed
        };
        Processor::current().spawn_opts(Box::new(wrapper), opts);

        JoinHandle {
            result: rx,
        }
    }

    /// Run the scheduler with `n` threads
    pub fn run(n: usize) {
        assert!(n >= 1, "There must be at least 1 thread");
        Scheduler::instance().proc_handles.lock().unwrap().clear();

        fn do_work() {
            {
                let mut guard = Scheduler::instance().proc_handles.lock().unwrap();
                Processor::current().set_neighbors(guard.iter().map(|x| x.1.clone()).collect());

                let hdl = Processor::current().handle();
                let stealer = Processor::current().stealer();
                for neigh in guard.iter() {
                    let &(ref sender, _) = neigh;
                    if let Err(err) = sender.send(ProcMessage::NewNeighbor(stealer.clone())) {
                        error!("Error while sending NewNeighbor {:?}", err);
                    }
                }

                guard.push((hdl, stealer));
            }

            match Processor::current().schedule() {
                Ok(..) => {},
                Err(err) => panic!("Processor schedule error: {:?}", err),
            }
        }

        let mut futs = Vec::new();
        for _ in 1..n {
            let fut = thread::spawn(do_work);

            futs.push(fut);
        }

        do_work();

        for fut in futs.into_iter() {
            fut.join().unwrap();
        }
    }

    /// Suspend the current coroutine
    pub fn sched() {
        Processor::current().sched();
    }

    /// Block the current coroutine
    pub fn block() {
        Processor::current().block();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_join_basic() {
        let mut guard = Scheduler::spawn(|| 1);
        Scheduler::run(1);

        assert_eq!(1, guard.join().unwrap());
    }
}
