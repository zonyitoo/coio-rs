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

use std::cell::UnsafeCell;
use std::fmt::Debug;
use std::io::{self, Write};
use std::mem;
use std::panic;
use std::sync::{Arc, Barrier, Condvar, Mutex, MutexGuard};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use std::ptr::Shared;

use mio::{Evented, EventLoop, EventSet, Handler, NotifyError, PollOpt, Sender, Token};
use slab::Slab;
use time;

use coroutine::{Coroutine, Handle, HandleList};
use join_handle::{self, JoinHandleReceiver};
use options::Options;
use sync::spinlock::Spinlock;

use runtime::processor::{self, Machine, Processor, ProcMessage};
use runtime::notifier::{Notifier, Waiter, WaiterState};
use runtime::timer::{Timer, Timeout, TimerError};

/// A handle that could join the coroutine
pub struct JoinHandle<T> {
    result: JoinHandleReceiver<T>,
}

unsafe impl<T: Send> Send for JoinHandle<T> {}

impl<T> JoinHandle<T> {
    /// Await completion of the coroutine and return it's result.
    pub fn join(self) -> thread::Result<T> {
        self.result.pop()
    }
}


type RegisterCallback<'a> = &'a mut FnMut(&mut EventLoop<Scheduler>, Token, ReadyStates) -> bool;
type DeregisterCallback<'a> = &'a mut FnMut(&mut EventLoop<Scheduler>);

#[doc(hidden)]
pub struct RegisterMessage {
    cb: RegisterCallback<'static>,
    coro: Handle,
}

impl RegisterMessage {
    #[inline]
    fn new(coro: Handle, cb: RegisterCallback) -> RegisterMessage {
        RegisterMessage {
            cb: unsafe { mem::transmute(cb) },
            coro: coro,
        }
    }
}

#[doc(hidden)]
pub struct DeregisterMessage {
    cb: DeregisterCallback<'static>,
    coro: Handle,
    token: Token,
}

impl DeregisterMessage {
    #[inline]
    fn new(coro: Handle, cb: DeregisterCallback, token: Token) -> DeregisterMessage {
        DeregisterMessage {
            cb: unsafe { mem::transmute(cb) },
            coro: coro,
            token: token,
        }
    }
}

// #[doc(hidden)]
// pub struct TimerMessage {
//     coro: Handle,
//     delay: u64,
//     result: *mut Result<(), TimerError>,
// }
//
// impl TimerMessage {
//     #[inline]
//     fn new(coro: Handle, delay: u64, result: &mut Result<(), TimerError>) -> TimerMessage {
//         TimerMessage {
//             coro: coro,
//             delay: delay,
//             result: result,
//         }
//     }
// }

#[doc(hidden)]
pub enum Message {
    Register(RegisterMessage),
    Deregister(DeregisterMessage),
    // Timer(TimerMessage),
    Shutdown,
}

unsafe impl Send for Message {}


#[doc(hidden)]
#[repr(usize)]
#[derive(Clone, Copy)]
pub enum ReadyType {
    Readable = 0,
    Writable,
    Error,
    Hup,
}

impl Into<EventSet> for ReadyType {
    fn into(self) -> EventSet {
        unsafe { mem::transmute(1usize << self as usize) }
    }
}

#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct ReadyStates(Arc<[Notifier; 2]>);

impl ReadyStates {
    #[inline]
    fn new() -> ReadyStates {
        ReadyStates(Arc::new([Notifier::default(), Notifier::default()]))
    }

    #[inline]
    pub fn wait(&self, ready_type: ReadyType) -> WaiterState {
        self.0[ready_type as usize].wait()
    }

    #[inline]
    pub fn wait_timeout(&self, ready_type: ReadyType, dur: Duration) -> WaiterState {
        self.0[ready_type as usize].wait_timeout(dur).unwrap()
    }

    #[inline]
    fn notify(&self, event_set: EventSet, handles: &mut HandleList) {
        if event_set.contains(EventSet::readable()) {
            self.0[ReadyType::Readable as usize].notify_all(WaiterState::Succeeded, handles);
        }

        if event_set.contains(EventSet::writable()) {
            self.0[ReadyType::Writable as usize].notify_all(WaiterState::Succeeded, handles);
        }

        if event_set.contains(EventSet::error() | EventSet::hup()) {
            self.0[ReadyType::Readable as usize].notify_all(WaiterState::Error, handles);
            self.0[ReadyType::Writable as usize].notify_all(WaiterState::Error, handles);
        }
    }
}

