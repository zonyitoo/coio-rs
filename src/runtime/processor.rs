// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Processing unit of a thread

use std::boxed::FnBox;
use std::cell::UnsafeCell;
use std::fmt;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SendError, Sender};
use std::sync::{Arc, Barrier, Weak};
use std::thread::{self, Builder};

use rand::rngs::SmallRng;
use rand::{FromEntropy, Rng};

use coroutine::{Coroutine, Handle, State};
use options::Options;
use runtime::stack_pool::StackPool;
use scheduler::Scheduler;

pub const QUEUE_SIZE: usize = 256;

thread_local!(static PROCESSOR: UnsafeCell<Option<Processor>> = UnsafeCell::new(None));

// type BlockWithCallback<'a> = &'a mut FnMut(&mut Processor, Handle);

#[derive(Clone)]
pub struct ProcMessageSender {
    inner: Sender<ProcMessage>,
    _processor: Processor,
}

impl ProcMessageSender {
    pub fn send(&self, proc_msg: ProcMessage) -> Result<(), SendError<ProcMessage>> {
        try!(self.inner.send(proc_msg));
        Ok(())
    }
}

unsafe impl Send for ProcMessageSender {}
unsafe impl Sync for ProcMessageSender {}

pub struct Machine {
    pub processor_handle: ProcMessageSender,
    pub processor: Processor,
    pub thread_handle: thread::JoinHandle<()>,
}

/// Control handle for the Processor
///
/// This wrapper struct is necessary to ensure safe usage with some operations. For instance:
/// `park_with()` will park the current Coroutine running on a certain Processor.
/// When the Coroutine is resumed later on it is not guaranteed that it's still
/// running on the previous Processor. The same thing is true for `sched()`.
/// In both cases one is forced to acquire a new `ProcessorHandle`.
///
/// Related issue: https://github.com/zonyitoo/coio-rs/issues/26
pub struct ProcessorHandle(&'static mut Processor);

impl ProcessorHandle {
    #[inline]
    pub fn id(&self) -> usize {
        self.0.id()
    }

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

    #[inline]
    pub fn spawn_opts<F: FnOnce() + Send + 'static>(&mut self, f: F, opts: Options) {
        self.spawn_opts_imp(Box::new(f), opts)
    }

    pub fn spawn_opts_imp(&mut self, f: Box<FnBox()>, opts: Options) {
        let new_coro = Coroutine::spawn_opts_with_pool(f, opts, self.stack_pool());
        self.ready(new_coro);
        self.scheduler().unpark_processor_maybe(1);
    }

    /// Obtains the currently running coroutine after setting it's state to Parked.
    ///
    /// # Safety
    ///
    /// - *DO NOT* call any Scheduler/Processor methods within the callback, other than ready().
    /// - *DO NOT* drop the Coroutine within the callback.
    ///   Tracking issues:
    ///       - https://github.com/zonyitoo/coio-rs/issues/44
    ///       - https://github.com/zonyitoo/coio-rs/issues/45
    pub fn park_with<'scope, F>(self, f: F)
    where
        F: FnOnce(&mut Processor, Handle) + 'scope,
    {
        let processor = self.0;

        debug_assert!(processor.current_coro.is_some(), "Coroutine is missing");

        // Create a data carrier to carry a static function pointer and the Some(callback).
        // The callback is finally executed in the Scheduler::resume() method.
        // TODO: Please clean me up! The Some() is redundant, etc.
        let mut f = Some(f);
        let mut carrier = Some((carrier_fn::<F> as usize, &mut f as *mut _ as usize));

        if let Some(ref mut coro) = processor.current_coro {
            trace!("{:?}: parking", coro);
            coro.yield_with(State::Parked, &mut carrier as *mut _ as usize);
        }

        // This function will be called on the Processor's Context as a bridge
        fn carrier_fn<F>(data: usize, p: &mut Processor, coro: Handle)
        where
            F: FnOnce(&mut Processor, Handle),
        {
            // Take out the callback function object from the Coroutine's stack
            let f = unsafe { (&mut *(data as *mut Option<F>)).take().unwrap() };
            f(p, coro);
        }
    }

    #[inline]
    pub fn stack_pool(&mut self) -> &mut StackPool {
        &mut self.0.stack_pool
    }
}

