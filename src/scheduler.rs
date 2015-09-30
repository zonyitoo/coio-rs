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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Sender;
use std::default::Default;
use std::any::Any;

use deque::Stealer;

use runtime::processor::{Processor, ProcMessage};
use coroutine::{Coroutine, SendableCoroutinePtr};
use options::Options;

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
    expected_worker_count: usize,
}

unsafe impl Send for Scheduler {}
unsafe impl Sync for Scheduler {}

impl Scheduler {
    pub fn with_workers(workers: usize) -> Scheduler {
        assert!(workers >= 1, "Must have at least one worker");
        Scheduler {
            work_counts: AtomicUsize::new(0),
            proc_handles: Mutex::new(Vec::new()),
            expected_worker_count: workers,
        }
    }

    /// Get the global Scheduler
    #[inline]
    pub fn instance() -> &'static Scheduler {
        Processor::current().scheduler()
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
        let wrapper = move|| {
            let ret = unsafe { ::try(move|| f()) };

            // No matter whether it is panicked or not, the result will be sent to the channel
            let _ = tx.send(ret); // Just ignore if it failed
        };
        Processor::current().spawn_opts(Box::new(wrapper), opts);

        JoinHandle {
            result: rx,
        }
    }

    /// Run the scheduler
    pub fn run<M, R>(self, main_fn: M) -> Result<R, Box<Any + Send + 'static>>
        where M: FnOnce() -> R + Send + 'static,
              R: Send + 'static
    {
        let the_sched = Arc::new(self);
        let mut handles = Vec::new();
        let main_coro_hdl = {
            // The first worker
            let mut proc_handles = the_sched.proc_handles.lock().unwrap();
            let (hdl, msg, st, main_hdl) = Processor::run_with_fn(the_sched.clone(), main_fn);
            handles.push(hdl);
            proc_handles.push((msg, st));
            main_hdl
        };

        // The others
        for _ in 1..the_sched.expected_worker_count {
            let mut proc_handles = the_sched.proc_handles.lock().unwrap();
            let (hdl, msg, st) = Processor::run_with_neighbors(the_sched.clone(),
                                                               proc_handles.iter().map(|x| x.1.clone()).collect());

            for &(ref msg, _) in proc_handles.iter() {
                if let Err(err) = msg.send(ProcMessage::NewNeighbor(st.clone())) {
                    error!("Error while sending NewNeighbor {:?}", err);
                }
            }

            handles.push(hdl);
            proc_handles.push((msg, st));
        }

        let main_ret = main_coro_hdl.recv().unwrap();
        // TODO: Ask all workers to exit!
        for &(ref msg, _) in the_sched.proc_handles.lock().unwrap().iter() {
            let _ = msg.send(ProcMessage::Shutdown);
        }

        for hdl in handles {
            hdl.join().unwrap();
        }

        main_ret
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
        Scheduler::with_workers(1).run(|| {
            let guard = Scheduler::spawn(|| 1);

            assert_eq!(1, guard.join().unwrap());
        }).unwrap();
    }
}
