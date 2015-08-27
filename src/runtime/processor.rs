// The MIT License (MIT)

// Copyright (c) 2015 Y. T. Chung <zonyitoo@gmail.com>

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

//! Processing unit of a thread

use std::cell::UnsafeCell;
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::convert::From;
// use std::sync::Arc;
use std::thread;
use std::mem;
use std::sync::mpsc::{self, Sender, Receiver};
use std::boxed::FnBox;

use mio::{EventLoop, Evented, Handler, Token, EventSet, PollOpt};
use mio::util::Slab;
#[cfg(target_os = "linux")]
use mio::Io;

// use mio::util::BoundedQueue;

use deque::{BufferPool, Stolen, Worker, Stealer};

use rand;

use scheduler::Scheduler;
use coroutine::{self, Coroutine, State, Handle, SendableCoroutinePtr};
use options::Options;

const MAX_TOKEN_NUM: usize = 102400;

thread_local!(static PROCESSOR: UnsafeCell<Processor> = UnsafeCell::new(Processor::new()));

/// Processing unit of a thread
pub struct Processor {
    event_loop: EventLoop<Processor>,

    main_coro: Handle,
    cur_running: Option<*mut Coroutine>,
    last_result: Option<coroutine::Result<State>>,

    queue_worker: Worker<SendableCoroutinePtr>,
    queue_stealer: Stealer<SendableCoroutinePtr>,
    neighbor_stealers: Vec<Stealer<SendableCoroutinePtr>>,

    chan_sender: Sender<ProcMessage>,
    chan_receiver: Receiver<ProcMessage>,

    is_scheduling: bool,
    has_ready_tasks: bool,

    #[cfg(any(target_os = "linux",
              target_os = "android"))]
    io_slabs: Slab<(*mut Coroutine, Io)>,
    #[cfg(any(target_os = "macos",
              target_os = "freebsd",
              target_os = "dragonfly",
              target_os = "ios",
              target_os = "bitrig",
              target_os = "openbsd"))]
    io_slabs: Slab<*mut Coroutine>,

    timer_slabs: Slab<*mut Coroutine>,
}

impl Processor {
    fn new() -> Processor {
        Processor::new_with_neighbors(Vec::new())
    }

    fn new_with_neighbors(neigh: Vec<Stealer<SendableCoroutinePtr>>) -> Processor {
        let main_coro = unsafe {
            Coroutine::empty()
        };

        let (worker, stealer) = BufferPool::new().deque();
        let (tx, rx) = mpsc::channel();

        Processor {
            event_loop: EventLoop::new().expect("Unable to create the eventloop"),

            main_coro: main_coro,
            cur_running: None,
            last_result: None,

            queue_worker: worker,
            queue_stealer: stealer,
            neighbor_stealers: neigh,

            chan_sender: tx,
            chan_receiver: rx,

            is_scheduling: false,
            has_ready_tasks: false,

            io_slabs: Slab::new(MAX_TOKEN_NUM),

            timer_slabs: Slab::new(MAX_TOKEN_NUM),
        }
    }

    #[inline]
    // Get the current running coroutine
    pub unsafe fn running(&mut self) -> Option<*mut Coroutine> {
        self.cur_running
    }

    #[inline]
    pub fn set_neighbors(&mut self, neigh: Vec<Stealer<SendableCoroutinePtr>>) {
        self.neighbor_stealers = neigh;
    }

