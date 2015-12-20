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
use std::sync::{Arc, Mutex, Condvar};
use std::sync::mpsc::{Sender, TryRecvError};
use std::default::Default;
use std::any::Any;
use std::thread;
use std::io;
use std::cell::UnsafeCell;
use std::time::Duration;

use deque::Stealer;

use mio::{EventLoop, Evented, Handler, Token, EventSet, PollOpt};
use mio::util::Slab;

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

struct IoHandler {
    io_slab: Slab<(usize, *mut Coroutine)>,
    timer_slab: Slab<(usize, *mut Coroutine)>,

    processors: Vec<(Sender<ProcMessage>, Stealer<SendableCoroutinePtr>)>,
}

impl Handler for IoHandler {
    type Timeout = Token;
    type Message = ();

    fn ready(&mut self, _: &mut EventLoop<Self>, token: Token, events: EventSet) {
        debug!("Got {:?} for {:?}", events, token);
        if token == Token(0) {
            error!("Received events from Token(0): {:?}", events);
            return;
        }

        match self.io_slab.remove(token) {
            Some((idx, hdl)) => {
                self.processors[idx].0.send(ProcMessage::Ready(SendableCoroutinePtr(hdl))).unwrap();
            }
            None => {
                warn!("No coroutine is waiting on token {:?}", token);
            }
        }
    }

    fn timeout(&mut self, _: &mut EventLoop<Self>, token: Token) {
        debug!("Timer waked up {:?}", token);
        match self.timer_slab.remove(token) {
            Some((idx, hdl)) => {
                self.processors[idx].0.send(ProcMessage::Ready(SendableCoroutinePtr(hdl))).unwrap();
            }
            None => {
                warn!("Timer token {:?} was awaited without waiting coroutines",
                      token);
            }
        }
    }
}

impl IoHandler {
    fn new() -> IoHandler {
        IoHandler {
            io_slab: Slab::new_starting_at(Token(1), 102400),
            timer_slab: Slab::new_starting_at(Token(1), 102400),

            processors: Vec::new(),
        }
    }
}

/// Coroutine scheduler
pub struct Scheduler {
    work_counts: AtomicUsize,
    // proc_handles: Mutex<Vec<(Sender<ProcMessage>, Stealer<SendableCoroutinePtr>)>>,
    expected_worker_count: usize,
    starving_lock: Arc<(Mutex<usize>, Condvar)>,

    eventloop: UnsafeCell<EventLoop<IoHandler>>,
    io_handler: Mutex<IoHandler>,
}

unsafe impl Send for Scheduler {}
unsafe impl Sync for Scheduler {}

impl Scheduler {
    /// Create a scheduler with default configurations
    pub fn new() -> Scheduler {
        Scheduler {
            work_counts: AtomicUsize::new(0),
            // proc_handles: Mutex::new(Vec::new()),
            expected_worker_count: 1,
            starving_lock: Arc::new((Mutex::new(0), Condvar::new())),

            eventloop: UnsafeCell::new(EventLoop::new().unwrap()),
            io_handler: Mutex::new(IoHandler::new()),
        }
    }

    /// Set the number of workers
    pub fn with_workers(mut self, workers: usize) -> Scheduler {
        assert!(workers >= 1, "Must have at least one worker");
        self.expected_worker_count = workers;
        self
    }

    /// Get the global Scheduler
    #[doc(hidden)]
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

    #[doc(hidden)]
    #[inline]
    pub fn proc_wait(&self) {
        let &(ref lock, ref cond) = &*self.starving_lock;
        let mut guard = lock.lock().unwrap();
        *guard -= 1;
        if *guard != 0 {
            debug!("Thread {:?} is starving and exile ...", thread::current());
            guard = cond.wait(guard).unwrap();
        }
        *guard += 1;
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
    #[inline]
    pub fn work_count(&self) -> usize {
        self.work_counts.load(Ordering::SeqCst)
    }

    /// Spawn a new coroutine with default options
    #[inline]
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
        let wrapper = move || {
            let ret = unsafe { ::try(move || f()) };

            // No matter whether it is panicked or not, the result will be sent to the channel
            let _ = tx.send(ret); // Just ignore if it failed
        };
        Processor::current().spawn_opts(Box::new(wrapper), opts);

        let &(ref lock, ref cond) = &*Scheduler::instance().starving_lock;
        let _ = lock.lock().unwrap();
        cond.notify_one();

        JoinHandle { result: rx }
    }

