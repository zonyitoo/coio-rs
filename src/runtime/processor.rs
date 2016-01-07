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
use std::mem;
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, Builder};

use deque::{BufferPool, Stolen, Worker, Stealer};

use rand;

use coroutine::{Coroutine, State, Handle};
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

    // Stores the context of the Processor::schedule() loop.
    main_coro: Handle,

    // NOTE: ONLY to be used by resume() and take_current_coroutine().
    current_coro: Option<Handle>,

    // NOTE: ONLY to be used to communicate the result from yield_with() to resume().
    last_state: State,

    rng: rand::XorShiftRng,
    queue_worker: Worker<Handle>,
    queue_stealer: Stealer<Handle>,
    neighbor_stealers: Vec<Stealer<Handle>>,
    take_coro_cb: Option<&'static mut FnMut(Handle)>,

    chan_sender: Sender<ProcMessage>,
    chan_receiver: Receiver<ProcMessage>,

    is_exiting: bool,
}

unsafe impl Send for Processor {}

impl Processor {
    fn new_with_neighbors(_processor_id: usize,
                          sched: *mut Scheduler,
                          neigh: Vec<Stealer<Handle>>)
                          -> Processor {
        let main_coro = unsafe { Coroutine::empty() };

        let (worker, stealer) = BufferPool::new().deque();
        let (tx, rx) = mpsc::channel();

        Processor {
            scheduler: sched,

            main_coro: main_coro,
            current_coro: None,
            last_state: State::Suspended,

            rng: rand::weak_rng(),
            queue_worker: worker,
            queue_stealer: stealer,
            neighbor_stealers: neigh,
            take_coro_cb: None,

            chan_sender: tx,
            chan_receiver: rx,

            is_exiting: false,
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

    pub fn run_with_neighbors(processor_id: usize,
                              sched: *mut Scheduler,
                              neigh: Vec<Stealer<Handle>>)
                              -> (thread::JoinHandle<()>, Sender<ProcMessage>, Stealer<Handle>) {
        let mut p = Processor::new_with_neighbors(processor_id, sched, neigh);
        let msg = p.handle();
        let st = p.stealer();

        let hdl = Builder::new()
                      .name(format!("Processor #{}", processor_id))
                      .spawn(move || {
                          Processor::set_tls(&mut p);
                          p.schedule();
                          Processor::unset_tls();
                      })
                      .unwrap();

        (hdl, msg, st)
    }

    pub fn run_main<M, T>(processor_id: usize,
                          sched: *mut Scheduler,
                          f: M)
                          -> (thread::JoinHandle<()>,
                              Sender<ProcMessage>,
                              Stealer<Handle>,
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

            p.schedule();
            Processor::unset_tls();
        }).unwrap();

        (hdl, msg, st, rx)
    }

    pub fn scheduler(&self) -> &Scheduler {
        unsafe { &*self.scheduler }
    }

    /// Get the thread local processor
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

    /// Obtains the currently running coroutine after setting it's state to Blocked.
    /// NOTE: DO NOT call any Scheduler or Processor method within the passed callback, other than ready().
    pub fn take_current_coroutine<U, F>(&mut self, f: F) -> U
        where F: FnOnce(Handle) -> U
    {
        let mut f = Some(f);
        let mut r = None;

        {
            let mut cb = |coro: Handle| r = Some(f.take().unwrap()(coro));

            // NOTE: Circumvents the following problem:
            //   transmute called with differently sized types: &mut [closure ...] (could be 64 bits) to
            //   &'static mut core::ops::FnMut(Box<coroutine::Coroutine>) + 'static (could be 128 bits) [E0512]
            let cb_ref: &mut FnMut(Handle) = &mut cb;
            let cb_ref_static = unsafe { mem::transmute(cb_ref) };

            // Gets executed as soon as yield_with() returns in Processor::resume().
            self.take_coro_cb = Some(cb_ref_static);
            self.yield_with(State::Blocked);
        }

        r.unwrap()
    }