    /// Get the thread local processor
    #[inline]
    pub fn current() -> &'static mut Processor {
        PROCESSOR.with(|p| unsafe { &mut *p.get() })
    }

    #[inline]
    pub fn stealer(&self) -> Stealer<SendableCoroutinePtr> {
        self.queue_stealer.clone()
    }

    #[inline]
    pub fn handle(&self) -> Sender<ProcMessage> {
        self.chan_sender.clone()
    }

    #[inline]
    // Call by scheduler
    pub unsafe fn ready(&mut self, coro_ptr: *mut Coroutine) {
        self.has_ready_tasks = true;
        self.queue_worker.push(SendableCoroutinePtr(coro_ptr));
    }

    #[inline]
    pub fn spawn_opts<F>(&mut self, f: Box<F>, opts: Options)
        where F: FnBox() + 'static
    {
        let coro = Coroutine::spawn_opts(f, opts);

        unsafe {
            self.ready(mem::transmute(coro));
        }
        self.sched();
    }

    #[inline]
    fn set_last_result(&mut self, r: coroutine::Result<State>) {
        self.last_result = Some(r);
    }

    #[inline]
    unsafe fn run_with_all_local_tasks(&mut self, coro_ptr: *mut Coroutine) {
        let mut hdl = coro_ptr;
        loop {
            let is_suspended = match self.resume(hdl) {
                Ok(State::Suspended) => {
                    true
                },
                Ok(State::Finished) | Err(..) => {
                    Scheduler::finished(hdl);
                    false
                },
                Ok(..) => {
                    false
                }
            };

            // Try to fetch one task from the local queue
            let next_hdl = self.queue_worker.pop();
            if is_suspended {
                // If the current task has to be suspended, then
                // push it back to the local work queue
                self.ready(hdl);
            }

            match next_hdl {
                Some(h) => hdl = h.0,
                None => break
            }
        }
    }

    /// Run the processor
    pub fn schedule(&mut self) -> io::Result<()> {
        self.is_scheduling = true;

        'outerloop:
        loop {
            // 1. Run all tasks in local queue
            if let Some(hdl) = self.queue_worker.pop() {
                unsafe {
                    self.run_with_all_local_tasks(hdl.0);
                }
            } else {
                self.has_ready_tasks = false;
            }

            // 2. Get work from timer
            //    In favor of mio's timer implementation

            // 3. Well, check if there are any works could be waked up
            if self.io_slabs.count() != 0 || self.timer_slabs.count() != 0 {
                // Make the borrow checker happy.
                let proc_ptr: *mut Processor = self;
                if let Err(err) = self.event_loop.run_once(unsafe { &mut *proc_ptr }) {
                    self.is_scheduling = false;
                    error!("EventLoop failed with {:?}", err);
                    return Err(err);
                }

                if self.has_ready_tasks {
                    continue 'outerloop;
                }
            } else if Scheduler::instance().work_count() == 0 {
                break;
            } else {
                // We don't have active tasks in the local queue
                // And we don't have any activated tasks from event loop
            }

            // 4. Randomly steal from neighbors
            let rand_idx = rand::random::<usize>();
            let total_stealers = self.neighbor_stealers.len();
            for idx in (0..self.neighbor_stealers.len()).map(|x| (x + rand_idx) % total_stealers) {
                if let Stolen::Data(hdl) = self.neighbor_stealers[idx].steal() {
                    unsafe {
                        self.run_with_all_local_tasks(hdl.0);
                    }
                    continue 'outerloop;
                }
            }

            // 5. Check the mailbox
            while let Ok(msg) = self.chan_receiver.try_recv() {
                match msg {
                    ProcMessage::NewNeighbor(nei) => self.neighbor_stealers.push(nei),
                }
            }

            thread::sleep_ms(100);
        }

        self.is_scheduling = false;

        Ok(())
    }

    #[inline]
    pub fn resume(&mut self, coro_ptr: *mut Coroutine) -> coroutine::Result<State> {
        self.cur_running = Some(coro_ptr);
        unsafe {
            self.main_coro.yield_to(&mut *coro_ptr);
        }

        match self.last_result.take() {
            None => Ok(State::Suspended),
            Some(r) => r,
        }
    }

    /// Block the current coroutine until the specific time
    #[inline]
    pub fn sleep_ms(&mut self, delay: u64) {
        if let Some(coro_ptr) = unsafe { self.running() } {
            // Set into the timer
            loop {
                match self.timer_slabs.insert(coro_ptr) {
                    Ok(token) => {
                        debug!("Going to block with delay {} and {:?}", delay, token);
                        self.event_loop.timeout_ms(token, delay).unwrap();
                        break;
                    },
                    Err(..) => {}
                }
            }

            // Just block it, the handle has already been sent to
            // the timer.
            self.block();
        } else {
            warn!("Called `wait_until` without running coroutine");
        }
    }

    /// Suspended the current running coroutine, equivalent to `Scheduler::sched`
    #[inline]
    pub fn sched(&mut self) {
        match self.cur_running.take() {
            None => {},
            Some(coro_ptr) => unsafe {
                self.set_last_result(Ok(State::Suspended));
                (&mut *coro_ptr).yield_to(&*self.main_coro)
            }
        }
    }

    /// Block the current running coroutine, equivalent to `Scheduler::block`
    #[inline]
    pub fn block(&mut self) {
        match self.cur_running.take() {
            None => {},
            Some(coro_ptr) => unsafe {
                self.set_last_result(Ok(State::Blocked));
                (&mut *coro_ptr).yield_to(&*self.main_coro)
            }
        }
    }

    /// Yield the current running coroutine with specified result
    #[inline]
    pub fn yield_with(&mut self, r: coroutine::Result<State>) {
        match self.cur_running.take() {
            None => {},
            Some(coro_ptr) => unsafe {
                self.set_last_result(r);
                (&mut *coro_ptr).yield_to(&*self.main_coro)
            }
        }
    }
}


