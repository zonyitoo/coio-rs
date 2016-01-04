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

use rand::Rng;
use std::any::Any;
use std::boxed::FnBox;
use std::cell::UnsafeCell;
use std::io;
use std::mem;
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, Builder};
use std::time::Duration;

use deque::{BufferPool, Stolen, Worker, Stealer};

use rand;

use coroutine::{self, Coroutine, State, Handle, SendableCoroutinePtr};
use options::Options;
use scheduler::Scheduler;

// TODO:
//  Reconsider making PROCESSOR a static object instead of a pointer.
//  This would simplify storage of Processor and improve performance of Processor::current()
// Blockers:
//  - std::thread::LocalKeyState is unstable (to avoid triggering the lazy init.).
//  - Investigate if LLVM generates dynamic or non-dynamic ELF TLS.
//    "dynamic" TLS is allocated lazily at first access of the store (which is what we want)
//    instead of being allocated statically for every thread.
//    ELF TLS Paper: "ELF Handling For Thread-Local Storage".
thread_local!(static PROCESSOR: UnsafeCell<*mut Processor> = UnsafeCell::new(ptr::null_mut()));

#[derive(Debug)]
pub struct ForceUnwind;

/// Processing unit of a thread
pub struct Processor {
    scheduler: *mut Scheduler,

    main_coro: Handle,
    cur_running: Option<*mut Coroutine>,
    last_result: Option<coroutine::Result<State>>,
    is_exiting: bool,

    rng: rand::XorShiftRng,
    queue_worker: Worker<SendableCoroutinePtr>,
    queue_stealer: Stealer<SendableCoroutinePtr>,
    neighbor_stealers: Vec<Stealer<SendableCoroutinePtr>>,

    chan_sender: Sender<ProcMessage>,
    chan_receiver: Receiver<ProcMessage>,

    is_scheduling: bool,
    has_ready_tasks: bool,
}

unsafe impl Send for Processor {}

impl Processor {
    fn new_with_neighbors(_processor_id: usize,
                          sched: *mut Scheduler,
                          neigh: Vec<Stealer<SendableCoroutinePtr>>)
                          -> Processor {
        let main_coro = unsafe { Coroutine::empty() };

        let (worker, stealer) = BufferPool::new().deque();
        let (tx, rx) = mpsc::channel();

        Processor {
            scheduler: sched,

            main_coro: main_coro,
            cur_running: None,
            last_result: None,
            is_exiting: false,

            rng: rand::weak_rng(),
            queue_worker: worker,
            queue_stealer: stealer,
            neighbor_stealers: neigh,

            chan_sender: tx,
            chan_receiver: rx,

            is_scheduling: false,
            has_ready_tasks: false,
        }
    }

    fn set_tls(p: &mut Processor) {
        PROCESSOR.with(|proc_opt| unsafe {
            *proc_opt.get() = p;
        })
    }

    fn unset_tls() {
        PROCESSOR.with(|proc_opt| unsafe {
            *proc_opt.get() = ptr::null_mut();
        })
    }

    #[inline]
    pub fn run_with_neighbors(processor_id: usize,
                              sched: *mut Scheduler,
                              neigh: Vec<Stealer<SendableCoroutinePtr>>)
                              -> (thread::JoinHandle<()>,
                                  Sender<ProcMessage>,
                                  Stealer<SendableCoroutinePtr>) {
        let mut p = Processor::new_with_neighbors(processor_id, sched, neigh);
        let msg = p.handle();
        let st = p.stealer();

        let hdl = Builder::new()
                      .name(format!("Processor #{}", processor_id))
                      .spawn(move || {
                          Processor::set_tls(&mut p);
                          let err = p.schedule();
                          Processor::unset_tls();

                          if let Err(err) = err {
                              panic!("Processor::schedule return Err: {:?}", err);
                          }
                      })
                      .unwrap();

        (hdl, msg, st)
    }

    #[inline]
    pub fn run_main<M, T>(processor_id: usize,
                          sched: *mut Scheduler,
                          f: M)
                          -> (thread::JoinHandle<()>,
                              Sender<ProcMessage>,
                              Stealer<SendableCoroutinePtr>,
                              ::std::sync::mpsc::Receiver<Result<T, Box<Any + Send + 'static>>>)
        where M: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        let mut p = Processor::new_with_neighbors(processor_id, sched, Vec::new());
        let (msg, st) = (p.handle(), p.stealer());
        let (tx, rx) = ::std::sync::mpsc::channel();

        let hdl = Builder::new().name(format!("Processor #{}", processor_id)).spawn(move || {
            Processor::set_tls(&mut p);

            let wrapper = move || {
                let ret = unsafe { ::try(move || f()) };

                // No matter whether it is panicked or not, the result will be sent to the channel
                let _ = tx.send(ret); // Just ignore if it failed
            };
            p.spawn_opts(Box::new(wrapper), Options::default());

            let err = p.schedule();
            Processor::unset_tls();

            if let Err(err) = err {
                panic!("Processor::schedule return Err: {:?}", err);
            }
        }).unwrap();

        (hdl, msg, st, rx)
    }

    #[inline]
    pub fn scheduler(&self) -> &Scheduler {
        unsafe { &*self.scheduler }
    }

