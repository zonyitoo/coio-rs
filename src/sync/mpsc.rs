// The MIT License (MIT)

// Copyright (c) 2015 Y. T. Chung <zonyitoo@gmail.com>

//  Permission is hereby granted, free of charge, to any person obtaining a
//  copy of this software and associated documentation files (the "Software"),
//  to deal in the Software without restriction, including without limitation
//  the rights to use, copy, modify, merge, publish, distribute, sublicense,
//  and/or sell copies of the Software, and to permit persons to whom the
//  Software is furnished to do so, subject to the following conditions:
//
//  The above copyright notice and this permission notice shall be included in
//  all copies or substantial portions of the Software.
//
//  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//  OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//  FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//  AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//  LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//  FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//  DEALINGS IN THE SOFTWARE.

//! Multi-producer, single-consumer FIFO queue communication primitives.

pub use std::sync::mpsc::{TrySendError, SendError, TryRecvError, RecvError};

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

use coroutine::Handle;
use runtime::Processor;
use scheduler::Scheduler;

#[derive(Clone)]
pub struct Sender<T> {
    inner: Option<mpsc::Sender<T>>,

    wait_list: Arc<Mutex<VecDeque<Handle>>>,
}

unsafe impl<T: Send> Send for Sender<T> {}

impl<T> Sender<T> {
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        match self.inner.as_ref().unwrap().send(t) {
            Ok(..) => {
                let mut wait_list = self.wait_list.lock().unwrap();
                if let Some(coro) = wait_list.pop_front() {
                    Scheduler::ready(coro);
                }
                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // Drop the inner Sender first
        {
            self.inner.take();
        }

        // Try to wake up all the pending coroutines if this is the last Sender.
        // Because if this is the last Sender, there won't be another one to push
        // items into this queue, so we have to wake the coroutine up explicitly,
        // who ownes the other end of this channel.
        if Arc::strong_count(&self.wait_list) <= 2 {
            let mut wait_list = self.wait_list.lock().unwrap();
            for hdl in wait_list.drain(..) {
                Scheduler::ready(hdl);
            }
        }
    }
}

pub struct Receiver<T> {
    inner: mpsc::Receiver<T>,

    wait_list: Arc<Mutex<VecDeque<Handle>>>,
}

unsafe impl<T: Send> Send for Receiver<T> {}

impl<T> Receiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        while let Some(mut processor) = Processor::current() {
            let processor_ptr = unsafe { processor.mut_ptr() };

            // 1. Try to receive first
            let mut r = self.try_recv();
            match r {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }

            // 2. Yield
            processor.take_current_coroutine(|coro| {
                // 3. Lock the wait list
                let mut wait_list = self.wait_list.lock().unwrap();

                // 4. Try to receive again, to ensure no one sent items into the queue while
                //    we are locking the wait list
                r = self.try_recv();

                match r {
                    Err(TryRecvError::Empty) => {
                        // 5.1. Push ourselves into the wait list
                        wait_list.push_back(coro);
                    }
                    _ => {
                        // 5.2. Success!
                        unsafe { &mut *processor_ptr }.ready(coro);
                    }
                }
            });

            // 6. Check it again after being waken up (if 5.2 succeeded)
            match r {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }
        }

        // What? The processor is gone? Then fallback to blocking recv
        self.inner.recv()
    }
}

/// Create a channel pair
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel();
    let wait_list = Arc::new(Mutex::new(VecDeque::new()));

    let sender = Sender {
        inner: Some(tx),
        wait_list: wait_list.clone(),
    };

    let receiver = Receiver {
        inner: rx,
        wait_list: wait_list,
    };

    (sender, receiver)
}

#[derive(Clone)]
pub struct SyncSender<T> {
    inner: Option<mpsc::SyncSender<T>>,

    send_wait_list: Arc<Mutex<VecDeque<Handle>>>,
    recv_wait_list: Arc<Mutex<VecDeque<Handle>>>,
}

unsafe impl<T: Send> Send for SyncSender<T> {}