    /// Run the scheduler
    pub fn run<M, R>(self, main_fn: M) -> Result<R, Box<Any + Send + 'static>>
        where M: FnOnce() -> R + Send + 'static,
              R: Send + 'static
    {
        let the_sched = Arc::new(self);
        let mut handles = Vec::new();

        *the_sched.starving_lock.0.lock().unwrap() = 1;
        let main_coro_hdl = {
            // The first worker
            let mut io_handler = the_sched.io_handler.lock().unwrap();

            let (hdl, msg, st, main_hdl) = Processor::run_main(0, the_sched.clone(), main_fn);
            handles.push(hdl);
            io_handler.processors.push((msg, st));
            main_hdl
        };

        {
            *the_sched.starving_lock.0.lock().unwrap() = the_sched.expected_worker_count;

            // The others
            for tid in 1..the_sched.expected_worker_count {
                let mut io_handler = the_sched.io_handler.lock().unwrap();

                let (hdl, msg, st) = Processor::run_with_neighbors(tid,
                                                                   the_sched.clone(),
                                                                   io_handler.processors
                                                                             .iter()
                                                                             .map(|x| x.1.clone())
                                                                             .collect());

                for &(ref msg, _) in io_handler.processors.iter() {
                    if let Err(err) = msg.send(ProcMessage::NewNeighbor(st.clone())) {
                        error!("Error while sending NewNeighbor {:?}", err);
                    }
                }

                handles.push(hdl);
                io_handler.processors.push((msg, st));
            }
        }

        loop {
            {
                let mut io_handler = the_sched.io_handler.lock().unwrap();

                if io_handler.io_slab.count() != 0 || io_handler.timer_slab.count() != 0 {
                    let event_loop: &mut EventLoop<IoHandler> = unsafe {
                        &mut *the_sched.eventloop.get()
                    };

                    event_loop.run_once(&mut io_handler, Some(100)).unwrap();
                }
            }

            match main_coro_hdl.try_recv() {
                Ok(main_ret) => {
                    {
                        let io_handler = the_sched.io_handler.lock().unwrap();

                        for &(ref msg, _) in io_handler.processors.iter() {
                            let _ = msg.send(ProcMessage::Shutdown);
                        }

                        for &(id, ptr) in io_handler.io_slab.iter() {
                            let _ = io_handler.processors[id]
                                        .0
                                        .send(ProcMessage::Ready(SendableCoroutinePtr(ptr)));
                        }

                        for &(id, ptr) in io_handler.timer_slab.iter() {
                            let _ = io_handler.processors[id]
                                        .0
                                        .send(ProcMessage::Ready(SendableCoroutinePtr(ptr)));
                        }
                    }

                    {
                        let &(ref lock, ref cond) = &*the_sched.starving_lock;
                        let _ = lock.lock().unwrap();
                        cond.notify_all();
                    }

                    for hdl in handles {
                        hdl.join().unwrap();
                    }

                    return main_ret;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    panic!("Main coro is disconnected");
                }
            }
        }
    }


    /// Suspend the current coroutine
    #[inline]
    pub fn sched() {
        Processor::current().sched();
    }

    /// Block the current coroutine
    #[inline]
    pub fn block() {
        Processor::current().block();
    }
}

impl Scheduler {
    /// Block the current coroutine and wait for I/O event
    #[doc(hidden)]
    pub fn wait_event<E: Evented>(&self, fd: &E, interest: EventSet) -> io::Result<()> {
        let processor = Processor::current();

        if let Some(ptr) = unsafe { processor.running() } {
            let event_loop: &mut EventLoop<IoHandler> = unsafe { &mut *self.eventloop.get() };
            let token = {
                let mut io_handler = self.io_handler.lock().unwrap();

                let token = io_handler.io_slab
                                      .insert((processor.id(), ptr))
                                      .unwrap();

                try!(event_loop.register(fd,
                                         token,
                                         interest,
                                         // PollOpt::edge()|PollOpt::oneshot()));
                                         PollOpt::level()));
                token
            };
            debug!("wait_event: Blocked current Coroutine ...; token={:?}",
                   token);

            processor.block();
            debug!("wait_event: Waked up; token={:?}", token);

            // For the latest MIO version, it requires to deregister every Evented object
            try!(event_loop.deregister(fd));
        }

        Ok(())
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep_ms(&self, delay: u64) {
        let processor = Processor::current();

        if let Some(ptr) = unsafe { processor.running() } {
            {
                let mut io_handler = self.io_handler.lock().unwrap();

                let token = io_handler.timer_slab
                                      .insert((processor.id(), ptr))
                                      .unwrap();

                let event_loop: &mut EventLoop<IoHandler> = unsafe { &mut *self.eventloop.get() };

                event_loop.timeout_ms(token, delay).unwrap();
            }

            processor.block();
        }
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep(&self, delay: Duration) {
        self.sleep_ms(delay.as_secs() * 1_000 + delay.subsec_nanos() as u64 / 1_000_000)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_join_basic() {
        Scheduler::new()
            .run(|| {
                let guard = Scheduler::spawn(|| 1);

                assert_eq!(1, guard.join().unwrap());
            })
            .unwrap();
    }
}
