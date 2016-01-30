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
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SendError};
use std::thread::{self, Builder, Thread};
use std::time::Duration;

use deque::{self, Worker, Stealer, Stolen};
use rand;

use coroutine::{Coroutine, State, Handle};
use options::Options;
use scheduler::Scheduler;

thread_local!(static PROCESSOR: UnsafeCell<Option<Processor>> = UnsafeCell::new(None));

#[derive(Clone)]
pub struct ProcMessageSender {
    inner: Sender<ProcMessage>,
    processor: Arc<ProcessorInner>,
}

impl ProcMessageSender {
    pub fn send(&self, proc_msg: ProcMessage) -> Result<(), SendError<ProcMessage>> {
        try!(self.inner.send(proc_msg));
        self.processor.try_wake_up();
        Ok(())
    }
}

unsafe impl Send for ProcMessageSender {}
unsafe impl Sync for ProcMessageSender {}

#[derive(Clone)]
pub struct Processor {
    inner: Arc<ProcessorInner>,
}

unsafe impl Send for Processor {}
unsafe impl Sync for Processor {}

/// Processing unit of a thread
pub struct ProcessorInner {
    weak_self: WeakProcessor,
    scheduler: *mut Scheduler,

    // Stores the context of the Processor::schedule() loop.
    main_coro: Handle,

    // NOTE: ONLY to be used by resume() and take_current_coroutine().
    current_coro: Option<Handle>,

    rng: rand::XorShiftRng,
    queue_worker: Worker<Handle>,
    queue_stealer: Stealer<Handle>,
    neighbor_stealers: Vec<Stealer<Handle>>, // TODO: make it a Arc<Vec<>>
    take_coro_cb: Option<&'static mut FnMut(Handle)>,

    chan_sender: Sender<ProcMessage>,
    chan_receiver: Receiver<ProcMessage>,

    thread_handle: Option<Thread>,
    should_wake_up: AtomicBool,
}

impl ProcessorInner {
    fn try_wake_up(&self) {
        // This flag should always set to true when we have job to do
        self.should_wake_up.store(true, Ordering::SeqCst);
        self.thread_handle.as_ref().map(|x| x.unpark());
    }
}

impl Processor {
    fn new_with_neighbors(sched: *mut Scheduler, neigh: Vec<Stealer<Handle>>) -> Processor {
        let (worker, stealer) = deque::new();
        let (tx, rx) = mpsc::channel();

        let mut p = Processor {
            inner: Arc::new(ProcessorInner {
                weak_self: unsafe { mem::zeroed() },
                scheduler: sched,

                main_coro: unsafe { Coroutine::empty() },
                current_coro: None,

                rng: rand::weak_rng(),
                queue_worker: worker,
                queue_stealer: stealer,
                neighbor_stealers: neigh,
                take_coro_cb: None,

                chan_sender: tx,
                chan_receiver: rx,

                thread_handle: None,
                should_wake_up: AtomicBool::new(false),
            }),
        };

        {
            let weak_self = WeakProcessor { inner: Arc::downgrade(&p.inner) };
            let inner = p.deref_mut();
            mem::forget(mem::replace(&mut inner.weak_self, weak_self));
        }

        p
    }

    fn set_tls(p: &Processor) {
        PROCESSOR.with(|proc_opt| unsafe {
            // HACK: Wohooo!
            let proc_opt = &mut *proc_opt.get();
            *proc_opt = Some(p.clone());
        })
    }

    pub fn run_with_neighbors(processor_id: usize,
                              sched: *mut Scheduler,
                              neigh: Vec<Stealer<Handle>>)
                              -> (thread::JoinHandle<()>, ProcMessageSender, Stealer<Handle>) {
        let mut p = Processor::new_with_neighbors(sched, neigh);
        let msg = p.handle();
        let st = p.stealer();

        let hdl = Builder::new()
                      .name(format!("Processor #{}", processor_id))
                      .spawn(move || {
                          Processor::set_tls(&mut p);
                          p.thread_handle = Some(thread::current());
                          p.schedule();
                      })
                      .unwrap();

        (hdl, msg, st)
    }

