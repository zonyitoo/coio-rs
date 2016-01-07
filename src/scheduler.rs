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
use std::boxed::FnBox;
use std::default::Default;
use std::io;
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, TryRecvError};
use std::time::Duration;

use deque::Stealer;

use mio::{EventLoop, Evented, Handler, Token, EventSet, PollOpt};
use mio::util::Slab;

use runtime::processor::{Processor, ProcMessage};
use coroutine::{SendableCoroutinePtr, Handle};
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
    slab: Slab<Option<ReadyCallback<'static>>>,
}

type RegisterCallback<'a> = Box<FnBox(&mut EventLoop<IoHandler>, Token) -> bool + Send + 'a>;
type ReadyCallback<'a> = Box<FnBox(&mut EventLoop<IoHandler>) + Send + 'a>;

struct IoHandlerMessage {
    register: RegisterCallback<'static>,
    ready: ReadyCallback<'static>,
}

impl IoHandlerMessage {
    fn new<'scope, Reg, Ready>(reg: Reg, ready: Ready) -> IoHandlerMessage
        where Reg: FnOnce(&mut EventLoop<IoHandler>, Token) -> bool + Send + 'scope,
              Ready: FnOnce(&mut EventLoop<IoHandler>) + Send + 'scope
    {
        let reg = unsafe {
            mem::transmute::<RegisterCallback<'scope>, RegisterCallback<'static>>(Box::new(reg))
        };

        let ready = unsafe {
            mem::transmute::<ReadyCallback<'scope>, ReadyCallback<'static>>(Box::new(ready))
        };

        IoHandlerMessage {
            register: reg,
            ready: ready,
        }
    }
}

unsafe impl Send for IoHandlerMessage {}

impl Handler for IoHandler {
    type Timeout = Token;
    type Message = IoHandlerMessage;

    fn ready(&mut self, event_loop: &mut EventLoop<Self>, token: Token, events: EventSet) {
        trace!("Got {:?} for {:?}", events, token);

        if token == Token(0) {
            error!("Received events from Token(0): {:?}", events);
            return;
        }

        match self.slab.remove(token) {
            Some(cb) => cb.unwrap().call_box((event_loop,)),
            None => {
                warn!("No coroutine is waiting on token {:?}", token);
            }
        }
    }

    fn timeout(&mut self, event_loop: &mut EventLoop<Self>, token: Token) {
        trace!("Timer waked up {:?}", token);

        if token == Token(0) {
            error!("Received timeout event from Token(0)");
            return;
        }

        match self.slab.remove(token) {
            Some(cb) => cb.unwrap().call_box((event_loop,)),
            None => {
                warn!("No coroutine is waiting on token {:?}", token);
            }
        }
    }

    fn notify(&mut self, event_loop: &mut EventLoop<Self>, msg: Self::Message) {
        self.slab
            .insert_with(move |token| {
                if msg.register.call_box((event_loop, token)) {
                    Some(msg.ready)
                } else {
                    None
                }
            })
            .unwrap();
    }
}

impl IoHandler {
    fn new() -> IoHandler {
        IoHandler { slab: Slab::new_starting_at(Token(1), 102400) }
    }

    fn wakeup_all(&mut self, event_loop: &mut EventLoop<Self>) {
        for cb in self.slab.iter_mut() {
            let cb = cb.take();
            cb.unwrap().call_box((event_loop,));
        }

        self.slab.clear();
    }
}

/// Coroutine scheduler
pub struct Scheduler {
    work_counts: AtomicUsize,
    expected_worker_count: usize,

    // Mio event loop and the handler
    // It controls all I/O and timer waits
    event_loop: EventLoop<IoHandler>,
    io_handler: IoHandler,
}

unsafe impl Send for Scheduler {}
unsafe impl Sync for Scheduler {}

impl Scheduler {
    /// Create a scheduler with default configurations
    pub fn new() -> Scheduler {
        Scheduler {
            work_counts: AtomicUsize::new(0),
            expected_worker_count: 1,

            event_loop: EventLoop::new().unwrap(),
            io_handler: IoHandler::new(),
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
    pub fn instance() -> Option<&'static Scheduler> {
        Processor::current().and_then(|p| Some(p.scheduler()))
    }

    /// A coroutine is ready for schedule
    #[doc(hidden)]
    pub fn ready(coro: Handle) {
        Processor::current().unwrap().ready(coro);
    }

    /// A coroutine is finished
    ///
    /// The coroutine will be destroy, make sure that the coroutine pointer is unique!
    #[doc(hidden)]
    pub fn finished(mut coro: Handle) {
        Scheduler::instance().unwrap().work_counts.fetch_sub(1, Ordering::SeqCst);
        coro.set_drop_allowed();
    }

    /// Total works
    pub fn work_count(&self) -> usize {
        self.work_counts.load(Ordering::SeqCst)
    }

    /// Spawn a new coroutine with default options
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
        let processor = Processor::current().unwrap();

        processor.scheduler().work_counts.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = ::sync::mpsc::channel();
        let wrapper = move || {
            let ret = unsafe { ::try(move || f()) };

            // No matter whether it is panicked or not, the result will be sent to the channel
            let _ = tx.send(ret); // Just ignore if it failed
        };
        processor.spawn_opts(Box::new(wrapper), opts);

        JoinHandle { result: rx }
    }