impl<T> SyncSender<T> {
    pub fn try_send(&self, t: T) -> Result<(), TrySendError<T>> {
        match self.inner.as_ref().unwrap().try_send(t) {
            Ok(..) => {
                let mut recv_wait_list = self.recv_wait_list.lock().unwrap();
                if let Some(coro) = recv_wait_list.pop_front() {
                    Scheduler::ready(coro);
                }
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub fn send(&self, mut t: T) -> Result<(), SendError<T>> {
        while let Some(mut processor) = Processor::current() {
            let processor_ptr = unsafe { processor.mut_ptr() };
            let mut r = self.try_send(t);

            match r {
                Ok(..) => return Ok(()),
                Err(TrySendError::Disconnected(e)) => return Err(SendError(e)),
                Err(TrySendError::Full(t)) => {
                    r = processor.take_current_coroutine(move |coro| {
                        let mut send_wait_list = self.send_wait_list.lock().unwrap();
                        let r = self.try_send(t);

                        match r {
                            Err(TrySendError::Full(..)) => {
                                send_wait_list.push_back(coro);
                            }
                            _ => {
                                unsafe { &mut *processor_ptr }.ready(coro);
                            }
                        };

                        r
                    });
                }
            }

            match r {
                Ok(..) => return Ok(()),
                Err(TrySendError::Disconnected(e)) => return Err(SendError(e)),
                Err(TrySendError::Full(t_bak)) => {
                    t = t_bak; // Restore t
                }
            }
        }

        // What? The processor is gone? Then use blocking send
        match self.inner.as_ref().unwrap().send(t) {
            Ok(..) => {
                let mut recv_wait_list = self.recv_wait_list.lock().unwrap();
                if let Some(coro) = recv_wait_list.pop_front() {
                    // Wake them up ...
                    Scheduler::ready(coro);
                }
                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}

impl<T> Drop for SyncSender<T> {
    fn drop(&mut self) {
        // Drop the inner SyncSender first
        {
            self.inner.take();
        }

        // Try to wake up all the pending coroutines if this is the last SyncSender.
        // Because if this is the last SyncSender, there won't be another one to push
        // items into this queue, so we have to wake the coroutine up explicitly,
        // who ownes the other end of this channel.
        if Arc::strong_count(&self.recv_wait_list) <= 2 {
            let mut recv_wait_list = self.recv_wait_list.lock().unwrap();
            for hdl in recv_wait_list.drain(..) {
                Scheduler::ready(hdl);
            }
        }
    }
}

pub struct SyncReceiver<T> {
    inner: Option<mpsc::Receiver<T>>,

    send_wait_list: Arc<Mutex<VecDeque<Handle>>>,
    recv_wait_list: Arc<Mutex<VecDeque<Handle>>>,
}

unsafe impl<T: Send> Send for SyncReceiver<T> {}

impl<T> SyncReceiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match self.inner.as_ref().unwrap().try_recv() {
            Ok(t) => {
                let mut send_wait_list = self.send_wait_list.lock().unwrap();
                if let Some(coro) = send_wait_list.pop_front() {
                    Scheduler::ready(coro);
                }
                Ok(t)
            }
            Err(err) => Err(err),
        }
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        while let Some(mut processor) = Processor::current() {
            let processor_ptr = unsafe { processor.mut_ptr() };
            let mut r = self.try_recv();

            match r {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }

            processor.take_current_coroutine(|coro| {
                let mut recv_wait_list = self.recv_wait_list.lock().unwrap();

                r = self.try_recv();

                match r {
                    Err(TryRecvError::Empty) => {
                        recv_wait_list.push_back(coro);
                    }
                    _ => {
                        unsafe { &mut *processor_ptr }.ready(coro);
                    }
                }
            });

            match r {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }
        }

        // What? The processor is gone? Then use blocking recv
        match self.inner.as_ref().unwrap().recv() {
            Ok(t) => {
                let mut send_wait_list = self.send_wait_list.lock().unwrap();
                if let Some(coro) = send_wait_list.pop_front() {
                    Scheduler::ready(coro);
                }
                Ok(t)
            }
            Err(err) => Err(err),
        }
    }
}

impl<T> Drop for SyncReceiver<T> {
    fn drop(&mut self) {
        // Drop the inner SyncReceiver first
        {
            self.inner.take();
        }

        // Try to wake up all the pending coroutines if this is the last SyncReceiver.
        // Because there won't be another one to push items into this queue, so we
        // have to wake the coroutine up explicitly, who ownes the other end of this channel.
        let mut send_wait_list = self.send_wait_list.lock().unwrap();
        for hdl in send_wait_list.drain(..) {
            Scheduler::ready(hdl);
        }
    }
}

/// Create a bounded channel pair
pub fn sync_channel<T>(bound: usize) -> (SyncSender<T>, SyncReceiver<T>) {
    let (tx, rx) = mpsc::sync_channel(bound);
    let send_wait_list = Arc::new(Mutex::new(VecDeque::new()));
    let recv_wait_list = Arc::new(Mutex::new(VecDeque::new()));

    let sender = SyncSender {
        inner: Some(tx),
        send_wait_list: send_wait_list.clone(),
        recv_wait_list: recv_wait_list.clone(),
    };

    let receiver = SyncReceiver {
        inner: Some(rx),
        send_wait_list: send_wait_list,
        recv_wait_list: recv_wait_list,
    };

    (sender, receiver)
}

#[cfg(test)]
mod test {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use scheduler::Scheduler;

    #[test]
    fn test_channel_basic() {
        Scheduler::new()
            .run(move || {
                let (tx, rx) = channel();

                let h = Scheduler::spawn(move || {
                    assert_eq!(rx.try_recv(), Ok(1));
                    assert_eq!(rx.try_recv(), Ok(2));
                    assert_eq!(rx.try_recv(), Ok(3));

                    for i in 1..10 {
                        assert_eq!(rx.recv(), Ok(i));
                    }
                });

                assert_eq!(tx.send(1), Ok(()));
                assert_eq!(tx.send(2), Ok(()));
                assert_eq!(tx.send(3), Ok(()));

                Scheduler::sched();

                for i in 1..10 {
                    assert_eq!(tx.send(i), Ok(()));
                }

                h.join().unwrap();
            })
            .unwrap();
    }

    #[test]
    fn test_sync_channel_basic() {
        Scheduler::new()
            .run(move || {
                let (tx, rx) = sync_channel(2);

                let h = Scheduler::spawn(move || {
                    assert_eq!(rx.try_recv(), Ok(1));
                    assert_eq!(rx.try_recv(), Ok(2));
                    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

                    for i in 1..10 {
                        assert_eq!(rx.recv(), Ok(i));
                    }
                });

                assert_eq!(tx.try_send(1), Ok(()));
                assert_eq!(tx.try_send(2), Ok(()));
                assert_eq!(tx.try_send(3), Err(TrySendError::Full(3)));

                Scheduler::sched();

                for i in 1..10 {
                    assert_eq!(tx.send(i), Ok(()));
                }

                h.join().unwrap();
            })
            .unwrap();
    }

    #[test]
    fn test_channel_without_processor() {
        let (tx1, rx1) = channel();
        let (tx2, rx2) = channel();
        let barrier = Arc::new(Barrier::new(2));

        {
            let barrier = barrier.clone();

            thread::spawn(move || {
                Scheduler::new()
                    .run(move || {
                        barrier.wait();
                        assert_eq!(rx1.recv(), Ok(1));
                        assert_eq!(tx2.send(2), Ok(()));
                    })
                    .unwrap();
            });
        }

        // ensure that rx1.recv() above has been called
        barrier.wait();
        thread::sleep(Duration::from_millis(10));

        assert_eq!(tx1.send(1), Ok(()));
        assert_eq!(rx2.recv(), Ok(2));
    }

    #[test]
    fn test_sync_channel_without_processor() {
        let (tx1, rx1) = sync_channel(1);
        let (tx2, rx2) = sync_channel(1);
        let barrier = Arc::new(Barrier::new(2));

        {
            let barrier = barrier.clone();

            thread::spawn(move || {
                Scheduler::new()
                    .run(move || {
                        barrier.wait();
                        assert_eq!(rx1.recv(), Ok(1));
                        assert_eq!(tx2.send(2), Ok(()));
                    })
                    .unwrap();
            });
        }

        // ensure that rx1.recv() above has been called
        barrier.wait();
        thread::sleep(Duration::from_millis(10));

        assert_eq!(tx1.send(1), Ok(()));
        assert_eq!(rx2.recv(), Ok(2));
    }

    #[test]
    fn test_channel_passing_ring() {
        Scheduler::new().with_workers(1).run(|| {
            let (tx, mut rx) = channel();

            for _ in 0..100 {
                let (ltx, lrx) = channel();
                Scheduler::spawn(move|| {
                    loop {
                        let value = match rx.recv() {
                            Ok(v) => v,
                            Err(..) => break,
                        };
                        ltx.send(value).unwrap();
                    }
                });

                rx = lrx;
            }

            for i in 0..10 {
                tx.send(i).unwrap();
                let value = rx.recv().unwrap();
                assert_eq!(i, value);
            }
        }).unwrap();
    }

    #[test]
    fn test_sync_channel_passing_ring() {
        Scheduler::new().with_workers(1).run(|| {
            let (tx, mut rx) = sync_channel(1);

            for _ in 0..100 {
                let (ltx, lrx) = sync_channel(1);
                Scheduler::spawn(move|| {
                    loop {
                        let value = match rx.recv() {
                            Ok(v) => v,
                            Err(..) => break,
                        };
                        ltx.send(value).unwrap();
                    }
                });

                rx = lrx;
            }

            for i in 0..10 {
                tx.send(i).unwrap();
                let value = rx.recv().unwrap();
                assert_eq!(i, value);
            }
        }).unwrap();
    }
}
