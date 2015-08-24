pub use std::sync::mpsc::{SendError, TryRecvError, RecvError};

use std::sync::mpsc;

use sched;

#[derive(Clone)]
pub struct Sender<T> {
    inner: mpsc::Sender<T>,
}

unsafe impl<T: Send> Send for Sender<T> {}

impl<T> Sender<T> {
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        self.inner.send(t)
    }
}

pub struct Receiver<T> {
    inner: mpsc::Receiver<T>,
}

impl<T> Receiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            match self.try_recv() {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {},
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }

            sched();
        }
    }
}

/// Create a channel pair
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel();
    (Sender { inner: tx }, Receiver { inner: rx })
}

#[cfg(test)]
mod test {
    use super::*;

    use {spawn, run};

    #[test]
    fn test_channel_basic() {
        let (tx, rx) = channel();
        spawn(move|| {
            tx.send(1).unwrap();
        });

        run(1);

        assert_eq!(1, rx.recv().unwrap());
    }
}
