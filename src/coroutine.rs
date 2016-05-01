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
use std::ptr::{self, Shared};

use context::{Context, Transfer};

use runtime::processor::Processor;
use runtime::stack_pool::{Stack, StackPool};
use options::Options;

extern "C" fn coroutine_entry(t: Transfer) -> ! {
    // Take over the data from Coroutine::spawn_opts
    let InitData { stack, callback } = unsafe {
        let data_opt_ref = &mut *(t.data as *mut Option<InitData>);
        data_opt_ref.take().expect("failed to acquire InitData")
    };

    let mut coro = Coroutine {
        context: None,
        name: None,
        state: State::Suspended,

        prev: None,
        next: None,

        stack: Some(stack),
    };

    {
        let coro_ptr = &mut coro as *mut _ as usize;

        let _ = panic::catch_unwind(panic::AssertUnwindSafe(move || {
            let coro = unsafe { &mut *(coro_ptr as *mut Coroutine) };

            trace!("{:?}: yielding back to spawn", coro);
            let ctx = t.context.resume(coro_ptr).context;

            coro.context = Some(ctx);

            trace!("{:?}: invoking callback", coro);
            callback();
            trace!("{:?}: finished", coro);
        }));
    }

    coro.state = State::Finished;

    let mut ctx = coro.take_context();

    while coro.state != State::Dropping {
        ctx = ctx.resume(0).context
    }

    // Drop the Coroutine including the stack after it is finished
    ctx.resume_ontop(&mut coro as *mut _ as usize, coroutine_exit);

    unreachable!();
}

extern "C" fn coroutine_exit(mut t: Transfer) -> Transfer {
    let coro = unsafe { &mut *(t.data as *mut Coroutine) };

    let stack = coro.stack.take();

    trace!("{:?}: dropping struct", coro);
    unsafe { ptr::drop_in_place(coro) };

    // Give back stack after `Coroutine` struct is destroyed
    if let Some(mut p) = Processor::current() {
        p.stack_pool().deallocate(stack.unwrap());
    }

    t.data = 0;
    t
}

extern "C" fn coroutine_unwind(t: Transfer) -> Transfer {
    let coro = unsafe { &mut *(t.data as *mut Coroutine) };

    // Save the Context in the Coroutine object because `coroutine_entry()` needs
    // to get a hold of the callee Context after exiting the callback.
    coro.context = Some(t.context);

    trace!("{:?}: unwinding", coro);
    panic::resume_unwind(Box::new(ForceUnwind));
}

#[derive(Debug)]
pub struct ForceUnwind;

struct InitData {
    stack: Stack,
    callback: Box<FnBox()>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum State {
    Suspended,
    Running,
    Parked,
    Finished,
    Dropping,
}

/// Coroutine is nothing more than a context and a stack
pub struct Coroutine {
    context: Option<Context>,
    name: Option<String>,
    state: State,

    prev: Option<Shared<Coroutine>>,
    next: Option<Handle>,

    stack: Option<Stack>,
}

unsafe impl Send for Coroutine {}

impl Coroutine {
    #[inline]
    pub fn spawn_opts(f: Box<FnBox()>, opts: Options) -> Handle {
        trace!("Coroutine: spawning {:?}", opts);

        let data = InitData {
            stack: StackPool::raw_allocate(opts.stack_size),
            callback: f,
        };

        Coroutine::create_coroutine(data, opts)
    }

    #[inline]
    pub fn spawn_opts_with_pool(f: Box<FnBox()>, opts: Options, pool: &mut StackPool) -> Handle {
        trace!("Coroutine: spawning {:?}", opts);

        let data = InitData {
            stack: pool.allocate(opts.stack_size),
            callback: f,
        };

        Coroutine::create_coroutine(data, opts)
    }

    fn create_coroutine(data: InitData, opts: Options) -> Handle {
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
    pub fn state(&self) -> State {
        self.state
    }

    #[inline]
    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(String::as_str)
    }

    #[inline]
    pub fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    #[doc(hidden)]
    #[inline]
    fn take_context(&mut self) -> Context {
        self.context.take().expect("failed to take context")
    }

    /// Check if the Coroutine is already finished
    #[doc(hidden)]
    #[inline]
    pub fn is_finished(&self) -> bool {
        self.state == State::Finished
    }

    /// Resume the Coroutine
    #[doc(hidden)]
    #[inline]
    pub fn resume(&mut self, data: usize) -> usize {
        self.yield_with(State::Running, data)
    }

    /// Yields the Coroutine to the Processor
    // FIXME:
    //   This inline(never) annotation is needed because the coroutine_unwinds_on_drop test fails
    //   otherwise in release builds (you can test it by marking this function as inline).
    //
    // Tracking issue: https://github.com/zonyitoo/coio-rs/issues/51
    #[doc(hidden)]
    #[inline(never)]
    pub fn yield_with(&mut self, state: State, data: usize) -> usize {
        let context = self.take_context();

        trace!("{:?}: yielding to {:?}", self, &context);

        self.state = state;

        let Transfer { context, data } = context.resume(data);

        // We've returned from a yield to the Processor, because it resume()d us!
        // `context` is the Context of the Processor which we store so we can yield back to it.
        self.context = Some(context);

        data
    }
}

impl fmt::Debug for Coroutine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.name {
            Some(ref name) => write!(f, "Coroutine({})", name),
            None => write!(f, "Coroutine({:p})", self),
        }
    }
}

