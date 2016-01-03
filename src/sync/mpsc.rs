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

use coroutine::Coroutine;
use scheduler::Scheduler;
use runtime::processor::Processor;

#[derive(Clone)]
pub struct Sender<T> {
    inner: mpsc::Sender<T>,

    wait_list: Arc<Mutex<VecDeque<*mut Coroutine>>>,
}

unsafe impl<T: Send> Send for Sender<T> {}

impl<T> Sender<T> {
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        match self.inner.send(t) {
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

pub struct Receiver<T> {
    inner: mpsc::Receiver<T>,

    wait_list: Arc<Mutex<VecDeque<*mut Coroutine>>>,
}

unsafe impl<T: Send> Send for Receiver<T> {}

impl<T> Receiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            // 1. Try receive
            match self.try_recv() {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }

            {
                // 2. Lock the wait list
                let mut wait_list = self.wait_list.lock().unwrap();

                // 3. Try to receive again, to ensure no one sent items into the queue while
                //    we are locking the wait list
                match self.try_recv() {
                    Ok(v) => return Ok(v),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => return Err(RecvError),
                }

                // 4. Push ourselves into the wait list
                wait_list.push_back(Processor::current_running()
                                        .expect("A running coroutine is required!"));

                // 5. Release the wait list
            }

            // 6. Yield
            Scheduler::block();
        }
    }
}

/// Create a channel pair
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel();
    let wait_list = Arc::new(Mutex::new(VecDeque::new()));

    let sender = Sender {
        inner: tx,
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
    inner: mpsc::SyncSender<T>,

    send_wait_list: Arc<Mutex<VecDeque<*mut Coroutine>>>,
    recv_wait_list: Arc<Mutex<VecDeque<*mut Coroutine>>>,
}

unsafe impl<T: Send> Send for SyncSender<T> {}

impl<T> SyncSender<T> {
    pub fn try_send(&self, t: T) -> Result<(), TrySendError<T>> {
        match self.inner.try_send(t) {
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

    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        let mut t2 = t;

        loop {
            // 1. Try send
            match self.try_send(t2) {
                Ok(..) => return Ok(()),
                Err(TrySendError::Disconnected(e)) => return Err(SendError(e)),
                Err(TrySendError::Full(e)) => t2 = e,
            }

            {
                // 2. Lock the wait list
                let mut send_wait_list = self.send_wait_list.lock().unwrap();

                // 3. Try to send again, to ensure no one received items from the queue while
                //    we are locking the wait list
                match self.try_send(t2) {
                    Ok(..) => return Ok(()),
                    Err(TrySendError::Disconnected(e)) => return Err(SendError(e)),
                    Err(TrySendError::Full(e)) => t2 = e,
                }

                // 4. Push ourselves into the wait list
                send_wait_list.push_back(Processor::current_running()
                                             .expect("A running coroutine is required!"));

                // 5. Release the wait list
            }

            // 6. Yield
            Scheduler::block();
        }
    }
}

pub struct SyncReceiver<T> {
    inner: mpsc::Receiver<T>,

    send_wait_list: Arc<Mutex<VecDeque<*mut Coroutine>>>,
    recv_wait_list: Arc<Mutex<VecDeque<*mut Coroutine>>>,
}

unsafe impl<T: Send> Send for SyncReceiver<T> {}

impl<T> SyncReceiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match self.inner.try_recv() {
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
        loop {
            // 1. Try receive
            match self.try_recv() {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }

            {
                // 2. Lock the wait list
                let mut recv_wait_list = self.recv_wait_list.lock().unwrap();

                // 3. Try to receive again, to ensure no one sent items into the queue while
                //    we are locking the wait list
                match self.try_recv() {
                    Ok(v) => return Ok(v),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => return Err(RecvError),
                }

                // 4. Push ourselves into the wait list
                recv_wait_list.push_back(Processor::current_running()
                                             .expect("A running coroutine is required!"));

                // 5. Release the wait list
            }

            // 6. Yield
            Scheduler::block();
        }
    }
}

/// Create a bounded channel pair
pub fn sync_channel<T>(bound: usize) -> (SyncSender<T>, SyncReceiver<T>) {
    let (tx, rx) = mpsc::sync_channel(bound);
    let send_wait_list = Arc::new(Mutex::new(VecDeque::new()));
    let recv_wait_list = Arc::new(Mutex::new(VecDeque::new()));

    let sender = SyncSender {
        inner: tx,
        send_wait_list: send_wait_list.clone(),
        recv_wait_list: recv_wait_list.clone(),
    };

    let receiver = SyncReceiver {
        inner: rx,
        send_wait_list: send_wait_list,
        recv_wait_list: recv_wait_list,
    };

    (sender, receiver)
}

#[cfg(test)]
mod test {
    use super::*;

    use scheduler::Scheduler;

    #[test]
    fn test_channel_basic() {
        let (tx, rx) = channel();

        Scheduler::new()
            .run(move || {
                tx.send(1).unwrap();
            })
            .unwrap();

        assert_eq!(1, rx.recv().unwrap());
    }

    #[test]
    fn test_sync_channel_basic() {
        Scheduler::new()
            .run(move || {
                let (tx, rx) = sync_channel(2);

                {
                    let tx = tx.clone();

                    Scheduler::spawn(move || {
                        assert_eq!(tx.try_send(1), Ok(()));
                        assert_eq!(tx.try_send(2), Ok(()));
                        assert_eq!(tx.try_send(3), Err(TrySendError::Full(3)));
                    });
                }

                assert_eq!(rx.try_recv(), Ok(1));
                assert_eq!(rx.try_recv(), Ok(2));
                assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

                {
                    let tx = tx.clone();

                    Scheduler::spawn(move || {
                        for i in 1..10 {
                            assert_eq!(tx.send(i), Ok(()));
                        }
                    });
                }

                Scheduler::instance().unwrap().sleep_ms(100);

                for i in 1..10 {
                    assert_eq!(rx.recv(), Ok(i));
                }
            })
            .unwrap();
    }
}