impl Processor {
    /// Register and wait I/O
    #[cfg(any(target_os = "linux",
              target_os = "android"))]
    pub fn wait_event<E: Evented + AsRawFd>(&mut self, fd: &E, interest: EventSet) -> io::Result<()> {
        let token = self.io_slabs.insert((unsafe { Processor::current().running().unwrap() },
                                               From::from(fd.as_raw_fd()))).unwrap();
        try!(self.event_loop.register_opt(fd, token, interest,
                                          PollOpt::edge()|PollOpt::oneshot()));

        debug!("wait_event: Blocked current Coroutine ...; token={:?}", token);
        self.block();
        debug!("wait_event: Waked up; token={:?}", token);

        Ok(())
    }

    #[cfg(any(target_os = "macos",
              target_os = "freebsd",
              target_os = "dragonfly",
              target_os = "ios",
              target_os = "bitrig",
              target_os = "openbsd"))]
    pub fn wait_event<E: Evented>(&mut self, fd: &E, interest: EventSet) -> io::Result<()> {
        let token = self.io_slabs.insert(unsafe { Processor::current().running().unwrap() }).unwrap();
        try!(self.event_loop.register_opt(fd, token, interest,
                                          PollOpt::edge()|PollOpt::oneshot()));

        debug!("wait_event: Blocked current Coroutine ...; token={:?}", token);
        self.block();
        debug!("wait_event: Waked up; token={:?}", token);

        Ok(())
    }
}

impl Handler for Processor {
    type Timeout = Token;
    type Message = ();

    #[cfg(any(target_os = "linux",
              target_os = "android"))]
    fn ready(&mut self, event_loop: &mut EventLoop<Self>, token: Token, events: EventSet) {
        debug!("Got {:?} for {:?}", events, token);

        match self.io_slabs.remove(token) {
            Some((hdl, fd)) => {
                // Linux EPoll needs to explicit EPOLL_CTL_DEL the fd
                event_loop.deregister(&fd).unwrap();
                mem::forget(fd);
                unsafe {
                    self.ready(hdl);
                }
            },
            None => {
                warn!("No coroutine is waiting on token {:?}", token);
            }
        }
    }

    #[cfg(any(target_os = "macos",
              target_os = "freebsd",
              target_os = "dragonfly",
              target_os = "ios",
              target_os = "bitrig",
              target_os = "openbsd"))]
    fn ready(&mut self, _: &mut EventLoop<Self>, token: Token, events: EventSet) {
        debug!("Got {:?} for {:?}", events, token);

        match self.io_slabs.remove(token) {
            Some(hdl) => {
                unsafe {
                    self.ready(hdl);
                }
            },
            None => {
                warn!("No coroutine is waiting on token {:?}", token);
            }
          }
    }

    fn timeout(&mut self, _: &mut EventLoop<Self>, token: Token) {
        debug!("Timer waked up {:?}", token);
        match self.timer_slabs.remove(token) {
            Some(coro_ptr) => unsafe {
                self.ready(coro_ptr);
            },
            None => {
                warn!("Timer token {:?} was awaited without waiting coroutines", token);
            }
        }
    }
}

pub enum ProcMessage {
    NewNeighbor(Stealer<SendableCoroutinePtr>),
}
