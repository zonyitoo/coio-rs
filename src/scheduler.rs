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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{Sender, TryRecvError};
use std::default::Default;
use std::any::Any;
use std::io;
use std::cell::UnsafeCell;
use std::time::Duration;
use std::boxed::FnBox;
use std::mem;

use deque::Stealer;

use mio::{EventLoop, Evented, Handler, Token, EventSet, PollOpt};
use mio::util::Slab;

use runtime::processor::{Processor, ProcMessage};
use coroutine::{Coroutine, SendableCoroutinePtr};
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

type RegisterCallback<'a> = Box<FnBox(&mut EventLoop<IoHandler>, Token) + Send + 'a>;
type ReadyCallback<'a> = Box<FnBox(&mut EventLoop<IoHandler>) + Send + 'a>;

struct IoHandlerMessage {
    register: RegisterCallback<'static>,
    ready: ReadyCallback<'static>,
}

impl IoHandlerMessage {
    fn new<'scope, Reg, Ready>(reg: Reg, ready: Ready) -> IoHandlerMessage
        where Reg: FnOnce(&mut EventLoop<IoHandler>, Token) + Send + 'scope,
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
                let IoHandlerMessage { register, ready } = msg;
                register.call_box((event_loop, token));
                Some(ready)
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
    eventloop: UnsafeCell<EventLoop<IoHandler>>,
    io_handler: UnsafeCell<IoHandler>,
}

unsafe impl Send for Scheduler {}
unsafe impl Sync for Scheduler {}