    /// Get the thread local processor
    #[inline]
    pub fn current() -> Option<&'static mut Processor> {
        PROCESSOR.with(|proc_opt| unsafe {
            let p: *mut Processor = *proc_opt.get();

            if p.is_null() {
                None
            } else {
                Some(&mut *p)
            }
        })
    }

    #[inline]
    // Get the current running coroutine
    pub fn running(&mut self) -> Option<*mut Coroutine> {
        self.cur_running
    }

    #[inline]
    // Get the current running coroutine
    pub fn current_running() -> Option<*mut Coroutine> {
        Processor::current().and_then(Processor::running)
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
    pub fn ready(&mut self, coro_ptr: *mut Coroutine) {
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
    fn run_with_all_local_tasks(&mut self, coro_ptr: *mut Coroutine) {
        let mut hdl = coro_ptr;
        loop {
            let is_suspended = match self.resume(hdl) {
                Ok(State::Suspended) => true,
                Ok(State::Finished) | Err(..) => {
                    Scheduler::finished(hdl);
                    false
                }
                Ok(..) => false,
            };

            // Try to fetch one task from the local queue
            match self.queue_worker.pop() {
                Some(h) => {
                    if is_suspended {
                        // If the current task has to be suspended, then
                        // push it back to the local work queue
                        self.ready(hdl);
                    }
                    hdl = h.0;
                }
                None => {
                    // Work queue is empty
                    if !is_suspended {
                        // Current task is blocked, just break the loop
                        break;
                    }
                    // Resume current task
                }
            }
        }
    }

    /// Run the processor
    fn schedule(&mut self) -> io::Result<()> {
        self.is_scheduling = true;

        'outerloop: loop {
            // 1. Run all tasks in local queue
            if let Some(hdl) = self.queue_worker.pop() {
                self.run_with_all_local_tasks(hdl.0);
            } else {
                self.has_ready_tasks = false;
            }

            // 2. Check the mainbox
            while let Ok(msg) = self.chan_receiver.try_recv() {
                match msg {
                    ProcMessage::NewNeighbor(nei) => self.neighbor_stealers.push(nei),
                    ProcMessage::Shutdown => {
                        self.destroy_all_coroutines();
                    }
                    ProcMessage::Ready(SendableCoroutinePtr(ptr)) => {
                        self.ready(ptr);
                        self.has_ready_tasks = true;
                    }
                }
            }

            if self.is_exiting {
                self.destroy_all_coroutines();
                break 'outerloop;
            }

            if self.has_ready_tasks {
                continue 'outerloop;
            }

            // 3. Randomly steal from neighbors
            let rand_idx = self.rng.gen::<usize>();
            let total_stealers = self.neighbor_stealers.len();
            for idx in (0..self.neighbor_stealers.len()).map(|x| (x + rand_idx) % total_stealers) {
                if let Stolen::Data(SendableCoroutinePtr(hdl)) = self.neighbor_stealers[idx]
                                                                     .steal() {
                    self.run_with_all_local_tasks(hdl);
                    continue 'outerloop;
                }
            }

            // This sleep throttles this loop in case a Processor runs out of work.
            // TODO: Replace this sleep by some mechanism capable of waking up
            //       idle workers as soon as new work is available.
            thread::sleep(Duration::from_millis(10));
        }

        self.is_scheduling = false;

        Ok(())
    }

    fn destroy_all_coroutines(&mut self) {
        // Not epoll
        self.is_exiting = true;

        // 1. Drain the work queue.
        if let Some(hdl) = self.queue_worker.pop() {
            self.run_with_all_local_tasks(hdl.0);
        }
    }

    #[inline]
    pub fn resume(&mut self, coro_ptr: *mut Coroutine) -> coroutine::Result<State> {
        self.cur_running = Some(coro_ptr);
        unsafe {
            let _guard = (*coro_ptr).yield_lock.lock().unwrap();
            self.main_coro.yield_to(&mut *coro_ptr);
        }

        match self.last_result.take() {
            None => Ok(State::Suspended),
            Some(r) => r,
        }
    }

    /// Suspended the current running coroutine, equivalent to `Scheduler::sched`
    #[inline]
    pub fn sched(&mut self) {
        match self.cur_running.take() {
            None => {}
            Some(coro_ptr) => unsafe {
                self.set_last_result(Ok(State::Suspended));
                (&mut *coro_ptr).yield_to(&*self.main_coro)
            },
        }

        // We are back!
        // Exit right now!
        if self.is_exiting {
            panic!(ForceUnwind);
        }
    }

    /// Block the current running coroutine, equivalent to `Scheduler::block`
    #[inline]
    pub fn block(&mut self) {
        match self.cur_running.take() {
            None => {}
            Some(coro_ptr) => unsafe {
                self.set_last_result(Ok(State::Blocked));
                (&mut *coro_ptr).yield_to(&*self.main_coro)
            },
        }

        // We are back!
        // Exit right now!
        if self.is_exiting {
            panic!(ForceUnwind);
        }
    }

    /// Yield the current running coroutine with specified result
    #[inline]
    pub fn yield_with(&mut self, r: coroutine::Result<State>) {
        match self.cur_running.take() {
            None => {}
            Some(coro_ptr) => unsafe {
                self.set_last_result(r);
                (&mut *coro_ptr).yield_to(&*self.main_coro)
            },
        }
    }
}

pub enum ProcMessage {
    NewNeighbor(Stealer<SendableCoroutinePtr>),
    Ready(SendableCoroutinePtr),
    Shutdown,
}