enum TimerWaitType {
    Handle(Handle),
    Waiter(Shared<Waiter>),
}

/// Coroutine scheduler
pub struct Scheduler {
    default_spawn_options: Options,
    expected_worker_count: usize,
    maximum_stack_memory_limit: usize,

    // Mio event loop handler
    event_loop_sender: Option<Sender<Message>>,
    slab: Slab<ReadyStates, usize>,
    timer: Spinlock<Timer<TimerWaitType>>,

    // NOTE:
    // This member is _used_ concurrently, but still deliberately used without any kind of locks.
    // The reason for this is that during runtime of the Scheduler the vector of Machines will
    // never change and thus it's contents are constant as long as any Processor is running.
    machines: UnsafeCell<Vec<Machine>>,

    idle_processor_condvar: Condvar,
    idle_processor_count: AtomicUsize,
    idle_processor_mutex: Mutex<()>,
    is_shutting_down: AtomicBool,
    spinning_processor_count: AtomicUsize,

    global_queue_size: AtomicUsize,
    global_queue: Mutex<HandleList>,
    io_handler_queue: HandleList,
}

impl Scheduler {
    /// Create a scheduler with default configurations
    pub fn new() -> Scheduler {
        Scheduler {
            default_spawn_options: Options::default(),
            expected_worker_count: 1,
            maximum_stack_memory_limit: 2 * 1024 * 1024 * 1024, // 2GB

            event_loop_sender: None,
            slab: Slab::new(1024),
            timer: Spinlock::new(Timer::new(100, 1_024, 65_536)),

            machines: UnsafeCell::new(Vec::new()),

            idle_processor_condvar: Condvar::new(),
            idle_processor_count: AtomicUsize::new(0),
            idle_processor_mutex: Mutex::new(()),
            is_shutting_down: AtomicBool::new(false),
            spinning_processor_count: AtomicUsize::new(0),

            global_queue_size: AtomicUsize::new(0),
            global_queue: Mutex::new(HandleList::new()),
            io_handler_queue: HandleList::new(),
        }
    }

    /// Set the number of workers
    pub fn with_workers(mut self, workers: usize) -> Scheduler {
        assert!(workers >= 1, "Must have at least one worker");
        self.expected_worker_count = workers;
        self
    }

    /// Set the default stack size
    pub fn default_stack_size(mut self, default_stack_size: usize) -> Scheduler {
        self.default_spawn_options.stack_size(default_stack_size);
        self
    }

    #[inline]
    pub fn work_count(&self) -> usize {
        ::global_work_count_get()
    }

    /// Run the scheduler
    pub fn run<F, T>(&mut self, f: F) -> thread::Result<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        trace!("setting custom panic hook");

        let default_handler = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            if let Some(mut p) = Processor::current() {
                if let Some(coro) = p.current() {
                    let mut stderr = io::stderr();
                    let name = match coro.name() {
                        Some(name) => name,
                        None => "<unnamed>",
                    };
                    let _ = write!(stderr, "Coroutine `{}` running in ", name);
                }
            }

