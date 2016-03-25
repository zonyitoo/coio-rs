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

use std::any::Any;
use std::fmt::Debug;
use std::io::{self, Write};
use std::mem;
use std::panic;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use mio::{Evented, EventLoop, EventSet, Handler, NotifyError, PollOpt, Sender, TimerError, Token};
use slab::Slab;
use linked_hash_map::LinkedHashMap;

use coroutine::{Handle, Coroutine};
use join_handle::{self, JoinHandleReceiver};
use options::Options;
use runtime::processor::{Processor, ProcMessage, ProcMessageSender};
use sync::mono_barrier::CoroMonoBarrier;


/// A handle that could join the coroutine
pub struct JoinHandle<T> {
    result: JoinHandleReceiver<T>,
}

unsafe impl<T: Send> Send for JoinHandle<T> {}

impl<T> JoinHandle<T> {
    /// Await completion of the coroutine and return it's result.
    pub fn join(self) -> Result<T, Box<Any + Send + 'static>> {
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
    fn new<'a>(coro: Handle, cb: RegisterCallback<'a>) -> RegisterMessage {
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
    fn new<'a>(coro: Handle, cb: DeregisterCallback<'a>, token: Token) -> DeregisterMessage {
        DeregisterMessage {
            cb: unsafe { mem::transmute(cb) },
            coro: coro,
            token: token,
        }
    }
}

#[doc(hidden)]
pub struct TimerMessage {
    coro: Handle,
    delay: u64,
    result: *mut Result<(), TimerError>,
}

impl TimerMessage {
    #[inline]
    fn new(coro: Handle, delay: u64, result: &mut Result<(), TimerError>) -> TimerMessage {
        TimerMessage {
            coro: coro,
            delay: delay,
            result: result,
        }
    }
}

#[doc(hidden)]
pub enum Message {
    Register(RegisterMessage),
    Deregister(DeregisterMessage),
    Timer(TimerMessage),
    Shutdown,
}

unsafe impl Send for Message {}


#[doc(hidden)]
#[repr(usize)]
pub enum ReadyType {
    Readable = 0,
    Writable,
    Error,
    Hup,
}

#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct ReadyStates(Arc<[CoroMonoBarrier; 4]>);

impl ReadyStates {
    #[inline]
    fn new() -> ReadyStates {
        ReadyStates(Arc::new([CoroMonoBarrier::new(),
                              CoroMonoBarrier::new(),
                              CoroMonoBarrier::new(),
                              CoroMonoBarrier::new()]))
    }

    #[inline]
    pub fn wait(&self, ready_type: ReadyType) {
        self.0[ready_type as usize].wait().unwrap();
    }

    #[inline]
    pub fn notify(&self, ready_type: ReadyType) {
        self.0[ready_type as usize].notify();
    }
}

/// Coroutine scheduler
pub struct Scheduler {
    default_spawn_options: Options,
    expected_worker_count: usize,

    // Mio event loop handler
    event_loop_sender: Option<Sender<Message>>,
    slab: Slab<ReadyStates, usize>,

    parked_processors: Mutex<LinkedHashMap<usize, ProcMessageSender>>,
}

unsafe impl Send for Scheduler {}

