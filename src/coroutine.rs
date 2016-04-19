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
use std::usize;

use context::{Context, Transfer};
use context::stack::ProtectedFixedSizeStack;

use runtime::processor::{Processor, WeakProcessor};
use options::Options;

extern "C" fn coroutine_entry(t: Transfer) -> ! {
    // Take over the data from Coroutine::spawn_opts
    let InitData { stack, callback } = unsafe {
        let data_opt_ref = &mut *(t.data as *mut Option<InitData>);
        data_opt_ref.take().expect("failed to acquire InitData")
    };

    // This block will ensure the `meta` will be destroied before dropping the stack
    let ctx = {
        let mut meta = Coroutine {
            context: None,
            name: None,
            preferred_processor: None,
            state: State::Suspended,
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

                trace!("Coroutine `{}`: returned from callback",
                       meta_ref.debug_name());
            })
        };

        trace!("Coroutine `{}`: finished => dropping stack",
               meta.debug_name());

        // If panicked inside, the meta.context stores the actual return Context
        meta.take_context()
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
    let coro = unsafe { &mut *(t.data as *mut Coroutine) };

    coro.context = Some(t.context);

    trace!("Coroutine `{}`: unwinding", coro.debug_name());
    panic::resume_unwind(Box::new(ForceUnwind));
}

#[derive(Debug)]
pub struct ForceUnwind;

struct InitData {
    stack: ProtectedFixedSizeStack,
    callback: Box<FnBox()>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum State {
    Suspended,
    Running,
    Parked,
}

/// Coroutine is nothing more than a context and a stack
pub struct Coroutine {
    context: Option<Context>,
    name: Option<String>,
    preferred_processor: Option<WeakProcessor>,
    state: State,
}

unsafe impl Send for Coroutine {}

impl Coroutine {
    #[inline]
    pub fn spawn_opts<F>(f: F, opts: Options) -> Handle
        where F: FnOnce() + Send + 'static
    {
        Self::spawn_opts_impl(Box::new(f), opts)
    }

    fn spawn_opts_impl(f: Box<FnBox()>, opts: Options) -> Handle {
        let data = InitData {
            stack: ProtectedFixedSizeStack::new(opts.stack_size).expect("failed to acquire stack"),
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

        ::global_work_count_add();

        // Done!
        Handle(coro_ref)
    }

    #[inline]
    pub fn set_preferred_processor(&mut self, preferred_processor: Option<WeakProcessor>) {
        self.preferred_processor = preferred_processor;
    }

    #[inline]
    pub fn preferred_processor(&self) -> Option<Processor> {
        self.preferred_processor.as_ref().and_then(WeakProcessor::upgrade)
    }

    #[inline]
    pub fn state(&self) -> State {
        self.state
    }

    #[inline]
    pub fn name(&self) -> Option<&String> {
        self.name.as_ref()
    }

    #[inline]
    pub fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    #[inline]
    pub fn debug_name(&self) -> String {
        match self.name {
            Some(ref name) => name.clone(),
            None => format!("{:p}", self),
        }
    }

    #[inline]
    fn take_context(&mut self) -> Context {
        self.context.take().expect("failed to take context")
    }
}

#[cfg(debug_assertions)]
impl Drop for Coroutine {
    fn drop(&mut self) {
        ::global_work_count_sub();
    }
}

/// Handle for a Coroutine
#[derive(Eq, PartialEq)]
pub struct Handle(*mut Coroutine);

impl Handle {
    pub unsafe fn empty() -> Handle {
        Handle(ptr::null_mut())
    }

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
    #[inline]
    pub fn resume(&mut self, data: usize) -> usize {
        self.yield_with(State::Running, data)
    }

    /// Yields the Coroutine to the Processor
    #[inline(never)]
    pub fn yield_with(&mut self, state: State, data: usize) -> usize {
        debug_assert!(data != usize::MAX);
        assert!(self.0 != ptr::null_mut());

        let coro = unsafe { &mut *self.0 };
        let context = coro.take_context();

        trace!("Coroutine `{}`: yielding to {:?}",
               coro.debug_name(),
               &context);

        coro.state = state;

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

    #[inline]
    fn deref(&self) -> &Coroutine {
        unsafe { &*self.0 }
    }
}

impl DerefMut for Handle {
    #[inline]
    fn deref_mut(&mut self) -> &mut Coroutine {
        unsafe { &mut *self.0 }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.is_finished() {
            return;
        }

        trace!("Coroutine `{}`: force unwinding", self.debug_name());

        let ctx = self.take_context();
        let coro = mem::replace(&mut self.0, ptr::null_mut());

        ctx.resume_ontop(coro as usize, coroutine_unwind);

        trace!("Coroutine `{}`: force unwound", self.debug_name());
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.is_finished() {
            write!(f, "Coroutine(None, Finished)")
        } else {
            write!(f,
                   "Coroutine(Some({}), {:?})",
                   self.debug_name(),
                   self.state())
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use options::Options;
    use scheduler::Scheduler;
    use super::*;

    #[test]
    fn coroutine_unwinds_on_drop() {
        let shared_usize = Arc::new(AtomicUsize::new(0));

        {
            let shared_usize = shared_usize.clone();

            Scheduler::new()
                .run(|| {
                    let coro_container = Arc::new(Mutex::new(None));

                    let handle = {
                        let coro_container = coro_container.clone();

                        Scheduler::spawn(move || {
                            struct DropCheck(Arc<AtomicUsize>);

                            impl Drop for DropCheck {
                                fn drop(&mut self) {
                                    self.0.store(1, Ordering::SeqCst);
                                }
                            }

                            let drop_check = DropCheck(shared_usize);

                            Scheduler::park_with(|_, coro| {
                                *coro_container.lock().unwrap() = Some(coro);
                            });

                            drop_check.0.store(2, Ordering::SeqCst);
                        })
                    };

                    Scheduler::sched();

                    let mut coro_container = coro_container.lock().unwrap();
                    assert!(match *coro_container {
                        Some(..) => true,
                        None => false,
                    });

                    *coro_container = None; // drop the Coroutine

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
        let _ = Coroutine::spawn_opts(f, Options::default());
    }
}