            default_handler(panic_info);
        }));

        // Timer has to be setup before any kind of operations on it
        self.timer.lock().setup();

        trace!("creating EventLoop");

        let mut event_loop = EventLoop::new().unwrap();
        self.event_loop_sender = Some(event_loop.channel());

        let mut result = None;

        let cloned_event_loop_sender = event_loop.channel();
        {
            let result = unsafe { &mut *(&mut result as *mut _) };
            let wrapper = move || {
                let ret = panic::catch_unwind(panic::AssertUnwindSafe(f));

                *result = Some(ret);

                trace!("Coroutine(<main>) finished => sending Shutdown");
                let _ = cloned_event_loop_sender.send(Message::Shutdown);
            };

            let mut opt = self.default_spawn_options.clone();
            opt.name("<main>".to_owned());
            let main_coro = Coroutine::spawn_opts(Box::new(wrapper), opt);

            self.push_global_queue(main_coro);
        };

        let mut machines = unsafe { &mut *self.machines.get() };
        machines.reserve(self.expected_worker_count);

        trace!("spawning Machines");
        {
            let barrier = Arc::new(Barrier::new(self.expected_worker_count + 1));
            let mem = self.maximum_stack_memory_limit;

            for tid in 0..self.expected_worker_count {
                machines.push(Processor::spawn(self, tid, barrier.clone(), mem));
            }

            // After this Barrier unblocks we know that all Processors a fully spawned and
            // ready to call Processor::schedule(). This knowledge plus the fact that machines
            // is a static array after this point allows us to access that array without locks.
            barrier.wait();
        }

        trace!("running EventLoop");

        while event_loop.is_running() {
            let begin_time = time::now();
            let now = self.timer.lock().now();
            loop {
                match self.timer.lock().tick_to(now) {
                    Some(TimerWaitType::Handle(hdl)) => self.io_handler_queue.push_back(hdl),
                    Some(TimerWaitType::Waiter(waiter_ptr)) => {
                        let waiter = unsafe { &**waiter_ptr };
                        if let Some(hdl) = waiter.notify(WaiterState::Timedout) {
                            self.io_handler_queue.push_back(hdl);
                        }
                    }
                    None => break,
                }
            }

            self.append_io_handler_to_global_queue();

            let passed = time::now() - begin_time;
            let passed = passed.to_std().unwrap();

            let pending = match self.timer.lock().next_tick_in_ms() {
                Some(ms) => Some(::std::time::Duration::from_millis(ms)),
                None => {
                    let expected_sleep = ::std::time::Duration::new(0, 500_000);
                    if passed < expected_sleep {
                        Some(expected_sleep - passed)
                    } else {
                        None
                    }
                }
            };

            // FIXME: pending is ms, but expected_sleep is 500_000 nanosec
            event_loop.run_once(self,
                                pending.map(|pending| ::duration_to_ms(pending) as usize))
                      .unwrap();
        }

        trace!("EventLoop finished => sending Shutdown");
        {
            let barrier = Arc::new(Barrier::new(self.expected_worker_count));

            for m in machines.iter() {
                m.processor_handle.send(ProcMessage::Shutdown(barrier.clone())).unwrap();
            }
        }

        trace!("awaiting completion of Machines");
        {
            self.is_shutting_down.store(true, Ordering::SeqCst);
            self.idle_processor_condvar.notify_all();

            // NOTE: It's critical that all threads are joined since Processor
            // maintains a reference to this Scheduler using raw pointers.
            for m in machines.drain(..) {
                let _ = m.thread_handle.join();
            }
        }

        // Restore panic handler
        trace!("restoring default panic hook");
        panic::take_hook();

        result.unwrap()
    }

    /// Get the global Scheduler
    pub fn instance() -> Option<&'static Scheduler> {
        Processor::current().and_then(|p| unsafe { Some(mem::transmute(p.scheduler())) })
    }

    /// Get the global Scheduler
    pub fn instance_or_err() -> io::Result<&'static Scheduler> {
        Self::instance().ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Scheduler missing"))
    }

    /// Spawn a new coroutine with default options
    pub fn spawn<F, T>(f: F) -> JoinHandle<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        let opt = Scheduler::instance().unwrap().default_spawn_options.clone();
        Scheduler::spawn_opts(f, opt)
    }

    /// Spawn a new coroutine with options
    pub fn spawn_opts<F, T>(f: F, opts: Options) -> JoinHandle<T>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        let (tx, rx) = join_handle::handle_pair();
        let wrapper = move || {
            let ret = panic::catch_unwind(panic::AssertUnwindSafe(f));

            // No matter whether it is panicked or not, the result will be sent to the channel
            let _ = tx.push(ret);
        };
        let mut processor = Processor::current().expect("Processor required for spawn");
        processor.spawn_opts(wrapper, opts);

        JoinHandle { result: rx }
    }

    /// Suspend the current coroutine or thread
    pub fn sched() {
        trace!("Scheduler::sched()");

        match Processor::current() {
            Some(p) => p.sched(),
            None => thread::yield_now(),
        }
    }

    /// Block the current coroutine
    pub fn park_with<'scope, F>(f: F)
        where F: FnOnce(&mut Processor, Handle) + 'scope
    {
        Processor::current().map(|x| x.park_with(f)).unwrap()
    }

    /// A coroutine is ready for schedule
    #[doc(hidden)]
    pub fn ready(mut coro: Handle) {
        trace!("{:?}: readying", coro);

        if let Some(mut current) = Processor::current() {
            trace!("{:?}: pushing into local queue", coro);
            current.ready(coro);
            return;
        }

        // Resume it right here
        warn!("{:?}: resuming without processor", coro);
        coro.resume(0);
    }

    /// Block the current coroutine and wait for I/O event
    #[doc(hidden)]
    pub fn register<E>(&self, fd: &E, interest: EventSet) -> io::Result<(Token, ReadyStates)>
        where E: Evented + Debug
    {
        trace!("Scheduler: requesting register of {:?} for {:?}",
               fd,
               interest);

        let mut ret = Err(io::Error::from_raw_os_error(0));

        {
            let mut cb = |evloop: &mut EventLoop<Scheduler>, token, ready_states| {
                trace!("Scheduler: register of {:?} for {:?}", fd, interest);
                let r = evloop.register(fd, token, interest, PollOpt::edge());

                match r {
                    Ok(()) => {
                        ret = Ok((token, ready_states));
                        true
                    }
                    Err(err) => {
                        ret = Err(err);
                        false
                    }
                }
            };
            let cb = &mut cb as RegisterCallback;

            Scheduler::park_with(|_, coro| {
                let channel = self.event_loop_sender.as_ref().unwrap();
                let mut msg = Message::Register(RegisterMessage::new(coro, cb));

                while let Err(NotifyError::Full(m)) = channel.send(msg) {
                    msg = m;
                }
            });
        }

        ret
    }

    #[doc(hidden)]
    pub fn deregister<E>(&self, fd: &E, token: Token) -> io::Result<()>
        where E: Evented + Debug
    {
        trace!("Scheduler: requesting deregister of {:?}", fd);

        let mut ret = Ok(());

        {
            let mut cb = |evloop: &mut EventLoop<Scheduler>| {
                trace!("Scheduler: deregister of {:?}", fd);
                ret = evloop.deregister(fd);
            };
            let cb = &mut cb as DeregisterCallback;

            Scheduler::park_with(|_, coro| {
                let channel = self.event_loop_sender.as_ref().unwrap();
                let mut msg = Message::Deregister(DeregisterMessage::new(coro, cb, token));

                loop {
                    match channel.send(msg) {
                        Err(NotifyError::Full(m)) => msg = m,
                        _ => break,
                    }
                }
            });
        }

        ret
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep_ms(&self, delay: u64) -> Result<(), TimerError> {
        trace!("Scheduler: requesting sleep for {}ms", delay);

        let mut ret = Ok(());
        Scheduler::park_with(|_, coro| {
            let mut timer = self.timer.lock();
            if let Err(err) = timer.timeout_ms(TimerWaitType::Handle(coro), delay) {
                ret = Err(err);
            }
        });

        ret
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep(&self, delay: Duration) -> Result<(), TimerError> {
        self.sleep_ms(::duration_to_ms(delay))
    }

    /// IO timeouts
    #[doc(hidden)]
    pub fn timeout(&self, delay: u64, waiter: &mut Waiter) -> Result<Timeout, TimerError> {
        trace!("Scheduler: requesting timeout for {}ms", delay);

        let mut timer = self.timer.lock();
        timer.timeout_ms(TimerWaitType::Waiter(unsafe { Shared::new(waiter) }), delay)
    }

    /// IO cancel
    pub fn cancel_timeout(&self, timeout: Timeout) -> bool {
        trace!("Scheduler: requesting to cancel timeout");

        let mut timer = self.timer.lock();
        timer.clear(&timeout)
    }

    #[doc(hidden)]
    pub fn get_machines(&'static self) -> &mut [Machine] {
        unsafe { &mut *self.machines.get() }
    }

    #[doc(hidden)]
    pub fn get_global_queue(&self) -> MutexGuard<HandleList> {
        self.global_queue.lock().unwrap()
    }

    #[doc(hidden)]
    pub fn push_global_queue(&self, hdl: Handle) {
        let size = {
            let mut queue = self.get_global_queue();
            queue.push_back(hdl);
            let size = queue.len();
            self.set_global_queue_size(size);
            size
        };

        self.unpark_processors_with_queue_size(size);
    }

    #[doc(hidden)]
    pub fn push_global_queue_iter<T>(&self, iter: T)
        where T: IntoIterator<Item = Handle>
    {
        let size = {
            let mut queue = self.get_global_queue();
            queue.extend(iter);
            let size = queue.len();
            self.set_global_queue_size(size);
            size
        };

        self.unpark_processors_with_queue_size(size);
    }

    #[doc(hidden)]
    pub fn append_io_handler_to_global_queue(&mut self) {
        if !self.io_handler_queue.is_empty() {
            let size = {
                let mut queue = self.global_queue.lock().unwrap();
                queue.append(&mut self.io_handler_queue);
                let size = queue.len();
                self.set_global_queue_size(size);
                size
            };

            self.unpark_processors_with_queue_size(size);
        }
    }

    #[doc(hidden)]
    #[inline]
    pub fn global_queue_size(&self) -> usize {
        self.global_queue_size.load(Ordering::Relaxed)
    }

    #[doc(hidden)]
    #[inline]
    pub fn set_global_queue_size(&self, size: usize) {
        self.global_queue_size.store(size, Ordering::Relaxed)
    }

    #[doc(hidden)]
    #[inline]
    pub fn inc_spinning(&self) {
        self.spinning_processor_count.fetch_add(1, Ordering::Relaxed);
    }

    #[doc(hidden)]
    #[inline]
    pub fn dec_spinning(&self) {
        self.spinning_processor_count.fetch_sub(1, Ordering::Relaxed);
    }

    #[doc(hidden)]
    pub fn park_processor(&self) {
        self.idle_processor_count.fetch_add(1, Ordering::Relaxed);

        {
            let idle_processor_mutex = self.idle_processor_mutex.lock().unwrap();
            let _ = self.idle_processor_condvar.wait(idle_processor_mutex);
        }

        self.idle_processor_count.fetch_sub(1, Ordering::Relaxed);
    }

    #[doc(hidden)]
    pub fn unpark_processors_with_queue_size(&self, size: usize) {
        self.unpark_processor_maybe(size / (processor::QUEUE_SIZE / 2) + 1);
    }

    #[doc(hidden)]
    pub fn unpark_processor_maybe(&self, max: usize) {
        let idle_processor_count = self.idle_processor_count.load(Ordering::Relaxed);

        if max > 0 && idle_processor_count > 0 &&
           self.spinning_processor_count.load(Ordering::Relaxed) == 0 {
            let cnt = if idle_processor_count < max {
                idle_processor_count
            } else {
                max
            };

            for _ in 0..cnt {
                self.idle_processor_condvar.notify_one();
            }
        }
    }

    #[doc(hidden)]
    pub fn is_shutting_down(&self) -> bool {
        self.is_shutting_down.load(Ordering::Relaxed)
    }
}

unsafe impl Send for Scheduler {}

impl Handler for Scheduler {
    type Timeout = Token;
    type Message = Message;

    fn ready(&mut self, _event_loop: &mut EventLoop<Self>, token: Token, events: EventSet) {
        trace!("Handler: got {:?} for {:?}", events, token);

        let ready_states = self.slab.get(token.as_usize()).expect("Token must be registered");
        ready_states.notify(events, &mut self.io_handler_queue)
    }

    fn timeout(&mut self, _event_loop: &mut EventLoop<Self>, token: Token) {
        let coro = unsafe { Handle::from_raw(mem::transmute(token)) };
        trace!("Handler: timout for {:?}", coro);
        self.io_handler_queue.push_back(coro);
    }

    fn notify(&mut self, event_loop: &mut EventLoop<Self>, msg: Self::Message) {
        match msg {
            Message::Register(RegisterMessage { cb, coro }) => {
                trace!("Handler: registering for {:?}", coro);

                if self.slab.remaining() == 0 {
                    // doubles the size of the slab each time
                    let grow = self.slab.count();
                    self.slab.grow(grow);
                }

                self.slab.insert_with_opt(move |token| {
                    let token = unsafe { mem::transmute(token) };
                    let ready_states = ReadyStates::new();

                    if (cb)(event_loop, token, ready_states.clone()) {
                        Some(ready_states)
                    } else {
                        None
                    }
                });

                self.io_handler_queue.push_back(coro);
            }
            Message::Deregister(msg) => {
                trace!("Handler: deregistering for {:?}", msg.coro);

                let _ = self.slab.remove(unsafe { mem::transmute(msg.token) });

                (msg.cb)(event_loop);
                self.io_handler_queue.push_back(msg.coro);
            }
            // Message::Timer(msg) => {
            //     trace!("Handler: adding timer for {:?}", msg.coro);
            //
            //     let coro_ptr = Handle::into_raw(msg.coro);
            //     let token = unsafe { mem::transmute(coro_ptr) };
            //     let result = unsafe { &mut *msg.result };
            //
            //     if let Err(err) = event_loop.timeout_ms(token, msg.delay) {
            //         *result = Err(err);
            //         self.io_handler_queue.push_back(unsafe { Handle::from_raw(coro_ptr) });
            //     }
            // }
            Message::Shutdown => {
                trace!("Handler: shutting down");
                event_loop.shutdown();
            }
        }
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

                assert_eq!(guard.join().unwrap(), 1);
            })
            .unwrap();
    }
}