impl Eq for ProcessorHandle {}

impl PartialEq<Processor> for ProcessorHandle {
    #[inline]
    fn eq(&self, other: &Processor) -> bool {
        unsafe {
            let a: usize = mem::transmute_copy(self.0);
            let b: usize = mem::transmute_copy(other);
            a == b
        }
    }
}

impl PartialEq<ProcessorHandle> for ProcessorHandle {
    #[inline]
    fn eq(&self, other: &ProcessorHandle) -> bool {
        unsafe {
            let a: usize = mem::transmute_copy(self.0);
            let b: usize = mem::transmute_copy(other.0);
            a == b
        }
    }
}

impl fmt::Debug for ProcessorHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Processor(#{})", self.id())
    }
}

// type TakeCoroutineCallback<'a> = &'a mut FnMut(&mut Processor, Handle);

// The members `queue_head`, `queue_tail` and `queue` in the `Processor` struct
//  implement an atomic single-producer/multi-consumer ring buffer.
// The implementation was taken from Go's source code and
// originally written by the well known expert "Dmitry Vyukov".
// (See src/runtime/proc.go or commit 4722b1cbd3c734b67c0e3c1cd4458cdbd51e5844.)
//
// Related methods:
//   schedule() for the general scheduling logic
//   procresize() for creating and deleting Processors and their queues
//   runqput() and runqget() for pushing and shifting Handles
//   runqgrab() and runqsteal() for stealing Handles

/// Processing unit of a thread
pub struct ProcessorInner {
    id: usize,

    weak_self: WeakProcessor,
    scheduler: *mut Scheduler,

    chan_receiver: Receiver<ProcMessage>,
    chan_sender: Sender<ProcMessage>,

    /// The backing of the SPMC ring buffer forming the execution queue for the Processor
    ///
    /// The basic layout is:
    ///
    /// ```text
    ///     0      1     2     3       4           n-2      n-1
    /// [invalid, coro, coro, coro, invalid, ... invalid, invalid]
    ///            ^                   ^
    ///       queue_head          queue_tail
    /// ```
    ///
    /// Both head as well as tail are *only* incremented and *never* decremented.
    /// All access on the ring buffer will thus happen modulo to the size of the buffer.
    queue: [*mut Coroutine; QUEUE_SIZE],

    /// Points to the next element being removed by `queue_pop_front()`
    ///
    /// This member will be increased by the current and foreign threads.
    queue_head: AtomicUsize,

    /// Points to the slot where the next coroutine will be inserted in `queue_push_back()`
    ///
    /// This member will only be increased by the current thread,
    /// but might be read by foreign ones.
    queue_tail: AtomicUsize,

    // NOTE: current_coro is ONLY to be used by resume() and park_with().
    current_coro: Option<Handle>,
    rand_order: RandomProcessorOrder,
    rng: SmallRng,

    stack_pool: StackPool,
}

impl Processor {
    /// Spawns a new thread and runs a new Processor on it.
    pub fn spawn(
        sched: *mut Scheduler,
        processor_id: usize,
        barrier: Arc<Barrier>,
        max_stack_memory_limit: usize,
    ) -> Machine {
        let (tx, rx) = mpsc::channel();

        let mut p = Processor(Arc::new(UnsafeCell::new(ProcessorInner {
            id: processor_id,

            weak_self: unsafe { mem::zeroed() },
            scheduler: sched,

            chan_receiver: rx,
            chan_sender: tx,

            queue_head: AtomicUsize::new(0),
            queue_tail: AtomicUsize::new(0),
            queue: unsafe { mem::zeroed() },

            current_coro: None,
            rand_order: RandomProcessorOrder::new(),
            rng: SmallRng::from_entropy(),

            stack_pool: StackPool::new(Some(max_stack_memory_limit / 2), Some(max_stack_memory_limit)),
        })));

        {
            let weak_self = WeakProcessor(Arc::downgrade(&p.0));
            let inner = p.deref_mut();
            mem::forget(mem::replace(&mut inner.weak_self, weak_self));
        }

        let processor_handle = p.handle();
        let processor = p.clone();
        let thread_handle = {
            Builder::new()
                .name(format!("Processor#{}", processor_id))
                .stack_size(32 * 1024)
                .spawn(move || {
                    PROCESSOR.with(|proc_opt| unsafe {
                        let proc_opt = &mut *proc_opt.get();
                        *proc_opt = Some(p.clone());
                    });

                    barrier.wait();
                    p.schedule();
                }).unwrap()
        };

        Machine {
            processor_handle: processor_handle,
            processor: processor,
            thread_handle: thread_handle,
        }
    }

