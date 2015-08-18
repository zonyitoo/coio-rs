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

use std::rt;
use std::cell::UnsafeCell;
use std::mem;
use std::any::Any;

use context::{Context, Stack};
use context::stack::StackPool;
use context::thunk::Thunk;

use processor::Processor;
use options::Options;

thread_local!(static STACK_POOL: UnsafeCell<StackPool> = UnsafeCell::new(StackPool::new()));

/// Initialization function for make context
extern "C" fn coroutine_initialize(_: usize, f: *mut ()) -> ! {
    let ret = unsafe {
        let func: Box<Thunk> = mem::transmute(f);
        rt::unwind::try(move|| func.invoke(()))
    };

    let processor = Processor::current();

    let st = match ret {
        Ok(..) => {
            processor.yield_with(Ok(State::Finished));
            State::Finished
        },
        Err(err) => {
            processor.yield_with(Err(Error::Panicking(err)));
            State::Panicked
        }
    };

    loop {
        processor.yield_with(Ok(st));
    }
}

pub type Handle = Box<Coroutine>;

/// Coroutine is nothing more than a context and a stack
pub struct Coroutine {
    context: Context,
    stack: Option<Stack>,
}

impl Coroutine {
    pub unsafe fn empty() -> Handle {
        Box::new(Coroutine {
            context: Context::empty(),
            stack: None,
        })
    }

    pub fn spawn_opts<F>(f: F, opts: Options) -> Handle
        where F: FnOnce() + Send + 'static
    {
        let mut stack = STACK_POOL.with(|pool| unsafe {
            (&mut *pool.get()).take_stack(opts.stack_size)
        });

        let ctx = Context::new(coroutine_initialize, 0, f, &mut stack);
        Box::new(Coroutine {
            context: ctx,
            stack: Some(stack),
        })
    }

    pub fn yield_to(&mut self, target: &Coroutine) {
        Context::swap(&mut self.context, &target.context);
    }
}

impl Drop for Coroutine {
    fn drop(&mut self) {
        match self.stack.take() {
            None => {},
            Some(st) => {
                STACK_POOL.with(|pool| unsafe {
                    let pool: &mut StackPool = mem::transmute(pool.get());
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
    Panicked,
    Finished,
}

#[derive(Debug)]
pub enum Error {
    Panicking(Box<Any + Send>),
}

pub type Result<T> = ::std::result::Result<T, Error>;
