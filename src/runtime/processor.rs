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

use chrono::NaiveDateTime;

use scheduler::{Scheduler, CoroutineRefMut};
use coroutine::{self, Coroutine, State, Handle};
use options::Options;
use runtime::timer::Timer;

thread_local!(static PROCESSOR: UnsafeCell<Processor> = UnsafeCell::new(Processor::new()));

/// Processing unit of a thread
pub struct Processor {
    event_loop: EventLoop<IoHandler>,
    // global_work_queue: Arc<BoundedQueue<CoroutineRefMut>>,
    handler: IoHandler,
    main_coro: Handle,
    cur_running: Option<CoroutineRefMut>,
    last_result: Option<coroutine::Result<State>>,

    queue_worker: Worker<CoroutineRefMut>,
    queue_stealer: Stealer<CoroutineRefMut>,
    neighbor_stealers: Vec<Stealer<CoroutineRefMut>>,

    chan_sender: Sender<ProcMessage>,
    chan_receiver: Receiver<ProcMessage>,

    is_scheduling: bool,
    has_ready_tasks: bool,

    coroutine_timer: Timer,
}

impl Processor {
    fn new() -> Processor {
        Processor::new_with_neighbors(Vec::new())
    }

    fn new_with_neighbors(neigh: Vec<Stealer<CoroutineRefMut>>) -> Processor {
        let main_coro = unsafe {
            Coroutine::empty()
        };

        let (worker, stealer) = BufferPool::new().deque();
        let (tx, rx) = mpsc::channel();

        Processor {
            event_loop: EventLoop::new().unwrap(),
            // global_work_queue: Scheduler::get().get_queue(),
            handler: IoHandler::new(),
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

            coroutine_timer: Timer::new(),
        }
    }

    #[doc(hidden)]
    pub unsafe fn running(&mut self) -> Option<CoroutineRefMut> {
        self.cur_running
    }

    #[doc(hidden)]
    pub fn set_neighbors(&mut self, neigh: Vec<Stealer<CoroutineRefMut>>) {
        self.neighbor_stealers = neigh;
    }

    /// Get the thread local processor
    pub fn current() -> &'static mut Processor {
        PROCESSOR.with(|p| unsafe { &mut *p.get() })
    }

    pub fn stealer(&self) -> Stealer<CoroutineRefMut> {
        self.queue_stealer.clone()
    }

    pub fn handle(&self) -> Sender<ProcMessage> {
        self.chan_sender.clone()
    }

    pub fn ready(&mut self, coro: CoroutineRefMut) {
        self.has_ready_tasks = true;
        self.queue_worker.push(coro);
    }

    pub fn spawn_opts<F>(&mut self, f: Box<F>, opts: Options)
        where F: FnBox() + 'static
    {
        let coro = Coroutine::spawn_opts(f, opts);
        let coro = CoroutineRefMut::new(unsafe { mem::transmute(coro) });

        self.ready(coro);
        self.sched();
    }

    #[doc(hidden)]
    pub fn set_last_result(&mut self, r: coroutine::Result<State>) {
        self.last_result = Some(r);
    }

    fn run_with_all_local_tasks(&mut self, hdl: CoroutineRefMut) {
        let mut hdl = hdl;
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
                Some(h) => hdl = h,
                None => break
            }
        }
    }

    #[doc(hidden)]
    pub fn schedule(&mut self) -> io::Result<()> {
        self.is_scheduling = true;

        'outerloop:
        loop {
            // 0. Check the mailbox
            while let Ok(msg) = self.chan_receiver.try_recv() {
                match msg {
                    ProcMessage::NewNeighbor(nei) => self.neighbor_stealers.push(nei),
                }
            }

            // 1. Run all tasks in local queue
            if let Some(hdl) = self.queue_worker.pop() {
                self.run_with_all_local_tasks(hdl);
            } else {
                self.has_ready_tasks = false;
            }

            // 2. Get work from timer
            while let Some(coro_ptr) = unsafe { self.coroutine_timer.try_awake() } {
                self.run_with_all_local_tasks(CoroutineRefMut::new(coro_ptr));
            }

            // 3. Well, check if there is some work could be waked up
            if self.handler.slabs.count() != 0 {
                if let Err(err) = self.event_loop.run_once(&mut self.handler) {
                    self.is_scheduling = false;
                    error!("EventLoop failed with {:?}", err);
                    return Err(err);
                }

                if self.has_ready_tasks {
                    continue;
                }
            } else if Scheduler::get().work_count() == 0 {
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
                    self.run_with_all_local_tasks(hdl);
                    continue 'outerloop;
                }
            }

            thread::sleep_ms(100);
        }

        self.is_scheduling = false;

        Ok(())
    }

    #[doc(hidden)]
    pub fn resume(&mut self, coro_ref: CoroutineRefMut) -> coroutine::Result<State> {
        self.cur_running = Some(coro_ref);
        unsafe {
            self.main_coro.yield_to(&mut *coro_ref.coro_ptr);
        }

        match self.last_result.take() {
            None => Ok(State::Suspended),
            Some(r) => r,
        }
    }

    /// Block the current coroutine until the specific time
    pub fn wait_until(&mut self, target_time: NaiveDateTime) {
        if let Some(coro_ref) = unsafe { self.running() } {
            // Set into the timer
            self.coroutine_timer.wait_until(coro_ref.coro_ptr, target_time);

            // Just block it, the handle has already been sent to
            // the timer.
            self.block();
        }
    }

    /// Suspended the current running coroutine, equivalent to `Scheduler::sched`
    pub fn sched(&mut self) {
        match self.cur_running.take() {
            None => {},
            Some(coro_ref) => unsafe {
                self.set_last_result(Ok(State::Suspended));
                (&mut *coro_ref.coro_ptr).yield_to(&*self.main_coro)
            }
        }
    }

    /// Block the current running coroutine, equivalent to `Scheduler::block`
    pub fn block(&mut self) {
        match self.cur_running.take() {
            None => {},
            Some(coro_ref) => unsafe {
                self.set_last_result(Ok(State::Blocked));
                (&mut *coro_ref.coro_ptr).yield_to(&*self.main_coro)
            }
        }
    }

    /// Yield the current running coroutine with specified result
    pub fn yield_with(&mut self, r: coroutine::Result<State>) {
        match self.cur_running.take() {
            None => {},
            Some(coro_ref) => unsafe {
                self.set_last_result(r);
                (&mut *coro_ref.coro_ptr).yield_to(&*self.main_coro)
            }
        }
    }
}

