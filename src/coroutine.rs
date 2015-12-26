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

use std::cell::UnsafeCell;
use std::boxed::FnBox;

use libc;

use sema::Semaphore;

use context::{Context, Stack};
use context::stack::StackPool;

use runtime::processor::Processor;
use options::Options;

thread_local!(static STACK_POOL: UnsafeCell<StackPool> = UnsafeCell::new(StackPool::new()));

/// Initialization function for make context
extern "C" fn coroutine_initialize(_: usize, f: *mut libc::c_void) -> ! {
    unsafe {
        let func: Box<Box<FnBox()>> = Box::from_raw(f as *mut Box<FnBox()>);
        func()
    };

    let processor = Processor::current();
    processor.yield_with(Ok(State::Finished));

    unreachable!("Should not reach here");
}

pub type Handle = Box<Coroutine>;

/// Coroutine is nothing more than a context and a stack
pub struct Coroutine {
    context: Context,
    stack: Option<Stack>,
    pub yield_lock: Semaphore,
}

impl Coroutine {
    pub unsafe fn empty() -> Handle {
        Box::new(Coroutine {
            context: Context::empty(),
            stack: None,
            yield_lock: Semaphore::new(1),
        })
    }

    pub fn spawn_opts(f: Box<FnBox()>, opts: Options) -> Handle {
        let mut stack = STACK_POOL.with(|pool| unsafe {
            (&mut *pool.get()).take_stack(opts.stack_size)
        });

        let ctx = Context::new(coroutine_initialize,
                               0,
                               Box::into_raw(Box::new(f)) as *mut libc::c_void,
                               &mut stack);
        Box::new(Coroutine {
            context: ctx,
            stack: Some(stack),
            yield_lock: Semaphore::new(1),
        })
    }

    pub fn yield_to(&mut self, target: &Coroutine) {
        Context::swap(&mut self.context, &target.context);
    }
}

impl Drop for Coroutine {
    fn drop(&mut self) {
        match self.stack.take() {
            None => {}
            Some(st) => {
                STACK_POOL.with(|pool| unsafe {
                    let pool: &mut StackPool = &mut *pool.get();
                    pool.give_stack(st);
                })
            }
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum State {
    Suspended,
    Blocked,
    Finished,
}

pub type Result<T> = ::std::result::Result<T, ()>;

/// Sendable coroutine mutable pointer
#[derive(Copy, Clone, Debug)]
pub struct SendableCoroutinePtr(pub *mut Coroutine);

unsafe impl Send for SendableCoroutinePtr {}