    pub fn run_main<M, T>(processor_id: usize,
                          sched: *mut Scheduler,
                          f: M)
                          -> (thread::JoinHandle<()>,
                              ProcMessageSender,
                              Stealer<Handle>,
                              ::std::sync::mpsc::Receiver<Result<T, Box<Any + Send + 'static>>>)
        where M: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        let mut p = Processor::new_with_neighbors(sched, Vec::new());
        let (msg, st) = (p.handle(), p.stealer());
        let (tx, rx) = ::std::sync::mpsc::channel();

        let hdl =
            Builder::new()
                .name(format!("Processor #{}", processor_id))
                .spawn(move || {
                    Processor::set_tls(&mut p);

                    let wrapper = move || {
                        let ret = unsafe { ::try(move || f()) };

                        // If sending fails Scheduler::run()'s loop would never quit --> unwrap.
                        tx.send(ret).unwrap();
                    };
                    p.spawn_opts(Box::new(wrapper), Options::new().name("<main>".to_owned()));

                    p.schedule();
                })
                .unwrap();

        (hdl, msg, st, rx)
    }

    pub fn scheduler(&self) -> &Scheduler {
        unsafe { &*self.scheduler }
    }

    pub unsafe fn mut_ptr(&self) -> *mut Processor {
        mem::transmute(self)
    }

    /// Get the thread local processor.
    pub fn current<'a>() -> Option<&'a mut Processor> {
        PROCESSOR.with(|proc_opt| unsafe { (&mut *proc_opt.get()).as_mut() })
    }

    /// Obtains the currently running coroutine after setting it's state to Blocked.
    /// NOTE: DO NOT call any Scheduler or Processor method within the passed callback, other than ready().
    pub fn take_current_coroutine<U, F>(&mut self, f: F) -> U
        where F: FnOnce(Handle) -> U
    {
        debug_assert!(self.current_coro.is_some(), "No coroutine is running yet");

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

    pub fn handle(&self) -> ProcMessageSender {
        ProcMessageSender {
            inner: self.chan_sender.clone(),
            processor: self.inner.clone(),
        }
    }

    pub fn spawn_opts(&mut self, f: Box<FnBox()>, opts: Options) {
        let mut new_coro = Coroutine::spawn_opts(f, opts);
        new_coro.set_preferred_processor(Some(self.weak_self.clone()));
        self.ready(new_coro);
    }

    /// Run the processor
    fn schedule(&mut self) {
        self.main_coro.set_state(State::Running);

        'outerloop: loop {
            // 1. Run all tasks in local queue
            while let Some(hdl) = self.queue_worker.pop() {
                self.resume(hdl);
            }

            // 2. Check the mainbox
            {
                let mut resume_all_tasks = false;

                while let Ok(msg) = self.chan_receiver.try_recv() {
                    match msg {
                        ProcMessage::NewNeighbor(nei) => self.neighbor_stealers.push(nei),
                        ProcMessage::Shutdown => {
                            resume_all_tasks = true;
                        }
                        ProcMessage::Ready(mut coro) => {
                            coro.set_preferred_processor(Some(self.weak_self.clone()));
                            self.ready(coro);
                            resume_all_tasks = true;
                        }
                        ProcMessage::Exit => {
                            break 'outerloop;
                        }
                    }
                }

                // Prefer running own tasks before stealing --> "continue" from anew.
                if resume_all_tasks {
                    continue 'outerloop;
                }
            }

            loop {
                // 3. Randomly steal from neighbors as a last measure.
                // TODO: To improve cache locality foreign lists should be split in half or so instead.
                let rand_idx = self.rng.gen::<usize>();
                let total_stealers = self.neighbor_stealers.len();

                for idx in 0..total_stealers {
                    let idx = (rand_idx + idx) % total_stealers;

                    if let Stolen::Data(hdl) = self.neighbor_stealers[idx].steal() {
                        self.resume(hdl);
                        continue 'outerloop;
                    }
                }

                // Check once before park
                if self.should_wake_up.swap(false, Ordering::SeqCst) {
                    break;
                }

                thread::park_timeout(Duration::from_millis(100));

                // If we are waken up, then break this loop
                // otherwise, continue to steal jobs from the others
                if self.should_wake_up.swap(false, Ordering::SeqCst) {
                    break;
                }
            }
        }

        // Clean up
        while let Some(hdl) = self.queue_worker.pop() {
            drop(hdl);
        }
    }