    /// Run the scheduler
    pub fn run<M, R>(&mut self, main_fn: M) -> Result<R, Box<Any + Send + 'static>>
        where M: FnOnce() -> R + Send + 'static,
              R: Send + 'static
    {
        let mut handles = Vec::new();

        let mut processor_handlers: Vec<Sender<ProcMessage>> = Vec::new();
        let mut processor_stealers: Vec<Stealer<Handle>> = Vec::new();

        // Run the main function
        let main_coro_hdl = {
            // The first worker
            let (hdl, msg, st, main_hdl) = Processor::run_main(0, self, main_fn);
            handles.push(hdl);

            processor_handlers.push(msg);
            processor_stealers.push(st);

            main_hdl
        };

        {
            // The others
            for tid in 1..self.expected_worker_count {
                let (hdl, msg, st) = Processor::run_with_neighbors(tid,
                                                                   self,
                                                                   processor_stealers.clone());

                for msg in processor_handlers.iter() {
                    if let Err(err) = msg.send(ProcMessage::NewNeighbor(st.clone())) {
                        error!("Error while sending NewNeighbor {:?}", err);
                    }
                }

                handles.push(hdl);

                processor_handlers.push(msg);
                processor_stealers.push(st);
            }
        }

        // The scheduler loop
        loop {
            self.event_loop.run_once(&mut self.io_handler, Some(100)).unwrap();

            match main_coro_hdl.try_recv() {
                Ok(main_ret) => {
                    for msg in processor_handlers.iter() {
                        let _ = msg.send(ProcMessage::Shutdown);
                    }

                    self.io_handler.wakeup_all(&mut self.event_loop);

                    // NOTE: It's critical that all threads are joined since Processor
                    // maintains a reference to this Scheduler using raw pointers.
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
    pub fn sched() {
        Processor::current().unwrap().sched();
    }

    /// Block the current coroutine
    #[inline]
    pub fn take_current_coroutine<U, F>(f: F) -> U
        where F: FnOnce(Handle) -> U
    {
        Processor::current().unwrap().take_current_coroutine(f)
    }
}

struct ResultWrapper(*mut io::Result<()>);
unsafe impl Send for ResultWrapper {}
unsafe impl Sync for ResultWrapper {}

impl Scheduler {
    /// Block the current coroutine and wait for I/O event
    #[doc(hidden)]
    pub fn wait_event<'scope, E: Evented>(&self,
                                          fd: &'scope E,
                                          interest: EventSet)
                                          -> io::Result<()> {
        let mut ret = Ok(());

        Scheduler::take_current_coroutine(|coro| {
            let proc_hdl1 = Processor::current().unwrap().handle();
            let proc_hdl2 = proc_hdl1.clone();
            let channel = self.event_loop.channel();

            struct EventedWrapper<E>(*const E);
            unsafe impl<E> Send for EventedWrapper<E> {}
            unsafe impl<E> Sync for EventedWrapper<E> {}

            let fd1 = EventedWrapper(fd);
            let fd2 = EventedWrapper(fd);
            let ret1 = ResultWrapper(&mut ret);
            let ret2 = ResultWrapper(&mut ret);
            let coro1 = SendableCoroutinePtr(Box::into_raw(coro));
            let coro2 = coro1;

            let reg = move |evloop: &mut EventLoop<IoHandler>, token| {
                let fd = unsafe { &*fd1.0 };
                let ret = unsafe { &mut *ret1.0 };
                let r = evloop.register(fd, token, interest, PollOpt::edge() | PollOpt::oneshot());

                match r {
                    Ok(..) => true,
                    Err(..) => {
                        *ret = r;
                        proc_hdl1.send(ProcMessage::Ready(unsafe { Box::from_raw(coro1.0) }))
                                 .unwrap();
                        false
                    }
                }
            };

            let ready = move |evloop: &mut EventLoop<IoHandler>| {
                if cfg!(not(any(target_os = "macos",
                                target_os = "ios",
                                target_os = "freebsd",
                                target_os = "dragonfly",
                                target_os = "netbsd"))) {
                    let fd = unsafe { &*fd2.0 };
                    let ret = unsafe { &mut *ret2.0 };
                    *ret = evloop.deregister(fd);
                }

                proc_hdl2.send(ProcMessage::Ready(unsafe { Box::from_raw(coro2.0) })).unwrap();
            };

            channel.send(IoHandlerMessage::new(reg, ready)).unwrap();
        });

        ret
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep_ms(&self, delay: u64) -> io::Result<()> {
        let mut ret = Ok(());

        Scheduler::take_current_coroutine(|coro| {
            let proc_hdl = Processor::current().unwrap().handle();
            let channel = self.event_loop.channel();

            let ret1 = ResultWrapper(&mut ret);

            let reg = |evloop: &mut EventLoop<IoHandler>, token| {
                let ret = unsafe { &mut *ret1.0 };

                match evloop.timeout_ms(token, delay) {
                    Ok(..) => true,
                    Err(..) => {
                        *ret = Err(io::Error::new(io::ErrorKind::Other, "failed to add timer"));
                        false
                    }
                }
            };

            let ready = move |_: &mut EventLoop<IoHandler>| {
                proc_hdl.send(ProcMessage::Ready(coro)).unwrap();
            };

            channel.send(IoHandlerMessage::new(reg, ready)).unwrap();
        });

        ret
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep(&self, delay: Duration) -> io::Result<()> {
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