    /// Get the thread local processor.
    ///
    /// # Safety
    ///
    /// This method *is* thread safe.
    // NOTE:
    //   This is required to be inline(never) due to
    //   https://github.com/rust-lang/rust/commit/12c5fc5877f708e8e4df05bf834261f5237ac437
    #[inline(never)]
    pub fn current() -> Option<ProcessorHandle> {
        PROCESSOR
            .with(|proc_opt| unsafe { (&mut *proc_opt.get()) })
            .as_mut()
            .map(ProcessorHandle)
    }

    #[inline(never)]
    pub fn current_required() -> ProcessorHandle {
        Processor::current().expect("requires a Processor")
    }

    /// Returns the Scheduler associated with this instance.
    ///
    /// # Safety
    ///
    /// This method *is* thread safe.
    #[inline]
    pub fn scheduler(&self) -> &'static Scheduler {
        unsafe { &*self.scheduler }
    }

    /// Get a reference to the current Coroutine.
    ///
    /// # Safety
    ///
    /// This method *is not* thread safe.
    #[inline]
    pub fn current_coroutine(&mut self) -> Option<&mut Handle> {
        self.thread_assert();
        self.current_coro.as_mut()
    }

    /// Returns a reference to a weak self reference.
    ///
    /// # Safety
    ///
    /// This method *is* thread safe.
    #[inline]
    pub fn weak_self(&self) -> &WeakProcessor {
        &self.weak_self
    }

    /// Returns the Processor id.
    ///
    /// # Safety
    ///
    /// This method *is* thread safe.
    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    /// Returns the handle through which messages can be sent to this instance.
    pub fn handle(&self) -> ProcMessageSender {
        ProcMessageSender {
            inner: self.chan_sender.clone(),
            _processor: self.clone(),
        }
    }

    /// Enqueue a coroutine to be resumed as soon as possible (making it the head of the queue)
    pub fn ready(&mut self, coro: Handle) {
        // FIXME: Do not use self.current_coro here! Which will cause crash!!
        // if self.current_coro.is_none() {
        //     self.current_coro = Some(coro);
        // } else {
        //     self.queue_push_back(coro);
        // }
        //
        self.queue_push_back(coro);
    }

    /// Suspends the current running coroutine, equivalent to `Scheduler::sched`
    pub fn sched(&mut self) {
        self.yield_with(State::Suspended)
    }

    /// Yield the current running coroutine with specified result
    pub fn yield_with(&mut self, r: State) {
        if let Some(coro) = self.current_coroutine() {
            coro.yield_with(r, 0);
        }
    }

    // Helper method to ensure that private, thread unsafe methods are never
    // somehow called from foreign threads through public methods.
    #[cfg(debug_assertions)]
    fn thread_assert(&self) {
        if let Some(p) = Processor::current() {
            if p.id() == self.id {
                return;
            }
        }

        panic!("called a thread unsafe method from a foreign thread");
    }

    #[cfg(not(debug_assertions))]
    #[inline(always)]
    fn thread_assert(&self) {}

    fn queue_empty(&self) -> bool {
        self.queue_head.load(Ordering::Relaxed) == self.queue_tail.load(Ordering::Relaxed)
    }

    fn queue_pop_front(&mut self) -> Option<Handle> {
        self.thread_assert();

        loop {
            let h = self.queue_head.load(Ordering::Acquire);
            let t = self.queue_tail.load(Ordering::Relaxed);

            if t == h {
                return None;
            }

            let coro = unsafe { *self.queue.get_unchecked(h % QUEUE_SIZE) };

            if self
                .queue_head
                .compare_and_swap(h, h.wrapping_add(1), Ordering::Release)
                == h
            {
                let hdl = Some(unsafe { Handle::from_raw(coro) });
                trace!("{:?}: popped {:?} from local queue", self, hdl);
                return hdl;
            }
        }
    }

    fn queue_push_back(&mut self, hdl: Handle) {
        self.thread_assert();
        trace!("{:?}: pushing {:?} to local queue", self, hdl);

        let coro = hdl.into_raw();

        loop {
            let h = self.queue_head.load(Ordering::Acquire);
            let t = self.queue_tail.load(Ordering::Relaxed);

            if t.wrapping_sub(h) < QUEUE_SIZE {
                unsafe { *self.queue.get_unchecked_mut(t % QUEUE_SIZE) = coro };
                self.queue_tail.store(t.wrapping_add(1), Ordering::Release);
                return;
            }

            if self.queue_push_back_slow(coro, h, t) {
                return;
            }
        }
    }

    /// Only to be called by queue_push_back()
    #[cold]
    fn queue_push_back_slow(&mut self, coro: *mut Coroutine, h: usize, t: usize) -> bool {
        let mut batch: [*mut Coroutine; QUEUE_SIZE / 2 + 1] = unsafe { mem::uninitialized() };
        let n = t.wrapping_sub(h) / 2;

        assert!(n == QUEUE_SIZE / 2, "queue is not full");

        {
            let src = self.queue.as_ptr();
            let dst = batch.as_mut_ptr();

            for i in 0..n {
                unsafe {
                    let src = src.offset((h.wrapping_add(i) % QUEUE_SIZE) as isize);
                    let dst = dst.offset(i as isize);
                    ptr::copy_nonoverlapping(src, dst, 1);
                }
            }
        }

        if self
            .queue_head
            .compare_and_swap(h, h.wrapping_add(n), Ordering::Release)
            != h
        {
            return false;
        }

        batch[n] = coro;
        self.global_queue_put_batch(&batch[0..(n + 1)]);

        true
    }

    /// Steals half of the local queue from self and puts it into batch.
    ///
    /// This is the only queue* method accessing foreign ones.
    fn queue_grab(&mut self, batch: &mut [*mut Coroutine; QUEUE_SIZE], batch_tail: usize) -> usize {
        loop {
            let h = self.queue_head.load(Ordering::Acquire); // synchronize with other consumers
            let t = self.queue_tail.load(Ordering::Acquire); // synchronize with the producer
            let n = t.wrapping_sub(h).wrapping_add(1) / 2; // steal half (and make sure at least one is stolen)

            if n == 0 {
                return 0;
            }

            if n > QUEUE_SIZE / 2 {
                // read inconsistent h and t
                continue;
            }

            {
                let src = self.queue.as_ptr();
                let dst = batch.as_mut_ptr();

                for i in 0..n {
                    unsafe {
                        let src = src.offset((h.wrapping_add(i) % QUEUE_SIZE) as isize);
                        let dst = dst.offset((batch_tail.wrapping_add(i) % QUEUE_SIZE) as isize);
                        ptr::copy_nonoverlapping(src, dst, 1);
                    }
                }
            }

            if self
                .queue_head
                .compare_and_swap(h, h.wrapping_add(n), Ordering::Release)
                == h
            {
                return n;
            }
        }
    }

    /// Steals half of the local queue from `from` and puts it into the local queue.
    fn queue_steal(&mut self, from: &mut Processor) -> Option<Handle> {
        let t = self.queue_tail.load(Ordering::Relaxed);
        let n = from.queue_grab(&mut self.queue, t);

        if n == 0 {
            return None;
        }

        trace!("{:?}: stole {} Coroutines from {:?}", self, n, from);

        let n = n - 1;
        let coro = unsafe { *self.queue.get_unchecked(t.wrapping_add(n) % QUEUE_SIZE) };

        if n != 0 {
            // synchronize with consumers
            let h = self.queue_head.load(Ordering::Acquire);
            assert!(t.wrapping_sub(h).wrapping_add(n) < QUEUE_SIZE, "queue overflow");
            // makes the item available for consumption
            self.queue_tail.store(t.wrapping_add(n), Ordering::Release);
        }

        Some(unsafe { Handle::from_raw(coro) })
    }

    fn global_queue_put_batch(&self, batch: &[*mut Coroutine]) {
        self.thread_assert();
        trace!("{:?}: putting {} Coroutines to global", self, batch.len());

        let iter = batch.into_iter().map(|coro| unsafe { Handle::from_raw(*coro) });
        self.scheduler().push_global_queue_iter(iter);
    }

    fn global_queue_get_batch(&mut self) -> Option<Handle> {
        self.thread_assert();

        let scheduler = self.scheduler();

        if scheduler.global_queue_size() == 0 {
            return None;
        }

        let mut queue = scheduler.get_global_queue();

        if queue.len() == 0 {
            return None;
        }

        let mut n = (queue.len() / scheduler.get_machines().len()) + 1;

        if n > QUEUE_SIZE / 2 {
            n = QUEUE_SIZE / 2;
        }

        let hdl = queue.pop_front();

        let cnt = if hdl.is_some() {
            let h = self.queue_head.load(Ordering::Acquire);
            let t = self.queue_tail.load(Ordering::Relaxed);
            let max = (QUEUE_SIZE - t.wrapping_sub(h) + 1) / 2;
            let dst = self.queue.as_mut_ptr();

            if n > max {
                n = max;
            }

            let queue = {
                let q = queue.split_off_front(n);
                scheduler.set_global_queue_size(queue.len());
                drop(queue);
                q
            };

            let cnt = queue.len();

            for (i, hdl) in queue.into_iter().enumerate() {
                unsafe {
                    let dst = dst.offset((t.wrapping_add(i) % QUEUE_SIZE) as isize);
                    *dst = Handle::into_raw(hdl);
                }
            }

            if cnt > 0 {
                // makes the item available for consumption
                self.queue_tail.store(t.wrapping_add(cnt), Ordering::Release);
            }

            cnt + 1
        } else {
            0
        };

        trace!("{:?}: got {} Coroutines from global", self, cnt);

        hdl
    }

    fn fetch_foreign_coroutines(&mut self) -> Option<Handle> {
        // Randomly steal from neighbors
        {
            let machines = self.scheduler().get_machines();

            for _ in 0..4 {
                let rnd = self.rng.gen();

                for x in self.rand_order.iter(rnd) {
                    let hdl = self.queue_steal(&mut machines[x].processor);

                    if hdl.is_some() {
                        return hdl;
                    }
                }
            }
        }

        // Steal from the global queue
        {
            let hdl = self.global_queue_get_batch();

            if hdl.is_some() {
                return hdl;
            }
        }

        None
    }

    fn schedule(&mut self) {
        self.thread_assert();
        trace!("{:?}: local scheduler begin", self);

        let machine_len = self.scheduler().get_machines().len();
        let scheduler = self.scheduler();
        let mut run_next = None;

        self.rand_order.reset(machine_len);

        loop {
            if let Ok(ProcMessage::Shutdown(barrier)) = self.chan_receiver.try_recv() {
                trace!("{:?}: got shutdown signal", self);
                barrier.wait();
                break;
            }

            // TODO: Ensure that coroutines from foreign queues are fetched once in a while.

            // Run tasks in local queue
            if run_next.is_none() {
                run_next = self.queue_pop_front();
            }

            if run_next.is_none() {
                scheduler.inc_spinning();
                run_next = self.fetch_foreign_coroutines();
                scheduler.dec_spinning();
            }

            if let Some(hdl) = run_next {
                run_next = self.resume(hdl);
            } else {
                trace!("{:?}: parking", self);
                scheduler.park_processor(|| {
                    run_next = self.fetch_foreign_coroutines();
                    run_next.is_none()
                });
                trace!("{:?}: unparked", self);
            }
        }

        // NOTE:
        //   A Barrier is sent inside the ProcMessage::Shutdown and
        //   awaited as soon as the Shutdown message is received.
        //   This means that this point in schedule() outside of the main loop above is only
        //   reached when all other Processors have fully acknowledged the Shutdown message too.
        //   We can thus safely access any local members without synchronization.

        trace!("{:?}: dropping run_next", self);
        drop(run_next);

        trace!("{:?}: dropping local coroutines", self);
        while self.queue_head.load(Ordering::Relaxed) != self.queue_tail.load(Ordering::Relaxed) {
            // pop from tail of local queue
            let t = self.queue_tail.fetch_sub(1, Ordering::Relaxed) - 1;
            let _coro = unsafe { Handle::from_raw(*self.queue.get_unchecked(t % QUEUE_SIZE)) };
        }

        trace!("{:?}: local scheduler end", self);
    }

    fn resume(&mut self, coro: Handle) -> Option<Handle> {
        self.thread_assert();

        assert!(coro.is_finished() == false, "Cannot resume a finished coroutine");

        trace!("{:?}: resuming {:?}", self, coro);
        let data = {
            debug_assert!(self.current_coro.is_none(), "{:?} is still running!", self.current_coro);

            self.current_coro = Some(coro);

            if let Some(ref mut c) = self.current_coro {
                c.resume(0)
            } else {
                0
            }
        };

        trace!("{:?}: Coroutine yield back with {:?}", self, self.current_coro);

        let mut hdl = None;
        if let Some(coro) = self.current_coro.take() {
            trace!("{:?}: yielded with {:?}", &coro, coro.state());
            match coro.state() {
                State::Suspended => {
                    // If the currently suspended coroutine is the only local one
                    // we want to ensure that it's not immediately resumed.
                    // Thus we fetch foreign coroutines first and then put the
                    // suspended one into the local queue as the last one.
                    if self.queue_empty() {
                        hdl = self.fetch_foreign_coroutines()
                    }

                    self.queue_push_back(coro);
                }
                State::Parked => {
                    assert!(data != 0, "Coroutine parked with data == 0");
                    // Take out the data carrier
                    let carrier = unsafe { (&mut *(data as *mut Option<(usize, usize)>)).take().unwrap() };

                    // Transmute the first item of the tuple back to the bridge function
                    let function: fn(usize, &mut Processor, Handle) = unsafe { mem::transmute(carrier.0) };

                    // The function is a global generic function, so it is safe to
                    // call it even if the Coroutine is dropped inside its body.
                    function(carrier.1, self, coro);
                }
                State::Finished => {
                    trace!("{:?}: finished", coro);
                }
                s => {
                    panic!("Coroutine yielded with invalid state {:?}", s);
                }
            }
        }

        hdl
    }
}