#[cfg(debug_assertions)]
impl Drop for Coroutine {
    fn drop(&mut self) {
        ::global_work_count_sub();
    }
}

/// Handle for a Coroutine
// NOTE: Handle must store at least 2 elements:
//   - A pointer to the Coroutine
//   - A flag if the Coroutine is finished and thus if it has been deleted.
pub struct Handle(&'static mut Coroutine);

impl Handle {
    #[doc(hidden)]
    #[inline]
    pub fn into_raw(self) -> *mut Coroutine {
        let coro = self.0 as *mut _;
        mem::forget(self);
        coro
    }

    #[doc(hidden)]
    #[inline]
    pub unsafe fn from_raw(coro: *mut Coroutine) -> Handle {
        assert!(coro != 0 as *mut _, "must be a non-zero pointer");
        Handle(&mut *coro)
    }
}

unsafe impl Send for Handle {}

impl Deref for Handle {
    type Target = Coroutine;

    #[inline]
    fn deref(&self) -> &Coroutine {
        self.0
    }
}

impl DerefMut for Handle {
    #[inline]
    fn deref_mut(&mut self) -> &mut Coroutine {
        self.0
    }
}

impl Drop for Handle {
    #[inline]
    fn drop(&mut self) {
        let mut ctx = self.take_context();
        let state = self.state();

        trace!("{:?}: dropping with state {:?}", self, state);
        if state != State::Finished {
            ctx = ctx.resume_ontop(self.0 as *mut _ as usize, coroutine_unwind).context;
        }

        debug_assert!(self.state() == State::Finished,
                      "Expecting Coroutine to be finished");

        // Final step, drop the coroutine
        self.state = State::Dropping;
        ctx.resume(0);
    }
}

impl fmt::Debug for Handle {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A double linked-list for `Handle`s.
pub struct HandleList {
    length: usize,
    head: Option<Handle>,
    tail: Option<Shared<Coroutine>>,
}

#[allow(dead_code)]
impl HandleList {
    /// Create a new HandleList
    #[inline]
    pub fn new() -> HandleList {
        HandleList {
            length: 0,
            head: None,
            tail: None,
        }
    }

    /// Push a `Handle` to the front
    pub fn push_front(&mut self, mut hdl: Handle) {
        match self.head {
            None => {
                // Since head is None the list must be empty => set head and tail to hdl
                let coro_ptr = hdl.0 as *mut _;
                self.head = Some(hdl);
                self.tail = Some(unsafe { Shared::new(coro_ptr) });
            }
            Some(ref mut head) => {
                // Since head is Some the list must be non-empty
                // Set prev of the current head to the soon-to-be head (hdl)
                head.prev = Some(unsafe { Shared::new(hdl.0 as *mut _) });
                // Set hdl as the new head
                mem::swap(head, &mut hdl);
                // head contains the pointer from hdl (and v.v.) => set the .next of the new head
                head.next = Some(hdl);
            }
        }

        self.length += 1;
    }

    /// Push a `Handle` to the back
    pub fn push_back(&mut self, mut hdl: Handle) {
        match self.tail {
            None => {
                // Since head is None the list must be empty => set head and tail to hdl
                let coro_ptr = hdl.0 as *mut _;
                self.head = Some(hdl);
                self.tail = Some(unsafe { Shared::new(coro_ptr) });
            }
            Some(tail) => {
                let tail_ref = unsafe { &mut **tail };
                let coro_ptr = hdl.0 as *mut _;
                // hdl will be the new tail => let it point to the previous one
                hdl.prev = Some(tail);
                // The previous tail should point to the new tail
                tail_ref.next = Some(hdl);
                // And we need to update our tail pointer of course
                self.tail = Some(unsafe { Shared::new(coro_ptr) });
            }
        }

        self.length += 1;
    }

    /// Pop a `Handle` from the front
    pub fn pop_front(&mut self) -> Option<Handle> {
        match self.head.take() {
            None => None,
            Some(mut head) => {
                self.length -= 1;

                // The next pointer from the returned head should be None
                match head.next.take() {
                    Some(mut hdl) => {
                        // Ensure that the second entry doesn't point to the previous head anymore
                        hdl.prev = None;
                        // The second entry is now the new head
                        self.head = Some(hdl);
                    }
                    None => self.tail = None,
                }

                Some(head)
            }
        }
    }

    /// Pop a `Handle` from the back
    pub fn pop_back(&mut self) -> Option<Handle> {
        // No need for .take() since self.tail is Copy and is overwritten below anyways
        match self.tail {
            None => None,
            Some(tail) => {
                let tail_ref = unsafe { &mut **tail };

                // The second last entry is now the new tail
                self.tail = tail_ref.prev;
                self.length -= 1;

                // The prev pointer from the returned tail should be None
                match tail_ref.prev.take() {
                    // Since we are the only element we can get the returned Handle from self.head
                    None => self.head.take(),
                    // ...otherwise get it from the second last entry
                    Some(prev) => unsafe { &mut **prev }.next.take(),
                }
            }
        }
    }

    pub fn append(&mut self, other: &mut HandleList) {
        match self.tail {
            None => {
                // If `self` is empty we can simply take over `other`
                self.head = other.head.take();
                self.tail = other.tail.take();
            }
            Some(tail) => {
                // ...otherwise we need to append `head` from `other` to the `tail` of `self`
                if let Some(mut o_head) = other.head.take() {
                    let tail_ref = unsafe { &mut **tail };
                    // `o_head` will now follow the previous `tail` => set the `prev` pointer first
                    o_head.prev = Some(tail);
                    // ...and do the same for the `next` pointer
                    tail_ref.next = Some(o_head);
                    // `o_tail` will now be the new tail of course
                    self.tail = other.tail.take();
                }
            }
        }

        self.length += other.length;
        other.length = 0;
    }

    pub fn split_off_front(&mut self, n: usize) -> HandleList {
        let mut list = HandleList::new();

        if n >= self.length {
            mem::swap(&mut list, self);
            return list;
        }

        if n == 0 {
            return list;
        }

        let mut p = match self.head {
            None => return list,
            Some(ref mut head) => head.0 as *mut _,
        };

        for _ in 1..n {
            let coro_ref: &mut Coroutine = unsafe { &mut *p };
            p = coro_ref.next.as_mut().unwrap().0 as *mut _;
        }

        // Split right at the p
        let coro_ref: &mut Coroutine = unsafe { &mut *p };
        let mut next_head = coro_ref.next.take();
        next_head.as_mut().map(|h| h.prev = None);

        let new_tail = Some(unsafe { Shared::new(p) });
        mem::swap(&mut next_head, &mut self.head);

        self.length -= n;

        list.head = next_head;
        list.tail = new_tail;
        list.length = n;

        list
    }

    /// Get the length of the list
    #[inline]
    pub fn len(&self) -> usize {
        self.length
    }

    /// Test if this container is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Immutable borrowed iteration
    #[inline]
    pub fn iter(&self) -> HandleListIter {
        HandleListIter {
            curr: &self.head,
            count: self.length,
        }
    }
}

unsafe impl Send for HandleList {}
impl !Sync for HandleList {}

impl Drop for HandleList {
    fn drop(&mut self) {
        // By explicitely dropping the contained Handles we prevent a too deep recursion
        // when every Handle in the "next" member is dropped.
        while let Some(_) = self.pop_front() {}
    }
}

impl fmt::Debug for HandleList {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl IntoIterator for HandleList {
    type Item = Handle;
    type IntoIter = HandleListIntoIter;

    /// Consumes the list into an iterator yielding elements by value.
    #[inline]
    fn into_iter(self) -> HandleListIntoIter {
        HandleListIntoIter { list: self }
    }
}

impl<'a> IntoIterator for &'a HandleList {
    type Item = &'a Handle;
    type IntoIter = HandleListIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Extend<Handle> for HandleList {
    fn extend<T>(&mut self, iter: T)
        where T: IntoIterator<Item = Handle>
    {
        for hdl in iter {
            self.push_back(hdl);
        }
    }
}

/// Immutable borrow iterator for `HandleList`
pub struct HandleListIter<'a> {
    curr: &'a Option<Handle>,
    count: usize,
}

impl<'a> Iterator for HandleListIter<'a> {
    type Item = &'a Handle;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count > 0 {
            if let Some(ref curr) = *self.curr {
                self.count -= 1;
                self.curr = &curr.next;
                return Some(&curr);
            }
        }

        None
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.count, Some(self.count))
    }

    #[inline]
    fn count(self) -> usize {
        self.count
    }
}

/// Immutable borrow iterator for `HandleList`
pub struct HandleListIntoIter {
    list: HandleList,
}

impl Iterator for HandleListIntoIter {
    type Item = Handle;

    fn next(&mut self) -> Option<Self::Item> {
        self.list.pop_front()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.list.length, Some(self.list.length))
    }

    #[inline]
    fn count(self) -> usize {
        self.list.length
    }
}

#[cfg(test)]
mod test {
    use std::mem;
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use options::Options;
    use scheduler::Scheduler;
    use super::*;

    #[test]
    fn coroutine_size() {
        let size = 2 * mem::size_of::<usize>();
        assert_eq!(mem::size_of::<Handle>(), size);
        assert_eq!(mem::size_of::<Option<Handle>>(), size);
    }

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
        let opts = Options::default();
        let f = || {
            panic!("Coroutine body should not be called");
        };
        let _ = Coroutine::spawn_opts(Box::new(f), opts);
    }
}