    pub fn stealer(&self) -> Stealer<Handle> {
        self.queue_stealer.clone()
    }

    pub fn handle(&self) -> Sender<ProcMessage> {
        self.chan_sender.clone()
    }

    pub fn spawn_opts(&mut self, f: Box<FnBox()>, opts: Options) {
        let new_coro = Coroutine::spawn_opts(f, opts);
        // NOTE: If Scheduler::spawn() is called we want to make
        // sure that the spawned coroutine is executed immediately.
        // TODO: Should we really do this?
        if self.current_coro.is_some() {
            // Circumvent borrowck
            let queue_worker = &self.queue_worker as *const Worker<Handle>;

            self.take_current_coroutine(|coro| unsafe {
                // queue_worker.push() inserts at the front of the queue.
                // --> Insert new_coro last to ensure that it's at the front of the queue.
                (&*queue_worker).push(coro);
                (&*queue_worker).push(new_coro);
            });
        } else {
            self.ready(new_coro);
        }
    }

    /// Run the processor
    fn schedule(&mut self) {
        'outerloop: loop {
            // 1. Run all tasks in local queue
            while let Some(hdl) = self.queue_worker.pop() {
                self.resume(hdl);
            }

            // NOTE: It's important that this block comes right after the loop above.
            // The chan_receiver loop below is the only place a Shutdown message can be received.
            // Right after receiving one it will continue the 'outerloop from the beginning,
            // resume() all coroutines in the queue_worker which will ForceUnwind
            // and after that we exit the 'outerloop here.
            if self.is_exiting {
                break;
            }

            // 2. Check the mainbox
            {
                let mut new_ready_tasks = false;

                while let Ok(msg) = self.chan_receiver.try_recv() {

                    match msg {
                        ProcMessage::NewNeighbor(nei) => self.neighbor_stealers.push(nei),
                        ProcMessage::Shutdown => {
                            self.is_exiting = true;
                            continue 'outerloop;
                        }
                        ProcMessage::Ready(hdl) => {
                            self.ready(hdl);
                            new_ready_tasks = true;
                        }
                    }
                }

                // Prefer running own tasks before stealing --> "continue" from anew.
                if new_ready_tasks {
                    continue;
                }
            }

            // 3. Randomly steal from neighbors as a last measure.
            let rand_idx = self.rng.gen::<usize>();
            let total_stealers = self.neighbor_stealers.len();

            for idx in 0..total_stealers {
                let idx = (rand_idx + idx) % total_stealers;

                if let Stolen::Data(hdl) = self.neighbor_stealers[idx].steal() {
                    self.resume(hdl);
                    continue 'outerloop;
                }
            }

            // Wait forever until we got notified
            if let Ok(msg) = self.chan_receiver.recv() {
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
            };
        }
    }

    fn resume(&mut self, coro: Handle) {
        self.current_coro = Some(coro);
        self.main_coro.yield_to(&self.current_coro.as_ref().unwrap());

        let coro = self.current_coro.take().unwrap();

        match self.last_state {
            State::Suspended => {
                self.ready(coro);
            }
            State::Blocked => {
                self.take_coro_cb.take().unwrap()(coro);
            }
            State::Finished => {
                Scheduler::finished(coro);
            }
        }
    }

    /// Enqueue a coroutine to be resumed as soon as possible (making it the head of the queue)
    pub fn ready(&mut self, coro: Handle) {
        self.queue_worker.push(coro);
    }

    /// Suspends the current running coroutine, equivalent to `Scheduler::sched`
    pub fn sched(&mut self) {
        self.yield_with(State::Suspended)
    }

    /// Yield the current running coroutine with specified result
    pub fn yield_with(&mut self, r: State) {
        self.last_state = r;
        self.current_coro.as_mut().unwrap().yield_to(&self.main_coro);

        // We are back!
        // Exit right now!
        if self.is_exiting {
            panic!(ForceUnwind);
        }
    }
}

pub enum ProcMessage {
    NewNeighbor(Stealer<Handle>),
    Ready(Handle),
    Shutdown,
}