#[derive(Clone)]
pub struct Processor(Arc<UnsafeCell<ProcessorInner>>);

unsafe impl Send for Processor {}

impl Deref for Processor {
    type Target = ProcessorInner;

    #[inline]
    fn deref(&self) -> &ProcessorInner {
        unsafe { &*self.0.get() }
    }
}

impl DerefMut for Processor {
    #[inline]
    fn deref_mut(&mut self) -> &mut ProcessorInner {
        unsafe { &mut *self.0.get() }
    }
}

impl Eq for Processor {}

impl PartialEq<Processor> for Processor {
    #[inline]
    fn eq(&self, other: &Processor) -> bool {
        unsafe {
            let a: usize = mem::transmute_copy(&self.0);
            let b: usize = mem::transmute_copy(&other.0);
            a == b
        }
    }
}

impl PartialEq<ProcessorHandle> for Processor {
    #[inline]
    fn eq(&self, other: &ProcessorHandle) -> bool {
        unsafe {
            let a: usize = mem::transmute_copy(&self.0);
            let b: usize = mem::transmute_copy(&other.0);
            a == b
        }
    }
}

impl fmt::Debug for Processor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Processor(#{})", self.id())
    }
}

// For coroutine.rs
#[derive(Clone)]
pub struct WeakProcessor(Weak<UnsafeCell<ProcessorInner>>);

