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
use std::cell::UnsafeCell;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SendError};
use std::thread::{self, Builder, Thread};
use std::time::Duration;
use std::fmt;

use deque::{self, Worker, Stealer, Stolen};
use rand;

use coroutine::{Coroutine, State, Handle};
use scheduler::Scheduler;
use options::Options;

thread_local!(static PROCESSOR: UnsafeCell<Option<Processor>> = UnsafeCell::new(None));

type BlockWithCallback<'a> = &'a mut FnMut(&mut Processor, Handle);

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

pub struct Machine {
    pub thread_handle: thread::JoinHandle<()>,
    pub processor_handle: ProcMessageSender,
    pub stealer: Stealer<Handle>,
}

/// Control handle for the Processor
pub struct ProcessorHandle(&'static mut Processor);

impl ProcessorHandle {
    #[inline]
    pub fn id(&self) -> usize {
        self.0.id()
    }

    /// Obtains the currently running coroutine after setting it's state to Parked.
    ///
    /// # Safety
    ///
    /// - *DO NOT* call any Scheduler/Processor methods within the callback, other than ready().
    /// - *DO NOT* drop the Coroutine within the callback.
    pub fn park_with<'scope, F>(self, f: F)
        where F: FnOnce(&mut Processor, Handle) + 'scope
    {
        let processor = self.0;

        debug_assert!(processor.current_coro.is_some(), "Coroutine is missing");

        // Create a data carrier to carry a static function pointer and the Some(callback).
        // The callback is finally executed in the Scheduler::resume() method.
        // TODO: Please clean me up! The Some() is redundant, etc.
        let mut f = Some(f);
        let mut carrier = Some((carrier_fn::<F> as usize, &mut f as *mut _ as usize));

        if let Some(ref mut coro) = processor.current_coro {
            trace!("Coroutine `{}` is going to be parked", coro.debug_name());
            coro.yield_with(State::Parked, &mut carrier as *mut _ as usize);
        }

        // This function will be called on the Processor's Context as a bridge
        fn carrier_fn<F>(data: usize, p: &mut Processor, coro: Handle)
            where F: FnOnce(&mut Processor, Handle)
        {
            // Take out the callback function object from the Coroutine's stack
            let f = unsafe { (&mut *(data as *mut Option<F>)).take().unwrap() };
            f(p, coro);
        }
    }

    /// Yield the current coroutine
    #[inline]
    pub fn sched(self) {
        self.0.sched()
    }

    #[inline]
    pub fn handle(&self) -> ProcMessageSender {
        self.0.handle()
    }

    #[inline]
    pub fn scheduler(&self) -> &Scheduler {
        self.0.scheduler()
    }

    #[inline]
    pub fn ready(&mut self, coroutine: Handle) {
        self.0.ready(coroutine)
    }

    #[inline]
    pub fn current(&mut self) -> Option<&mut Handle> {
        self.0.current_coroutine()
    }

    pub fn spawn_opts<F>(&mut self, f: F, opts: Options)
        where F: FnOnce() + Send + 'static
    {
        let mut new_coro = Coroutine::spawn_opts(f, opts);
        new_coro.set_preferred_processor(Some(self.0.weak_self().clone()));
        new_coro.attach(unsafe { self.0.scheduler().work_counter() });
        self.ready(new_coro);
    }
}

#[derive(Clone)]
pub struct Processor {
    inner: Arc<ProcessorInner>,
}

unsafe impl Send for Processor {}

impl fmt::Debug for Processor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Processor(#{})", self.id())
    }
}

type TakeCoroutineCallback<'a> = &'a mut FnMut(&mut Processor, Handle);

/// Processing unit of a thread
pub struct ProcessorInner {
    id: usize,

    weak_self: WeakProcessor,
    scheduler: *mut Scheduler,

    // NOTE: ONLY to be used by resume() and park_with().
    current_coro: Option<Handle>,