impl Scheduler {
    /// Create a scheduler with default configurations
    pub fn new() -> Scheduler {
        Scheduler {
            work_counts: AtomicUsize::new(0),
            expected_worker_count: 1,

            eventloop: UnsafeCell::new(EventLoop::new().unwrap()),
            io_handler: UnsafeCell::new(IoHandler::new()),
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
    #[inline]
    pub fn instance() -> Option<&'static Scheduler> {
        Processor::current().and_then(|p| Some(p.scheduler()))
    }

    /// A coroutine is ready for schedule
    #[doc(hidden)]
    #[inline]
    pub fn ready(coro: *mut Coroutine) {
        Processor::current().unwrap().ready(coro);
    }

    /// A coroutine is finished
    ///
    /// The coroutine will be destroy, make sure that the coroutine pointer is unique!
    #[doc(hidden)]
    #[inline]
    pub fn finished(coro_ptr: *mut Coroutine) {
        Scheduler::instance().unwrap().work_counts.fetch_sub(1, Ordering::SeqCst);

        let boxed = unsafe { Box::from_raw(coro_ptr) };
        drop(boxed);
    }

    /// Total works
    #[inline]
    pub fn work_count(&self) -> usize {
        self.work_counts.load(Ordering::SeqCst)
    }

    /// Spawn a new coroutine with default options
    #[inline]
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
    pub fn run<M, R>(self, main_fn: M) -> Result<R, Box<Any + Send + 'static>>
        where M: FnOnce() -> R + Send + 'static,
              R: Send + 'static
    {
        let the_sched = Arc::new(self);
        let mut handles = Vec::new();

        let mut processor_handlers: Vec<Sender<ProcMessage>> = Vec::new();
        let mut processor_stealers: Vec<Stealer<SendableCoroutinePtr>> = Vec::new();

        // Run the main function
        let main_coro_hdl = {
            // The first worker
            let (hdl, msg, st, main_hdl) = Processor::run_main(0, the_sched.clone(), main_fn);
            handles.push(hdl);

            processor_handlers.push(msg);
            processor_stealers.push(st);

            main_hdl
        };

        {
            // The others
            for tid in 1..the_sched.expected_worker_count {
                let (hdl, msg, st) = Processor::run_with_neighbors(tid,
                                                                   the_sched.clone(),
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
            {
                let io_handler: &mut IoHandler = unsafe { &mut *the_sched.io_handler.get() };
                let event_loop: &mut EventLoop<IoHandler> = unsafe {
                    &mut *the_sched.eventloop.get()
                };

                event_loop.run_once(io_handler, Some(100)).unwrap();
            }

            match main_coro_hdl.try_recv() {
                Ok(main_ret) => {
                    {
                        for msg in processor_handlers.iter() {
                            let _ = msg.send(ProcMessage::Shutdown);
                        }
                    }

                    {
                        let event_loop: &mut EventLoop<IoHandler> = unsafe {
                            &mut *the_sched.eventloop.get()
                        };
                        let io_handler: &mut IoHandler = unsafe {
                            &mut *the_sched.io_handler.get()
                        };
                        io_handler.wakeup_all(event_loop);
                    }

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
    #[inline]
    pub fn sched() {
        Processor::current().unwrap().sched();
    }

    /// Block the current coroutine
    #[inline]
    pub fn block() {
        Processor::current().unwrap().block();
    }
}

impl Scheduler {
    fn get_processor_and_coroutine() -> io::Result<(&'static mut Processor, *mut Coroutine)> {
        if let Some(processor) = Processor::current() {
            if let Some(coroutine) = processor.running() {
                return Ok((processor, coroutine));
            }
        };

        Err(io::Error::new(io::ErrorKind::Other, "A running coroutine is required!"))
    }

    /// Block the current coroutine and wait for I/O event
    #[doc(hidden)]
    pub fn wait_event<'scope, E: Evented>(&self,
                                          fd: &'scope E,
                                          interest: EventSet)
                                          -> io::Result<()> {
        let (processor, ptr) = try!(Scheduler::get_processor_and_coroutine());

        let event_loop: &mut EventLoop<IoHandler> = unsafe { &mut *self.eventloop.get() };
        let proc_hdl = processor.handle();
        let sendable_coro = SendableCoroutinePtr(ptr);
        let channel = event_loop.channel();

        struct EventedWrapper<E: Evented>(*const E);
        unsafe impl<E: Evented> Send for EventedWrapper<E> {}
        unsafe impl<E: Evented> Sync for EventedWrapper<E> {}

        let fd1 = EventedWrapper(fd);
        let fd2 = EventedWrapper(fd);

        let msg = IoHandlerMessage::new(|evloop: &mut EventLoop<IoHandler>, token| {
                                            let fd: &E = unsafe { &*fd1.0 };
                                            evloop.register(fd,
                                                            token,
                                                            interest,
                                                            PollOpt::edge() | PollOpt::oneshot())
                                                  .unwrap();
                                        },
                                        move |evloop: &mut EventLoop<IoHandler>| {
                                            proc_hdl.send(ProcMessage::Ready(sendable_coro))
                                                    .unwrap();

                                            if cfg!(not(any(target_os = "macos",
                                                            target_os = "ios",
                                                            target_os = "freebsd",
                                                            target_os = "dragonfly",
                                                            target_os = "netbsd"))) {
                                                let fd: &E = unsafe { &*fd2.0 };
                                                evloop.deregister(fd).unwrap();
                                            }
                                        });
        channel.send(msg).unwrap();

        processor.block();

        Ok(())
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep_ms(&self, delay: u64) {
        if let Ok((processor, ptr)) = Scheduler::get_processor_and_coroutine() {
            let event_loop: &mut EventLoop<IoHandler> = unsafe { &mut *self.eventloop.get() };
            let proc_hdl = processor.handle();
            let sendable_coro = SendableCoroutinePtr(ptr);
            let channel = event_loop.channel();

            let msg = IoHandlerMessage::new(|evloop: &mut EventLoop<IoHandler>, token| {
                                                evloop.timeout_ms(token, delay).unwrap();
                                            },
                                            move |_: &mut EventLoop<IoHandler>| {
                                                proc_hdl.send(ProcMessage::Ready(sendable_coro))
                                                        .unwrap();
                                            });
            channel.send(msg).unwrap();

            processor.block();
        }
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep(&self, delay: Duration) {
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
