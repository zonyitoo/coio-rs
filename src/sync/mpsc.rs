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

pub use std::sync::mpsc::{SendError, TryRecvError, RecvError};

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

use coroutine::Coroutine;
use scheduler::Scheduler;
use runtime::processor::Processor;

#[allow(raw_pointer_derive)]
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
                    unsafe {
                        Scheduler::ready(coro);
                    }
                }
                Ok(())
            },
            Err(err) => Err(err)
        }
    }
}

pub struct Receiver<T> {
    inner: mpsc::Receiver<T>,

    wait_list: Arc<Mutex<VecDeque<*mut Coroutine>>>,
}

impl<T> Receiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            // 1. Try receive
            match self.try_recv() {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {},
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }

            {
                // 2. Lock the wait list
                let mut wait_list = self.wait_list.lock().unwrap();

                // 3. Try to receive again, to ensure no one sent items into the queue while
                //    we are locking the wait list
                match self.try_recv() {
                    Ok(v) => return Ok(v),
                    Err(TryRecvError::Empty) => {},
                    Err(TryRecvError::Disconnected) => return Err(RecvError),
                }

                // 4. Push ourselves into the wait list
                wait_list.push_back(unsafe { Processor::current().running()
                                                .expect("A running coroutine is required!") });

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

#[cfg(test)]
mod test {
    use super::*;

    use scheduler::Scheduler;

    #[test]
    fn test_channel_basic() {
        let (tx, rx) = channel();

        Scheduler::with_workers(1)
            .run(move|| {
                tx.send(1).unwrap();
            }).unwrap();

        assert_eq!(1, rx.recv().unwrap());
    }
}