    rng: rand::XorShiftRng,
    queue_worker: Worker<Handle>,
    queue_stealer: Stealer<Handle>,
    neighbor_stealers: Vec<Stealer<Handle>>, // TODO: make it a Arc<Vec<>>

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
    pub fn new(sched: *mut Scheduler, processor_id: usize) -> Machine {
        let (worker, stealer) = deque::new();
        let (tx, rx) = mpsc::channel();

        let mut p = Processor {
            inner: Arc::new(ProcessorInner {
                id: processor_id,

                weak_self: unsafe { mem::zeroed() },
                scheduler: sched,

                current_coro: None,

                rng: rand::weak_rng(),
                queue_worker: worker,
                queue_stealer: stealer,
                neighbor_stealers: Vec::new(),

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

        let processor_handle = p.handle();
        let stealer = p.stealer();

        let thread_handle = Builder::new()
                                .name(format!("Processor#{}", processor_id))
                                .spawn(move || {
                                    PROCESSOR.with(|proc_opt| unsafe {
                                        let proc_opt = &mut *proc_opt.get();
                                        *proc_opt = Some(p.clone());
                                    });

                                    p.schedule();
                                })
                                .unwrap();

        Machine {
            thread_handle: thread_handle,
            processor_handle: processor_handle,
            stealer: stealer,
        }
    }

    #[inline]
    pub fn scheduler(&self) -> &Scheduler {
        unsafe { &*self.scheduler }
    }

    /// Get the thread local processor.
    pub fn current() -> Option<ProcessorHandle> {
        Processor::instance().map(ProcessorHandle)
    }

    pub fn instance<'a>() -> Option<&'a mut Processor> {
        PROCESSOR.with(|proc_opt| unsafe { (&mut *proc_opt.get()).as_mut() })
    }

    pub fn current_coroutine(&mut self) -> Option<&mut Handle> {
        self.current_coro.as_mut()
    }

    #[inline]
    pub fn weak_self(&self) -> &WeakProcessor {
        &self.weak_self
    }

    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    #[inline]
    pub fn stealer(&self) -> Stealer<Handle> {
        self.queue_stealer.clone()
    }

    #[inline]
    pub fn handle(&self) -> ProcMessageSender {
        ProcMessageSender {
            inner: self.chan_sender.clone(),
            processor: self.inner.clone(),
        }
    }

    /// Run the processor
    fn schedule(&mut self) {
        trace!("{:?} starts", self);

        'outerloop: loop {
            // 1. Run all tasks in local queue
            while let Some(hdl) = self.queue_worker.pop() {
                self.resume(hdl);
            }

            // 2. Check the mainbox
            loop {
                {
                    let mut resume_all_tasks = false;

                    while let Ok(msg) = self.chan_receiver.try_recv() {
                        match msg {
                            ProcMessage::NewNeighbor(nei) => self.neighbor_stealers.push(nei),
                            ProcMessage::Shutdown => {
                                trace!("{:?} got ProcMessage::Shutdown signal", self);
                                break 'outerloop;
                            }
                            ProcMessage::Ready(mut coro) => {
                                coro.set_preferred_processor(Some(self.weak_self.clone()));
                                self.ready(coro);
                                resume_all_tasks = true;
                            }
                        }
                    }

                    // Prefer running own tasks before stealing --> "continue" from anew.
                    if resume_all_tasks {
                        continue 'outerloop;
                    }
                }

                // 3. Randomly steal from neighbors as a last measure.
                // TODO: To improve cache locality foreign lists
                //       should be split in half or so instead.
                let rand_idx = self.rng.gen::<usize>();
                let total_stealers = self.neighbor_stealers.len();

                for idx in 0..total_stealers {
                    let idx = (rand_idx + idx) % total_stealers;

                    if let Stolen::Data(hdl) = self.neighbor_stealers[idx].steal() {
                        trace!("{:?} steals Coroutine `{}` from neighbor[{}]",
                               self,
                               hdl.debug_name(),
                               idx);
                        self.resume(hdl);
                        continue 'outerloop;
                    }
                }

                // Check once before park
                if self.should_wake_up.swap(false, Ordering::SeqCst) {
                    break;
                }

                thread::park_timeout(Duration::from_millis(1));

                // If we are waken up, then break this loop
                // otherwise, continue to steal jobs from the others
                if self.should_wake_up.swap(false, Ordering::SeqCst) {
                    break;
                }
            }
        }

        trace!("{:?} dropping all pending Coroutines in the channel", self);
        while let Ok(msg) = self.chan_receiver.try_recv() {
            match msg {
                ProcMessage::Ready(coro) => {
                    trace!("Coroutine `{}` is dropping in {:?}",
                           coro.debug_name(),
                           self);
                    drop(coro);
                }
                _ => {}
            }
        }

        trace!("{:?} dropping all pending Coroutines in the work queue",
               self);
        // Clean up
        while let Some(hdl) = self.queue_worker.pop() {
            trace!("Coroutine `{}` is dropping in {:?}", hdl.debug_name(), self);
            drop(hdl);
        }

        trace!("{:?} is shutdown", self);
    }

    fn resume(&mut self, coro: Handle) {
        debug_assert!(!coro.is_finished(), "Cannot resume a finished coroutine");

        trace!("Resuming Coroutine `{}` in {:?}", coro.debug_name(), self);
        let data = {
            // let current_coro: *mut Coroutine = &mut *coro;
            self.current_coro = Some(coro);
            // (&mut *current_coro).resume()
            if let Some(ref mut c) = self.current_coro {
                c.resume(0)
            } else {
                0
            }
        };

        match self.current_coro.take() {
            Some(coro) => {
                if !coro.is_finished() {
                    trace!("Coroutine `{}` yield with state {:?}",
                           coro.debug_name(),
                           coro.state());

                    match coro.state() {
                        State::Suspended => {
                            self.chan_sender.send(ProcMessage::Ready(coro)).unwrap();
                        }
                        State::Parked => {
                            if data != 0 {
                                // Take out the data carrier
                                let carrier = unsafe {
                                    (&mut *(data as *mut Option<(usize, usize)>)).take().unwrap()
                                };

                                // Transmute the first item of the tuple back to the bridge function
                                let function: fn(usize, &mut Processor, Handle) = unsafe {
                                    mem::transmute(carrier.0)
                                };

                                // The function is a global generic function, so it is safe to
                                // call it even if the Coroutine is dropped inside its body.
                                function(carrier.1, self, coro);
                            }
                        }
                        s => {
                            panic!("Coroutine yielded with invalid state {:?}", s);
                        }
                    }
                } else {
                    trace!("A Coroutine is finished and going to be dropped right now");
                }
            }
            None => {
                panic!("The current Coroutine handle is taken out somewhere else");
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
        if let Some(coro) = self.current_coro.as_mut() {
            coro.yield_with(r, 0);
        }
    }
}

impl Deref for Processor {
    type Target = ProcessorInner;

    fn deref(&self) -> &ProcessorInner {
        self.inner.deref()
    }
}

impl DerefMut for Processor {
    fn deref_mut(&mut self) -> &mut ProcessorInner {
        unsafe { &mut *(self.inner.deref() as *const ProcessorInner as *mut ProcessorInner) }
    }
}

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