impl Scheduler {
    /// Create a scheduler with default configurations
    pub fn new() -> Scheduler {
        Scheduler {
            default_spawn_options: Options::default(),
            expected_worker_count: 1,

            event_loop_sender: None,
            slab: Slab::new(1024),

            parked_processors: Mutex::new(LinkedHashMap::new()),
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
    pub fn run<F, T>(&mut self, f: F) -> Result<T, Box<Any + Send>>
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        let default_handler = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            if let Some(mut p) = Processor::current() {
                if let Some(c) = p.current() {
                    let mut stderr = io::stderr();
                    let _ = write!(stderr, "Coroutine `{}` running in ", c.debug_name());
                }
            }

            default_handler(panic_info);
        }));

        let mut machines = Vec::with_capacity(self.expected_worker_count);

        for tid in 0..self.expected_worker_count {
            machines.push(Processor::spawn(self, tid));
        }

        for x in 0..machines.len() {
            for y in 0..machines.len() {
                if x != y {
                    machines[x]
                        .processor_handle
                        .send(ProcMessage::NewNeighbor(machines[y].stealer.clone()))
                        .unwrap();
                }
            }
        }

        let mut event_loop = EventLoop::new().unwrap();
        self.event_loop_sender = Some(event_loop.channel());

        let mut result = None;

        let cloned_event_loop_sender = event_loop.channel();
        {
            let result = unsafe { &mut *(&mut result as *mut _) };
            let wrapper = move || {
                let ret = unsafe { ::try(move || f()) };

                *result = Some(ret);

                trace!("Coroutine `<main>` finished => sending Shutdown");
                cloned_event_loop_sender.send(Message::Shutdown).unwrap();
            };

            let mut opt = self.default_spawn_options.clone();
            opt.name("<main>".to_owned());
            let main_coro = Coroutine::spawn_opts(wrapper, opt);

            machines[0].processor_handle.send(ProcMessage::Ready(main_coro)).unwrap();
        };

        event_loop.run(self).unwrap();

        trace!("EventLoop finished => sending Shutdown");

        for m in machines.iter() {
            m.processor_handle.send(ProcMessage::Shutdown).unwrap();
        }

        trace!("awaiting completion of Machines");

        // NOTE: It's critical that all threads are joined since Processor
        // maintains a reference to this Scheduler using raw pointers.
        for m in machines.drain(..) {
            let _ = m.thread_handle.join();
        }

        trace!("Machines finished");

        // Restore panic handler
        panic::take_hook();

        result.unwrap()
    }

    /// Get the global Scheduler
    pub fn instance() -> Option<&'static Scheduler> {
        Processor::current().and_then(|p| unsafe { Some(mem::transmute(p.scheduler())) })
    }

    /// Get the global Scheduler
    pub fn instance_or_err() -> io::Result<&'static Scheduler> {
        Self::instance().ok_or(io::Error::new(io::ErrorKind::Other, "Scheduler missing"))
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
            let ret = unsafe { ::try(move || f()) };

            // No matter whether it is panicked or not, the result will be sent to the channel
            let _ = tx.push(ret);
        };
        let mut processor = Processor::current().unwrap();
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
        trace!("Coroutine `{}`: readying", coro.debug_name());

        let current = Processor::current();

        if let Some(mut preferred) = coro.preferred_processor() {
            if let Some(current) = current {
                if preferred == current {
                    trace!("Coroutine `{}`: pushing into preferred queue",
                           coro.debug_name());

                    // We're on the same thread ---> use the faster ready() method.
                    return preferred.ready(coro);
                }
            }

            trace!("Coroutine `{}`: sending to preferred {:?}",
                   coro.debug_name(),
                   preferred);

            let _ = preferred.handle().send(ProcMessage::Ready(coro));
            return;
        }

        if let Some(mut current) = current {
            trace!("Coroutine `{}`: pushing into current queue",
                   coro.debug_name());
            return current.ready(coro);
        }

        // Resume it right here
        trace!("Coroutine `{}`: resuming without processor",
               coro.debug_name());
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

        {
            Scheduler::park_with(|_, coro| {
                let channel = self.event_loop_sender.as_ref().unwrap();
                let mut msg = Message::Timer(TimerMessage::new(coro, delay, &mut ret));

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
    pub fn sleep(&self, delay: Duration) -> Result<(), TimerError> {
        self.sleep_ms(delay.as_secs() * 1_000 + delay.subsec_nanos() as u64 / 1_000_000)
    }

    #[doc(hidden)]
    pub fn park_processor(&self, id: usize, prochdl: ProcMessageSender) {
        let mut parked = self.parked_processors.lock().unwrap();
        parked.insert(id, prochdl);
    }

    #[doc(hidden)]
    pub fn unpark_processor(&self, id: usize) {
        let mut parked = self.parked_processors.lock().unwrap();
        parked.remove(&id);
    }
}

impl Handler for Scheduler {
    type Timeout = Token;
    type Message = Message;

    fn ready(&mut self, _event_loop: &mut EventLoop<Self>, token: Token, events: EventSet) {
        trace!("Handler: got {:?} for {:?}", events, token);

        let ready_states = self.slab.get(token.as_usize()).expect("Token must be registered");

        if events.is_readable() {
            ready_states.notify(ReadyType::Readable);
        }

        if events.is_writable() {
            ready_states.notify(ReadyType::Writable);
        }

        if events.is_error() {
            ready_states.notify(ReadyType::Error);
        }

        if events.is_hup() {
            ready_states.notify(ReadyType::Hup);
        }
    }

    fn timeout(&mut self, _event_loop: &mut EventLoop<Self>, token: Token) {
        let coro = unsafe { Handle::from_raw(mem::transmute(token)) };
        trace!("Handler: timout for {:?}", coro);
        Scheduler::ready(coro);
    }

    fn notify(&mut self, event_loop: &mut EventLoop<Self>, msg: Self::Message) {
        match msg {
            Message::Register(msg) => {
                trace!("Handler: registering for {:?}", msg.coro);

                if self.slab.remaining() == 0 {
                    // doubles the size of the slab each time
                    let grow = self.slab.count();
                    self.slab.grow(grow);
                }

                self.slab.insert_with_opt(move |token| {
                    let token = unsafe { mem::transmute(token) };
                    let ready_states = ReadyStates::new();

                    let ret = if (msg.cb)(event_loop, token, ready_states.clone()) {
                        Some(ready_states)
                    } else {
                        None
                    };

                    Scheduler::ready(msg.coro);

                    ret
                });
            }
            Message::Deregister(msg) => {
                trace!("Handler: deregistering for {:?}", msg.coro);

                let _ = self.slab.remove(unsafe { mem::transmute(msg.token) });

                (msg.cb)(event_loop);
                Scheduler::ready(msg.coro);
            }
            Message::Timer(msg) => {
                trace!("Handler: adding timer for {:?}", msg.coro);

                let coro_ptr = Handle::into_raw(msg.coro);
                let token = unsafe { mem::transmute(coro_ptr) };
                let result = unsafe { &mut *msg.result };

                if let Err(err) = event_loop.timeout_ms(token, msg.delay) {
                    *result = Err(err);
                    Scheduler::ready(unsafe { Handle::from_raw(coro_ptr) });
                }
            }
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
