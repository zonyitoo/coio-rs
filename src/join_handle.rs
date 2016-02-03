use std::any::Any;
use std::sync::Arc;
use std::cell::UnsafeCell;

use sync::mono_barrier::MonoBarrier;

struct JoinHandleInner<T> {
    barrier: MonoBarrier,
    data: UnsafeCell<Option<Result<T, Box<Any + Send + 'static>>>>,
}

unsafe impl<T: Send> Send for JoinHandleInner<T> {}
unsafe impl<T> Sync for JoinHandleInner<T> {}

impl<T> JoinHandleInner<T> {
    fn new() -> JoinHandleInner<T> {
        JoinHandleInner {
            barrier: MonoBarrier::new(),
            data: UnsafeCell::new(None),
        }
    }
}

pub struct JoinHandleSender<T> {
    inner: Arc<JoinHandleInner<T>>,
}

impl<T> JoinHandleSender<T> {
    pub fn push(self, result: Result<T, Box<Any + Send + 'static>>) {
        let data = unsafe { &mut *self.inner.data.get() };
        *data = Some(result);
        self.inner.barrier.signal();
    }
}

pub struct JoinHandleReceiver<T> {
    inner: Arc<JoinHandleInner<T>>,
    received: bool,
}

impl<T> JoinHandleReceiver<T> {
    pub fn pop(mut self) -> Result<T, Box<Any + Send + 'static>> {
        self.inner.barrier.wait().unwrap();
        let data = unsafe { &mut *self.inner.data.get() };
        self.received = true;
        data.take().unwrap()
    }
}

pub fn handle_pair<T>() -> (JoinHandleSender<T>, JoinHandleReceiver<T>) {
    let inner = Arc::new(JoinHandleInner::new());
    let sender = JoinHandleSender { inner: inner.clone() };
    let receiver = JoinHandleReceiver { inner: inner, received: false };
    (sender, receiver)
}

#[cfg(test)]
mod test {
    use super::*;

    use scheduler::Scheduler;

    #[test]
    fn test_join_handle_basic() {
        for _ in 0..10 {
            Scheduler::new()
                .run(|| {
                    let (tx, rx) = handle_pair();

                    Scheduler::spawn(move || {
                        tx.push(Ok(1));
                    });

                    let value = rx.pop().unwrap();
                    assert_eq!(value, 1);
                })
                .unwrap();
        }
    }

    #[test]
    fn test_join_handle_basic2() {
        Scheduler::new()
            .run(|| {
                let mut handles = Vec::new();

                for _ in 0..10 {
                    let (tx, rx) = handle_pair();
                    Scheduler::spawn(move || {
                        tx.push(Ok(1));
                    });
                    handles.push(rx);
                }

                for h in handles {
                    assert_eq!(h.pop().unwrap(), 1);
                }
            })
            .unwrap();
    }

    #[test]
    fn test_join_handle_basic3() {
        Scheduler::new()
            .with_workers(4)
            .run(|| {
                let mut handles = Vec::new();

                for _ in 0..10 {
                    let (tx, rx) = handle_pair();
                    Scheduler::spawn(move || {
                        tx.push(Ok(1));
                    });
                    handles.push(rx);
                }

                for h in handles {
                    assert_eq!(h.pop().unwrap(), 1);
                }
            })
            .unwrap();
    }
}