impl WeakProcessor {
    pub fn upgrade(&self) -> Option<Processor> {
        self.0.upgrade().and_then(|p| Some(Processor(p)))
    }
}

pub enum ProcMessage {
    /// Ask the processor to shutdown, which will going to force unwind all pending coroutines.
    Shutdown(Arc<Barrier>),
}

// The following idea stems from Go:
// These are helper types for randomized work stealing.
// They allow to enumerate all Processors in different pseudo-random orders without repetitions.
// The algorithm is based on the fact that if we have X such that X and processors.len()
// are coprime, then a sequences of (i + X) % processors.len() gives the required enumeration.
struct RandomProcessorOrder {
    count: usize,
    coprimes: Vec<usize>,
}

impl RandomProcessorOrder {
    fn new() -> RandomProcessorOrder {
        RandomProcessorOrder {
            count: 0,
            coprimes: Vec::new(),
        }
    }

    fn reset(&mut self, count: usize) {
        fn gcd(mut a: usize, mut b: usize) -> usize {
            while b != 0 {
                let t = a % b;
                a = b;
                b = t;
            }

            a
        }

        self.count = count;
        self.coprimes.clear();
        self.coprimes.reserve(count);

        for i in 1..(count + 1) {
            if gcd(i, count) == 1 {
                self.coprimes.push(i)
            }
        }
    }