    fn resume(&mut self, mut coro: Handle) {
        debug_assert!(coro.state() != State::Finished, "Cannot resume a finished coroutine");

        unsafe {
            let current_coro: *mut Coroutine = &mut *coro;

            self.current_coro = Some(coro);
            // self.main_coro.yield_to(State::Suspended, &mut *current_coro);
            self.raw_resume(&mut *current_coro);
        }

        let coro = self.current_coro.take().unwrap();

        match coro.state() {
            State::Suspended => {
                self.chan_sender.send(ProcMessage::Ready(coro)).unwrap();
            }
            State::Blocked => {
                self.take_coro_cb.take().unwrap()(coro);
            }
            State::Finished => {
                Scheduler::finished(coro);
            }
            s => {
                panic!("Impossible! The coroutine is yield with {:?}", s);
            }
        }
    }

    /// Enqueue a coroutine to be resumed as soon as possible (making it the head of the queue)
    pub fn ready(&mut self, coro: Handle) {
        self.queue_worker.push(coro);

        // Wake up the worker thread if it is parked
        self.try_wake_up();
    }

    /// Suspends the current running coroutine, equivalent to `Scheduler::sched`
    pub fn sched(&mut self) {
        self.yield_with(State::Suspended)
    }

    /// Yield the current running coroutine with specified result
    pub fn yield_with(&mut self, r: State) {
        unsafe {
            let main_coro: *mut Coroutine = &mut *self.main_coro;
            self.current_coro.as_mut().unwrap().yield_to(r, &mut *main_coro);
        }
    }

    #[doc(hidden)]
    pub unsafe fn raw_resume(&mut self, target: &mut Coroutine) {
        self.main_coro.yield_to(State::Suspended, target)
    }

    #[doc(hidden)]
    pub unsafe fn toggle_unwinding(&mut self, target: &mut Coroutine) {
        self.main_coro.set_state(State::Suspended);
        target.set_state(State::ForceUnwinding);
        self.main_coro.raw_yield_to(target);
    }
}

impl Deref for Processor {
    type Target = ProcessorInner;

    #[inline]
    fn deref(&self) -> &ProcessorInner {
        self.inner.deref()
    }
}

impl DerefMut for Processor {
    #[inline]
    fn deref_mut(&mut self) -> &mut ProcessorInner {
        unsafe { &mut *(self.inner.deref() as *const ProcessorInner as *mut ProcessorInner) }
    }
}

impl PartialEq for Processor {
    fn eq(&self, other: &Processor) -> bool {
        (self as *const Processor) == (other as *const Processor)
    }
}

impl Eq for Processor {}

// For coroutine.rs
#[derive(Clone)]
pub struct WeakProcessor {
    inner: Weak<ProcessorInner>,
}

unsafe impl Send for WeakProcessor {}
unsafe impl Sync for WeakProcessor {}

impl WeakProcessor {
    pub fn upgrade(&self) -> Option<Processor> {
        self.inner.upgrade().and_then(|p| Some(Processor { inner: p }))
    }
}

pub enum ProcMessage {
    /// Got a new spawned neighbor
    NewNeighbor(Stealer<Handle>),

    /// Got a new ready coroutine
    Ready(Handle),

    /// Ask the processor to shutdown, which will going to force unwind all pending coroutines.
    Shutdown,

    /// Exit the processor immediately.
    Exit,
}

#[cfg(test)]
mod test {
    use std::sync::{Arc, Mutex};
    use std::ops::Deref;

    use scheduler::Scheduler;

    // Scheduler::spawn() must push the new coroutine at the head of the runqueue.
    // Thus if we spawn a number of coroutines they will be executed in reverse order.
    // This test will make sure that this is the case.
    #[test]
    fn processor_sched_order() {
        Scheduler::new()
            .run(|| {
                //
                let results = Arc::new(Mutex::new(Vec::with_capacity(5)));
                let expected = vec![0, 3, 2, 1, 99];

                for i in 1..4 {
                    let results = results.clone();

                    Scheduler::spawn(move || {
                        let mut results = results.lock().unwrap();
                        results.push(i);
                    });
                }

                {
                    let mut results = results.lock().unwrap();
                    results.push(0);
                }

                Scheduler::sched();

                let mut results = results.lock().unwrap();
                results.push(99);

                assert_eq!(results.deref(), &expected);
            })
            .unwrap();
    }
}
