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

use std::boxed::FnBox;
use std::cell::UnsafeCell;
use std::ptr;

use libc;

use context::{Context, Stack};
use context::stack::StackPool;

use runtime::processor::{Processor, WeakProcessor};
use options::Options;

thread_local!(static STACK_POOL: UnsafeCell<StackPool> = UnsafeCell::new(StackPool::new()));

#[derive(Debug)]
pub struct ForceUnwind;

/// Initialization function for make context
extern "C" fn coroutine_initialize(_: usize, f: *mut libc::c_void) -> ! {
    let coro: &mut Coroutine = unsafe { &mut *(f as *mut Coroutine) };
    {
        if let Some(f) = coro.runnable.take() {
            f();
        }
    }

    unsafe {
        debug_assert!(coro.final_yield_to != ptr::null_mut(),
                      "Final yield to target is a nullptr, which is impossible");
        let final_yield_to: &mut Coroutine = &mut *coro.final_yield_to;
        coro.yield_to(State::Finished, final_yield_to);
    }
    unreachable!();
}

pub type Handle = Box<Coroutine>;

/// Coroutine is nothing more than a context and a stack
pub struct Coroutine {
    context: Context,
    stack: Option<Stack>,
    preferred_processor: Option<WeakProcessor>,
    state: State,
    runnable: Option<Box<FnBox()>>,
    name: Option<String>,
    final_yield_to: *mut Coroutine,
}

unsafe impl Send for Coroutine {}

impl Coroutine {
    fn new(ctx: Context, stack: Option<Stack>, runnable: Option<Box<FnBox()>>) -> Handle {
        Box::new(Coroutine {
            context: ctx,
            stack: stack,
            preferred_processor: None,
            state: State::Initialized,
            runnable: runnable,
            name: None,
            final_yield_to: ptr::null_mut(),
        })
    }

    pub unsafe fn empty() -> Handle {
        Coroutine::new(Context::empty(), None, None)
    }

    unsafe fn empty_on_stack() -> Coroutine {
        Coroutine {
            context: Context::empty(),
            stack: None,
            preferred_processor: None,
            state: State::Initialized,
            runnable: None,
            name: None,
            final_yield_to: ptr::null_mut(),
        }
    }

    pub fn spawn_opts(f: Box<FnBox()>, opts: Options) -> Handle {
        let stack = STACK_POOL.with(|pool| unsafe {
            (&mut *pool.get()).take_stack(opts.stack_size)
        });

        let mut coro = Coroutine::new(Context::empty(), Some(stack), Some(f));
        coro.name = opts.name;
        let coro_ptr: *mut Coroutine = &mut *coro as *mut Coroutine;
        let stack_ptr: *mut Stack = coro.stack.as_mut().unwrap();
        coro.context.init_with(coroutine_initialize,
                               0,
                               coro_ptr as *mut libc::c_void,
                               unsafe { &mut *stack_ptr });
        coro
    }

    pub fn yield_to(&mut self, state: State, target: &mut Coroutine) {
        self.set_state(state);
        target.set_state(State::Running);
        unsafe {
            self.raw_yield_to(target);
        }
    }

    pub unsafe fn raw_yield_to(&mut self, target: &mut Coroutine) {
        target.final_yield_to = self;
        Context::swap(&mut self.context, &target.context);

        if let State::ForceUnwinding = self.state() {
            panic!(ForceUnwind);
        }
    }

    pub fn resume(state: State, target: &mut Coroutine) {
        unsafe {
            let mut dummy = Coroutine::empty_on_stack();
            target.set_state(state);
            dummy.raw_yield_to(target);
        }
    }

    pub fn set_preferred_processor(&mut self, preferred_processor: Option<WeakProcessor>) {
        self.preferred_processor = preferred_processor;
    }

    pub fn preferred_processor(&self) -> Option<Processor> {
        self.preferred_processor.as_ref().and_then(|p| p.upgrade())
    }

    #[inline]
    pub fn state(&self) -> State {
        self.state
    }

    #[inline]
    pub fn set_state(&mut self, state: State) {
        self.state = state
    }

    #[inline]
    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|x| &x[..])
    }

    #[inline]
    pub fn name_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.name().unwrap_or(default)
    }
}

impl Drop for Coroutine {
    fn drop(&mut self) {
        trace!("Dropping Coroutine `{}` with state: {:?}", self.name_or("<unnamed>"), self.state());

        unsafe {
            match self.state() {
                State::Initialized | State::Finished => {}
                _ => {
                    // Unwind the stack only if it actually has a stack!
                    if self.stack.is_some() {
                        if let Some(p) = Processor::current() {
                            trace!("Coroutine `{}` is force-unwinding with processors", self.name_or("<unnamed>"));
                            p.toggle_unwinding(self);
                        } else {
                            // This would happen if all the processors are gone just before
                            // the container that holding the Handle of this coroutine is dropping
                            trace!("Coroutine `{}` is force-unwinding without processors", self.name_or("<unnamed>"));
                            Coroutine::resume(State::ForceUnwinding, self);
                        }
                    }
                }
            }
        }

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

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum State {
    Initialized,
    Suspended,
    Running,
    Blocked,
    Finished,
    ForceUnwinding,
}

pub type Result<T> = ::std::result::Result<T, ()>;

/// Sendable coroutine mutable pointer
#[derive(Copy, Clone, Debug)]
pub struct SendableCoroutinePtr(pub *mut Coroutine);
unsafe impl Send for SendableCoroutinePtr {}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use scheduler::Scheduler;

    #[test]
    fn coroutine_unwinds_on_drop() {
        let shared_usize = Arc::new(AtomicUsize::new(0));

        {
            let shared_usize = shared_usize.clone();

            Scheduler::new()
                .run(|| {
                    let handle = Scheduler::spawn(|| {
                        struct Test(Arc<AtomicUsize>);

                        impl Drop for Test {
                            fn drop(&mut self) {
                                self.0.store(1, Ordering::SeqCst);
                            }
                        }

                        let test = Test(shared_usize);
                        Scheduler::take_current_coroutine(|_| {});
                        test.0.store(2, Ordering::SeqCst);
                    });

                    let _ = handle.join();
                })
                .unwrap();
        }

        assert_eq!(shared_usize.load(Ordering::SeqCst), 1);
    }
}
