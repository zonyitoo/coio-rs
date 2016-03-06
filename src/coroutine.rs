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
use std::fmt;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::panic;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::usize;

use context::{Context, Transfer};
use context::stack::ProtectedFixedSizeStack;

use runtime::processor::{Processor, WeakProcessor};
use options::Options;

extern "C" fn coroutine_entry(t: Transfer) -> ! {
    // Take over the data from Coroutine::spawn_opts
    let InitData { stack, callback } = unsafe {
        let data_opt_ref = &mut *(t.data as *mut Option<InitData>);
        data_opt_ref.take().unwrap()
    };

    // This block will ensure the `meta` will be destroied before dropping the stack
    let ctx = {
        let mut meta = Coroutine {
            context: None,
            global_work_count: None,
            name: None,
            preferred_processor: None,
            state: State::Initialized,
        };

        // Yield back after take out the callback function
        // Now the Coroutine is initialized
        let meta_ptr = &mut meta as *mut _ as usize;
        let _ = unsafe {
            ::try(move || {
                let Transfer { context: ctx, .. } = t.context.resume(meta_ptr);
                let meta_ref = &mut *(meta_ptr as *mut Coroutine);
                meta_ref.context = Some(ctx);

                // Take out the callback and run it
                callback();

                trace!("Coroutine `{}` finished its callback, going to cleanup",
                       meta_ref.debug_name());
            })
        };

        trace!("Coroutine `{}` is going to drop its stack",
               meta.debug_name());

        // If panicked inside, the meta.context stores the actual return Context
        meta.context.take().unwrap()
    };

    // Drop the stack after it is finished
    let mut stack_opt = Some(stack);
    ctx.resume_ontop(&mut stack_opt as *mut _ as usize, coroutine_exit);

    unreachable!();
}

extern "C" fn coroutine_exit(mut t: Transfer) -> Transfer {
    unsafe {
        // Drop the stack
        let stack_ref = &mut *(t.data as *mut Option<ProtectedFixedSizeStack>);
        let _ = stack_ref.take();
    }

    t.data = usize::MAX;
    t
}

extern "C" fn coroutine_unwind(t: Transfer) -> Transfer {
    // Save the Context in the Coroutine object
    // because the `t` won't be able to be passed to the caller
    unsafe {
        let coro = &mut *(t.data as *mut Coroutine);
        coro.context = Some(t.context);
    }

    panic::propagate(Box::new(ForceUnwind));
}

#[derive(Debug)]
pub struct ForceUnwind;

