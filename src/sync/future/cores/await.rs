// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::sync::Arc;
use std::thread;

use coroutine::Coroutine;
use runtime::processor::Processor;
use scheduler::Scheduler;
use super::CoreWrapper;
use super::super::{ContinueWith, Core};

enum AwaitHandle {
    None,
    Thread(thread::Thread),
    Coroutine(Coroutine),
}

pub struct AwaitCoreInner<To, Eo> {
    result: Option<Result<To, Eo>>,
    handle: AwaitHandle,
}

pub type AwaitCore<To, Eo> = CoreWrapper<AwaitCoreInner<To, Eo>>;

impl<To, Eo> AwaitCore<To, Eo> {
    pub fn new() -> Arc<AwaitCore<To, Eo>> {
        AwaitCore::wrap(AwaitCoreInner {
            result: None,
            handle: AwaitHandle::None,
        })
    }

    pub fn await(&self) -> Option<Result<To, Eo>> {
        let mut inner = self.inner.lock();

        if let Some(res) = inner.result.take() {
            return res;
        } else {
            if let Some(p) = Processor::current() {
                p.park_with(move |_, coro| {
                    inner.handle = AwaitHandle::Coroutine(coro);
                    drop(inner);
                });
            } else {
                inner.handle = AwaitHandle::Thread(thread::current());
                drop(inner);
                thread::park();
            }
        }

        self.inner.lock().result.take().expect("Await resulted in empty Result")
    }
}


impl<To, Eo> ContinueWith<To, Eo> for AwaitCore<To, Eo> {
    fn continue_with(&self, val: Result<To, Eo>) {
        let mut inner = self.inner.lock();
        inner.result = Some(val);

        match mem::replace(&mut inner.handle, AwaitHandle::None) {
            None => {}
            Thread(thread) => thread.unpark(),
            Coroutine(coro) => Scheduler::ready(coro),
        }
    }
}

impl<To, Eo> Core<To, Eo> for AwaitCore<To, Eo> {
    fn set_next(&self, _: Arc<ContinueWith<To, Eo>>) {
        unimplemented!();
    }

    fn settle(&self, _: Result<To, Eo>) {
        unimplemented!();
    }
}
