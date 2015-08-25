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
// CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use std::collections::BinaryHeap;
use std::cmp::{Ord, Ordering, Eq, PartialOrd};

use chrono::NaiveDateTime;
use chrono::Local;

use coroutine::Coroutine;

#[allow(raw_pointer_derive)]
#[derive(Eq, PartialEq)]
struct SleepingTask {
    coro_ptr: *mut Coroutine,
    expected_wakeup_time: NaiveDateTime,
}

impl PartialOrd<SleepingTask> for SleepingTask {
    fn partial_cmp(&self, other: &SleepingTask) -> Option<Ordering> {
        Some(other.expected_wakeup_time.cmp(&self.expected_wakeup_time))
    }
}

impl Ord for SleepingTask {
    fn cmp(&self, other: &SleepingTask) -> Ordering {
        other.expected_wakeup_time.cmp(&self.expected_wakeup_time)
    }
}

unsafe impl Send for SleepingTask {}

impl SleepingTask {
    fn new(coro_ptr: *mut Coroutine, wakeup: NaiveDateTime) -> SleepingTask {
        SleepingTask {
            coro_ptr: coro_ptr,
            expected_wakeup_time: wakeup,
        }
    }
}

pub struct Timer {
    sleeping_tasks: BinaryHeap<SleepingTask>,
}

impl Timer {
    pub fn new() -> Timer {
        Timer {
            sleeping_tasks: BinaryHeap::new(),
        }
    }

    pub fn wait_until(&mut self, coro_ptr: *mut Coroutine, wakeup: NaiveDateTime) {
        let new_task = SleepingTask::new(coro_ptr, wakeup);
        self.sleeping_tasks.push(new_task);
    }

    #[inline(always)]
    pub unsafe fn try_awake(&mut self) -> Option<*mut Coroutine> {
        let now = Local::now().naive_local();
        let ret = if let Some(sleeping) = self.sleeping_tasks.peek() {
            if sleeping.expected_wakeup_time <= now {
                Some(sleeping.coro_ptr)
            } else {
                None
            }
        } else {
            None
        };

        if ret.is_some() {
            self.sleeping_tasks.pop();
        }

        ret
    }
}