    fn iter(&self, start: usize) -> RandomProcessorIter {
        RandomProcessorIter {
            i: 0,
            pos: start % self.count,
            inc: self.coprimes[start % self.coprimes.len()],
            count: self.count,
        }
    }
}

struct RandomProcessorIter {
    i: usize,
    pos: usize,
    inc: usize,
    count: usize,
}

impl Iterator for RandomProcessorIter {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.i < self.count {
            let r = self.pos;
            self.i += 1;
            self.pos = (self.pos + self.inc) % self.count;
            Some(r)
        } else {
            None
        }
    }
}

impl ExactSizeIterator for RandomProcessorIter {
    fn len(&self) -> usize {
        self.count
    }
}

#[cfg(test)]
mod test {
    use std::ops::Deref;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::RandomProcessorOrder;
    use options::Options;
    use scheduler::Scheduler;

    // Scheduler::spawn() must push the new coroutine at the head of the runqueue.
    // Thus if we spawn a number of coroutines they will be executed in reverse order.
    // This test will make sure that this is the case.
    #[test]
    fn processor_sched_order() {
        Scheduler::new()
            .run(|| {
                //
                let results = Arc::new(Mutex::new(Vec::with_capacity(4)));
                let expected = vec![0, 1, 2, 3];

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

                let results = results.lock().unwrap();
                assert_eq!(results.deref(), &expected);
            }).unwrap();
    }

