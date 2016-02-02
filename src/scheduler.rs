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
use std::sync::mpsc::{TryRecvError, RecvError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mio::{EventLoop, Evented, Handler, Token, EventSet, PollOpt, Sender};
use mio::util::Slab;

use coroutine::{SendableCoroutinePtr, Handle, Coroutine, State};
use options::Options;
use runtime::processor::{Processor, ProcMessage};
use sync::mpsc::Receiver;

/// A handle that could join the coroutine
pub struct JoinHandle<T> {
    result: Receiver<Result<T, Box<Any + Send + 'static>>>,
}

impl<T> JoinHandle<T> {
    /// Tries to join with the coroutine.
    pub fn try_join(&self) -> Option<Result<T, Box<Any + Send + 'static>>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => panic!("Failed to receive from the channel"),
        }
    }

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
}

/// Coroutine scheduler
pub struct Scheduler {
    expected_worker_count: usize,

    // // Mio event loop and the handler
    // // It controls all I/O and timer waits
    // event_loop: EventLoop<IoHandler>,
    // io_handler: IoHandler,
    event_loop_sender: Option<Sender<IoHandlerMessage>>,
    work_count: Arc<AtomicUsize>,
}

unsafe impl Send for Scheduler {}
unsafe impl Sync for Scheduler {}

impl Scheduler {
    /// Create a scheduler with default configurations
    pub fn new() -> Scheduler {
        Scheduler {
            expected_worker_count: 1,

            // event_loop: EventLoop::new().unwrap(),
            // io_handler: IoHandler::new(),
            event_loop_sender: None,
            work_count: Arc::new(AtomicUsize::new(0)),
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
        Processor::current().and_then(|p| unsafe { Some(mem::transmute(p.scheduler())) })
    }

    #[inline]
    pub fn work_count(&self) -> usize {
        self.work_count.load(Ordering::SeqCst)
    }

    /// A coroutine is ready for schedule
    #[doc(hidden)]
    pub fn ready(mut coro: Handle) {
        trace!("Coroutine `{}` is ready to run", coro.debug_name());
        let current = Processor::current();

        if let Some(mut preferred) = coro.preferred_processor() {
            trace!("Coroutine `{}` has preferred {:?}", coro.debug_name(), preferred);

            if let Some(ref curr) = current {
                if preferred.id() == curr.id() {
                    // We're on the same thread ---> use the faster ready() method.
                    trace!("Coroutine `{}` preferred to run in the current thread, push it into local queue",
                           coro.debug_name());
                    return preferred.ready(coro);
                }
            }

            trace!("Push Coroutine `{}` into the message queue of {:?}",
                   coro.debug_name(),
                   preferred);
            let _ = preferred.handle().send(ProcMessage::Ready(coro));
            return;
        }

        if let Some(mut current) = current {
            trace!("Coroutine `{}` does not have preferred processor, push it into local queue",
                   coro.debug_name());
            return current.ready(coro);
        }

        // Resume it right here
        trace!("Coroutine `{}` runs without processor", coro.debug_name());
        Coroutine::resume(State::Running, &mut *coro);
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
        let (mut new_coro, rx) = Scheduler::create_coroutine_opts(f, opts);
        new_coro.set_preferred_processor(Some(processor.weak_self().clone()));
        new_coro.attach(processor.scheduler().work_count.clone());

        processor.ready(new_coro);

        JoinHandle { result: rx }
    }

    fn create_coroutine_opts<F, T>(f: F, opts: Options)
            -> (Handle, Receiver<Result<T, Box<Any + Send>>>)
        where F: FnOnce() -> T + Send + 'static,
              T: Send + 'static
    {
        let (tx, rx) = ::sync::mpsc::channel();
        let wrapper = move || {
            let ret = unsafe { ::try(move || f()) };

            // No matter whether it is panicked or not, the result will be sent to the channel
            let _ = tx.send(ret);
        };

        let new_coro = Coroutine::spawn_opts(Box::new(wrapper), opts);
        (new_coro, rx)
    }

    /// Run the scheduler
    pub fn run<F, T>(&mut self, f: F) -> Result<T, Box<Any + Send>>
        where F: FnOnce() -> T + Send,
              T: Send
    {
        let mut machines = Vec::with_capacity(self.expected_worker_count);

        for tid in 0..self.expected_worker_count {
            machines.push(Processor::new(self, tid));
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
        let mut io_handler = IoHandler::new();

        let main_coro_rx = {
            let (tx, rx) = ::sync::mpsc::channel();
            let wrapper = move || {
                let ret = unsafe { ::try(move || f()) };

                // No matter whether it is panicked or not, the result will be sent to the channel
                let _ = tx.send(ret);
            };

            let f: Box<FnBox()> = Box::new(wrapper);
            let mut main_coro = Coroutine::spawn_opts(unsafe { mem::transmute(f) },
                                                      Options::new().name("<main>".to_owned()));

            main_coro.attach(self.work_count.clone());

            machines[0].processor_handle.send(ProcMessage::Ready(main_coro)).unwrap();

            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                _ => panic!("nope"),
            }

            rx
        };

        match main_coro_rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            _ => panic!("nope"),
        }

        while self.work_count.load(Ordering::SeqCst) != 0 {
            event_loop.run_once(&mut io_handler, Some(100)).unwrap();
        }

        match main_coro_rx.recv() {
            Ok(main_ret) => {
                // Check again to be sure
                while self.work_count.load(Ordering::SeqCst) != 0 {
                    event_loop.run_once(&mut io_handler, Some(100)).unwrap();
                }

                trace!("Scheduler is going to shutdown, asking all the threads to shutdown");

                for m in machines.iter() {
                    m.processor_handle.send(ProcMessage::Shutdown).unwrap();
                }

                // NOTE: It's critical that all threads are joined since Processor
                // maintains a reference to this Scheduler using raw pointers.
                for m in machines.drain(..) {
                    let _ = m.thread_handle.join();
                }

                trace!("Bye bye!");

                return main_ret;
            }
            Err(RecvError) => {
                panic!("Main coro is failed with RecvError");
            }
        }
    }

    /// Suspend the current coroutine
    #[inline]
    pub fn sched() {
        Processor::current().unwrap().sched()
    }

    /// Block the current coroutine
    #[inline]
    pub fn block_with<'scope, U, F>(f: F) -> U
        where F: FnOnce(&mut Processor, Handle) -> U + 'scope,
              U: 'scope
    {
        Processor::current().unwrap().block_with(f)
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

        Scheduler::block_with(|_, coro| {
            let proc_hdl1 = Processor::current().unwrap().handle();
            let proc_hdl2 = proc_hdl1.clone();
            // let channel = self.event_loop.channel();
            let channel = self.event_loop_sender.as_ref().unwrap();

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

        Scheduler::block_with(|_, coro| {
            let proc_hdl = Processor::current().unwrap().handle();
            let channel = self.event_loop_sender.as_ref().unwrap();

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