struct InitData {
    stack: ProtectedFixedSizeStack,
    callback: Box<FnBox()>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum State {
    Initialized,
    Suspended,
    Running,
    Blocked,
}

/// Coroutine is nothing more than a context and a stack
pub struct Coroutine {
    context: Option<Context>,
    global_work_count: Option<Arc<AtomicUsize>>,
    name: Option<String>,
    preferred_processor: Option<WeakProcessor>,
    state: State,
}

unsafe impl Send for Coroutine {}

impl Drop for Coroutine {
    fn drop(&mut self) {
        if let Some(gwc) = self.global_work_count.as_ref() {
            gwc.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Coroutine {
    pub fn attach(&mut self, gwc: Arc<AtomicUsize>) {
        gwc.fetch_add(1, Ordering::SeqCst);
        self.global_work_count = Some(gwc);
    }

    #[inline]
    pub fn spawn_opts<F>(f: F, opts: Options) -> Handle
        where F: FnOnce() + Send + 'static
    {
        Self::spawn_opts_impl(Box::new(f), opts)
    }

    fn spawn_opts_impl(f: Box<FnBox()>, opts: Options) -> Handle {
        let data = InitData {
            stack: ProtectedFixedSizeStack::new(opts.stack_size).unwrap(),
            callback: f,
        };

        let context = Context::new(&data.stack, coroutine_entry);

        // Give him the initialization data
        let mut data_opt = Some(data);
        let t = context.resume(&mut data_opt as *mut _ as usize);
        debug_assert!(data_opt.is_none());

        let coro_ref = unsafe { &mut *(t.data as *mut Coroutine) };
        coro_ref.context = Some(t.context);

        if let Some(name) = opts.name {
            coro_ref.set_name(name);
        }

        // Done!
        Handle(coro_ref)
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
    pub fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|x| &x[..])
    }

    pub fn debug_name(&self) -> &str {
        self.name().unwrap_or("<unnamed>")
    }
}

/// Sendable coroutine mutable pointer
#[derive(Copy, Clone, Debug)]
pub struct SendableCoroutinePtr(pub *mut Coroutine);
unsafe impl Send for SendableCoroutinePtr {}

/// Handle for a Coroutine
pub struct Handle(*mut Coroutine);

impl Handle {
    #[doc(hidden)]
    #[inline]
    pub fn into_raw(self) -> *mut Coroutine {
        let coro = self.0;
        mem::forget(self);
        coro
    }

    #[doc(hidden)]
    #[inline]
    pub unsafe fn from_raw(coro: *mut Coroutine) -> Handle {
        Handle(coro)
    }

    /// Check if the Coroutine is already finished
    #[doc(hidden)]
    #[inline]
    pub fn is_finished(&self) -> bool {
        self.0 == ptr::null_mut()
    }

    /// Resume the Coroutine
    #[doc(hidden)]
    pub fn resume(&mut self, data: usize) -> usize {
        debug_assert!(data != usize::MAX);
        self.yield_with(State::Running, data)
    }

    /// Yield the Coroutine to its parent with data
    #[inline(never)]
    pub fn yield_with(&mut self, state: State, data: usize) -> usize {
        debug_assert!(data != usize::MAX);

        let coro = unsafe { &mut *self.0 };
        coro.set_state(state);

        let context = coro.context.take().unwrap();
        let Transfer { context, data } = context.resume(data);

        // We've returned from a yield to the Processor, because it resume()d us!
        // `context` is the Context of the Processor which we store so we can yield back to it.
        if data != usize::MAX {
            coro.context = Some(context);
        } else {
            self.0 = ptr::null_mut();
        }

        data
    }
}

unsafe impl Send for Handle {}

impl Deref for Handle {
    type Target = Coroutine;
    fn deref(&self) -> &Coroutine {
        unsafe { &*self.0 }
    }
}

impl DerefMut for Handle {
    fn deref_mut(&mut self) -> &mut Coroutine {
        unsafe { &mut *self.0 }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.is_finished() {
            return;
        }

        trace!("Force-unwind Coroutine `{}`", self.debug_name());

        let ctx = self.context.take().unwrap();
        ctx.resume_ontop(self.0 as *mut Coroutine as usize, coroutine_unwind);
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.is_finished() {
            write!(f, "Coroutine(.., Finished)")
        } else {
            write!(f, "Coroutine({}, {:?})", self.debug_name(), self.state())
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use scheduler::Scheduler;
    use options::Options;

    use super::*;

    #[test]
    fn coroutine_unwinds_on_drop() {
        let shared_usize = Arc::new(AtomicUsize::new(0));

        {
            let shared_usize = shared_usize.clone();

            Scheduler::new()
                .run(|| {
                    let handle = Scheduler::spawn(move || {
                        struct Test(Arc<AtomicUsize>);

                        impl Drop for Test {
                            fn drop(&mut self) {
                                self.0.store(1, Ordering::SeqCst);
                            }
                        }

                        let test = Test(shared_usize);

                        Scheduler::block_with(|_, coro| drop(coro));
                        test.0.store(2, Ordering::SeqCst);
                    });

                    let result = handle.join();
                    assert!(result.is_err());
                })
                .unwrap();
        }

        assert_eq!(shared_usize.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn coroutine_drop_after_initialized() {
        let f = || {};
        let coro = Coroutine::spawn_opts(f, Options::default());
        drop(coro);
    }
}
