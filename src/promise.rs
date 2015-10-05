// The MIT License (MIT)

// Copyright (c) 2015 Y. T. Chung <zonyitoo@gmail.com>

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
// CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE

//! Promise style asynchronous APIs

use scheduler::{JoinHandle, Scheduler};
use options::Options;

/// Store the result of an coroutine, return Ok(T) if succeeded, Err(E) otherwise.
pub struct Promise<T, E>
    where T: Send + 'static,
          E: Send + 'static
{
    join_handle: JoinHandle<Result<T, E>>,
}

impl<T, E> Promise<T, E>
    where T: Send + 'static,
          E: Send + 'static
{
    /// Spawn a new coroutine to execute the task
    pub fn spawn<F>(f: F) -> Promise<T, E>
        where F: FnOnce() -> Result<T, E> + Send + 'static
    {
        Promise { join_handle: Scheduler::spawn(f) }
    }

    /// Spawn a new coroutine with options to execute the task
    pub fn spawn_opts<F>(f: F, opts: Options) -> Promise<T, E>
        where F: FnOnce() -> Result<T, E> + Send + 'static
    {
        Promise { join_handle: Scheduler::spawn_opts(f, opts) }
    }

    /// Synchronize the execution with the caller and retrive the result
    pub fn sync(self) -> Result<T, E> {
        self.join_handle.join().unwrap()
    }

    /// Execute one of the function depending on the result of the current coroutine
    pub fn then<TT, EE, FT, FE>(self, ft: FT, fe: FE) -> Promise<TT, EE>
        where TT: Send + 'static,
              EE: Send + 'static,
              FT: Send + 'static + FnOnce(T) -> Result<TT, EE>,
              FE: Send + 'static + FnOnce(E) -> Result<TT, EE>
    {
        let join_handle = Scheduler::spawn(move || {
            match self.join_handle.join().unwrap() {
                Ok(t) => ft(t),
                Err(e) => fe(e),
            }
        });

        Promise { join_handle: join_handle }
    }

    /// Run the function with the result of the current task
    pub fn chain<TT, EE, F>(self, f: F) -> Promise<TT, EE>
        where TT: Send + 'static,
              EE: Send + 'static,
              F: Send + 'static + FnOnce(Result<T, E>) -> Result<TT, EE>
    {
        let join_handle = Scheduler::spawn(move || f(self.join_handle.join().unwrap()));

        Promise { join_handle: join_handle }
    }

    /// Execute the function of the result is `Ok`, otherwise it will just return the value
    pub fn success<TT, F>(self, f: F) -> Promise<TT, E>
        where TT: Send + 'static,
              F: FnOnce(T) -> Result<TT, E> + Send + 'static
    {
        let join_handle = Scheduler::spawn(move || {
            match self.join_handle.join().unwrap() {
                Ok(t) => f(t),
                Err(e) => Err(e),
            }
        });

        Promise { join_handle: join_handle }
    }

    /// Execute the function of the result is `Err`, otherwise it will just return the value
    pub fn fail<F>(self, f: F) -> Promise<T, E>
        where F: Send + 'static + FnOnce(E) -> Result<T, E>
    {
        let join_handle = Scheduler::spawn(move || {
            match self.join_handle.join().unwrap() {
                Ok(t) => Ok(t),
                Err(e) => f(e),
            }
        });

        Promise { join_handle: join_handle }
    }

    /// Execute the function with the result of the previous promise asynchronously
    pub fn finally<F>(self, f: F)
        where F: Send + 'static + FnOnce(Result<T, E>)
    {
        Scheduler::spawn(move || f(self.join_handle.join().unwrap()));
    }

    /// Execute the function with the result of the previous promise synchronously
    pub fn finally_sync<F>(self, f: F)
        where F: Send + 'static + FnOnce(Result<T, E>)
    {
        f(self.sync())
    }
}