    #[test]
    #[ignore]
    fn processor_queue_overflow() {
        Scheduler::new()
            .run(move || {
                let counter = Arc::new(AtomicUsize::new(0));
                let mut opts = Options::new();

                opts.stack_size(32 * 1024);

                for _ in 0..300 {
                    let counter = counter.clone();
                    let f = move || {
                        counter.fetch_add(1, Ordering::SeqCst);
                    };
                    Scheduler::spawn_opts(f, opts.clone());
                }

                for _ in 0..2 {
                    Scheduler::sched();
                }

                assert_eq!(counter.load(Ordering::SeqCst), 300);
            }).unwrap();
    }

    #[test]
    fn random_processor_order() {
        let mut order = RandomProcessorOrder::new();
        order.reset(5);

        assert_eq!(order.iter(0).collect::<Vec<usize>>(), vec![0, 1, 2, 3, 4]);
        assert_eq!(order.iter(1).collect::<Vec<usize>>(), vec![1, 3, 0, 2, 4]);
        assert_eq!(order.iter(2).collect::<Vec<usize>>(), vec![2, 0, 3, 1, 4]);
        assert_eq!(order.iter(3).collect::<Vec<usize>>(), vec![3, 2, 1, 0, 4]);
        assert_eq!(order.iter(4).collect::<Vec<usize>>(), vec![4, 0, 1, 2, 3]);
        assert_eq!(order.iter(5).collect::<Vec<usize>>(), vec![0, 2, 4, 1, 3]);
    }
}
