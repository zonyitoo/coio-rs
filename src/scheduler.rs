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
use std::thread;

use mio::{EventLoop, Evented, Handler, Token, EventSet, PollOpt, Sender};
use mio::util::Slab;

use coroutine::{Handle, Coroutine, State};
use options::Options;
use runtime::processor::{Processor, ProcMessage};
use join_handle::{self, JoinHandleReceiver};

/// A handle that could join the coroutine
pub struct JoinHandle<T> {
    result: JoinHandleReceiver<T>,
}

impl<T> JoinHandle<T> {
    /// Join the coroutine until it finishes.
    ///
    /// If it already finished, this method will return immediately.
    pub fn join(self) -> Result<T, Box<Any + Send + 'static>> {
        self.result.pop()
    }
}

unsafe impl<T: Send> Send for JoinHandle<T> {}

struct IoHandler {
    slab: Slab<Option<(Handle, ReadyCallback<'static>)>>,
}

type RegisterCallback<'a> = &'a mut (FnMut(&mut EventLoop<IoHandler>, Token) -> bool);
type ReadyCallback<'a> = &'a mut (FnMut(&mut EventLoop<IoHandler>));

struct IoHandlerMessage {
    coroutine: Handle,
    register: RegisterCallback<'static>,
    ready: ReadyCallback<'static>,
}

impl IoHandlerMessage {
    fn new<'scope>(coro: Handle,
                   reg: RegisterCallback<'scope>,
                   ready: ReadyCallback<'scope>)
                   -> IoHandlerMessage {
        let reg = unsafe {
            mem::transmute::<RegisterCallback<'scope>, RegisterCallback<'static>>(reg)
        };

        let ready = unsafe {
            mem::transmute::<ReadyCallback<'scope>, ReadyCallback<'static>>(ready)
        };

        IoHandlerMessage {
            coroutine: coro,
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
            Some(Some((coro, cb))) => {
                cb(event_loop);
                Scheduler::ready(coro);
            }
            None => {
                warn!("No coroutine is waiting on token {:?}", token);
            }
            _ => unreachable!(),
        }
    }

    fn timeout(&mut self, event_loop: &mut EventLoop<Self>, token: Token) {
        trace!("Timer waked up {:?}", token);

        if token == Token(0) {
            error!("Received timeout event from Token(0)");
            return;
        }

        match self.slab.remove(token) {
            Some(Some((coro, cb))) => {
                cb(event_loop);
                Scheduler::ready(coro);
            }
            None => {
                warn!("No coroutine is waiting on token {:?}", token);
            }
            _ => unreachable!(),
        }
    }

    fn notify(&mut self, event_loop: &mut EventLoop<Self>, msg: Self::Message) {
        self.slab
            .insert_with(move |token| {
                if (msg.register)(event_loop, token) {
                    Some((msg.coroutine, msg.ready))
                } else {
                    Scheduler::ready(msg.coroutine);
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

    #[inline]
    #[doc(hidden)]
    pub unsafe fn work_counter(&self) -> Arc<AtomicUsize> {
        self.work_count.clone()
    }

    /// A coroutine is ready for schedule
    #[doc(hidden)]
    pub fn ready(mut coro: Handle) {
        trace!("Coroutine `{}` is ready to run", coro.debug_name());
        let current = Processor::current();

        if let Some(mut preferred) = coro.preferred_processor() {
            trace!("Coroutine `{}` has preferred {:?}",
                   coro.debug_name(),
                   preferred);

            if let Some(ref curr) = current {
                if preferred.id() == curr.id() {
                    // We're on the same thread ---> use the faster ready() method.
                    trace!("Coroutine `{}` preferred to run in the current thread, push it into \
                            local queue",
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

    /// Suspend the current coroutine or thread
    #[inline]
    pub fn sched() {
        match Processor::current() {
            Some(p) => p.sched(),
            None => thread::yield_now(),
        }
    }

    /// Block the current coroutine
    #[inline]
    pub fn block_with<'scope, U, F>(f: F) -> U
        where F: FnOnce(&mut Processor, Handle) -> U + 'scope,
              U: 'scope
    {
        Processor::current().map(|x| x.block_with(f)).unwrap()
    }
}

impl Scheduler {
    /// Block the current coroutine and wait for I/O event
    #[doc(hidden)]
    pub fn wait_event<'scope, E: Evented>(&self,
                                          fd: &'scope E,
                                          interest: EventSet)
                                          -> io::Result<()> {
        let mut ret1 = Ok(());
        let mut ret2 = Ok(());

        {
            let mut reg = |evloop: &mut EventLoop<IoHandler>, token| {
                let r = evloop.register(fd, token, interest, PollOpt::edge() | PollOpt::oneshot());

                match r {
                    Ok(..) => true,
                    Err(..) => {
                        ret1 = r;
                        false
                    }
                }
            };

            let mut ready = |evloop: &mut EventLoop<IoHandler>| {
                if cfg!(not(any(target_os = "macos",
                                target_os = "ios",
                                target_os = "freebsd",
                                target_os = "dragonfly",
                                target_os = "netbsd"))) {
                    ret2 = evloop.deregister(fd);
                }
            };

            let reg = &mut reg as &mut FnMut(&mut EventLoop<IoHandler>, Token) -> bool;
            let ready = &mut ready as &mut FnMut(&mut EventLoop<IoHandler>);

            Scheduler::block_with(|_, coro| {
                let channel = self.event_loop_sender.as_ref().unwrap();
                channel.send(IoHandlerMessage::new(coro, reg, ready)).unwrap();
            });
        }

        ret1.and(ret2)
    }

    /// Block the current coroutine until the specific time
    #[doc(hidden)]
    pub fn sleep_ms(&self, delay: u64) -> io::Result<()> {
        let mut ret = Ok(());

        {
            let mut reg = |evloop: &mut EventLoop<IoHandler>, token| {
                match evloop.timeout_ms(token, delay) {
                    Ok(..) => true,
                    Err(..) => {
                        ret = Err(io::Error::new(io::ErrorKind::Other, "failed to add timer"));
                        false
                    }
                }
            };

            let mut ready = move |_: &mut EventLoop<IoHandler>| {};

            let reg = &mut reg as &mut FnMut(&mut EventLoop<IoHandler>, Token) -> bool;
            let ready = &mut ready as &mut FnMut(&mut EventLoop<IoHandler>);

            Scheduler::block_with(|_, coro| {
                let channel = self.event_loop_sender.as_ref().unwrap();
                channel.send(IoHandlerMessage::new(coro, reg, ready)).unwrap();
            });
        }

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