const MAX_TOKEN_NUM: usize = 102400;
impl IoHandler {
    fn new() -> IoHandler {
        IoHandler {
            slabs: Slab::new(MAX_TOKEN_NUM),
        }
    }
}

#[cfg(any(target_os = "linux",
          target_os = "android"))]
impl Processor {
    /// Register and wait I/O
    pub fn wait_event<E: Evented + AsRawFd>(&mut self, fd: &E, interest: EventSet) -> io::Result<()> {
        let token = self.handler.slabs.insert((unsafe { Processor::current().running().unwrap() },
                                               From::from(fd.as_raw_fd()))).unwrap();
        try!(self.event_loop.register_opt(fd, token, interest,
                                          PollOpt::edge()|PollOpt::oneshot()));

        debug!("wait_event: Blocked current Coroutine ...; token={:?}", token);
        Scheduler::block();
        debug!("wait_event: Waked up; token={:?}", token);

        Ok(())
    }
}

#[cfg(any(target_os = "linux",
          target_os = "android"))]
struct IoHandler {
    slabs: Slab<(CoroutineRefMut, Io)>,
}

#[cfg(any(target_os = "linux",
          target_os = "android"))]
impl Handler for IoHandler {
    type Timeout = ();
    type Message = ();

    fn ready(&mut self, event_loop: &mut EventLoop<Self>, token: Token, events: EventSet) {
        debug!("Got {:?} for {:?}", events, token);

        match self.slabs.remove(token) {
            Some((hdl, fd)) => {
                // Linux EPoll needs to explicit EPOLL_CTL_DEL the fd
                event_loop.deregister(&fd).unwrap();
                mem::forget(fd);
                Scheduler::ready(hdl);
            },
            None => {
                warn!("No coroutine is waiting on readable {:?}", token);
            }
        }
    }
}

#[cfg(any(target_os = "macos",
          target_os = "freebsd",
          target_os = "dragonfly",
          target_os = "ios",
          target_os = "bitrig",
          target_os = "openbsd"))]
impl Processor {
    /// Register and wait I/O
    pub fn wait_event<E: Evented>(&mut self, fd: &E, interest: EventSet) -> io::Result<()> {
        let token = self.handler.slabs.insert(unsafe { Processor::current().running().unwrap() }).unwrap();
        try!(self.event_loop.register_opt(fd, token, interest,
                                          PollOpt::edge()|PollOpt::oneshot()));

        debug!("wait_event: Blocked current Coroutine ...; token={:?}", token);
        // Coroutine::block();
        Scheduler::block();
        debug!("wait_event: Waked up; token={:?}", token);

        Ok(())
    }
}

#[cfg(any(target_os = "macos",
          target_os = "freebsd",
          target_os = "dragonfly",
          target_os = "ios",
          target_os = "bitrig",
          target_os = "openbsd"))]
struct IoHandler {
    slabs: Slab<CoroutineRefMut>,
}

#[cfg(any(target_os = "macos",
          target_os = "freebsd",
          target_os = "dragonfly",
          target_os = "ios",
          target_os = "bitrig",
          target_os = "openbsd"))]
impl Handler for IoHandler {
    type Timeout = ();
    type Message = ();

    fn ready(&mut self, _: &mut EventLoop<Self>, token: Token, events: EventSet) {
        debug!("Got {:?} for {:?}", events, token);

        match self.slabs.remove(token) {
            Some(hdl) => {
                Scheduler::ready(hdl);
            },
            None => {
                warn!("No coroutine is waiting on readable {:?}", token);
            }
        }
    }
}

pub enum ProcMessage {
    NewNeighbor(Stealer<CoroutineRefMut>),
}
