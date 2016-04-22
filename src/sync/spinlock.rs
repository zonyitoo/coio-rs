// The MIT License (MIT)

// Copyright (c) 2015 Y. T. Chung <zonyitoo@gmail.com>

//  Permission is hereby granted, free of charge, to any person obtaining a
//  copy of this software and associated documentation files (the "Software"),
//  to deal in the Software without restriction, including without limitation
//  the rights to use, copy, modify, merge, publish, distribute, sublicense,
//  and/or sell copies of the Software, and to permit persons to whom the
//  Software is furnished to do so, subject to the following conditions:
//
//  The above copyright notice and this permission notice shall be included in
//  all copies or substantial portions of the Software.
//
//  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//  OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//  FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//  AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//  LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//  FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//  DEALINGS IN THE SOFTWARE.

//! A simple Spinlock

use std::cell::UnsafeCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Spinlock<T: ?Sized> {
    lock: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Spinlock<T> {}
unsafe impl<T: ?Sized + Send> Sync for Spinlock<T> {}

// This spinlock is a unfair one as such a thread _can_ starve waiting to get the lock.
// An implementation for a fair version using ticketed spinlocks can be found here:
//   https://github.com/torvalds/linux/blob/master/arch/x86/include/asm/spinlock.h
impl<T> Spinlock<T> {
    pub fn new(data: T) -> Spinlock<T> {
        Spinlock {
            lock: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }
}

impl<T: ?Sized> Spinlock<T> {
    pub fn try_lock<'a>(&'a self) -> Option<SpinlockGuard<'a, T>> {
        const SUCCESS: Ordering = Ordering::Acquire;
        const FAILURE: Ordering = Ordering::Relaxed;

        match self.lock.compare_exchange_weak(false, true, SUCCESS, FAILURE) {
            Ok(_) => Some(SpinlockGuard(&self.lock, unsafe { &mut *self.data.get() })),
            Err(_) => None,
        }
    }

    pub fn lock<'a>(&'a self) -> SpinlockGuard<'a, T> {
        const SUCCESS: Ordering = Ordering::Acquire;
        const FAILURE: Ordering = Ordering::Relaxed;

        while self.lock.compare_exchange_weak(false, true, SUCCESS, FAILURE) != Ok(false) {
            if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
                unsafe {
                    // "Modern" processors exiting a tight loop (like this one) usually detect a
                    // _possible_ memory order violation. The PAUSE instruction hints that this
                    // is a busy-waiting loop and that no such violation will occur.
                    // Furthermore it might also relax the loop and efficiently "pause" the
                    // processor for a bit, which reduces power consumption for some CPUs.
                    asm!("pause" ::: "memory" : "volatile");
                }
            }
        }

        SpinlockGuard(&self.lock, unsafe { &mut *self.data.get() })
    }
}

impl<T: ?Sized + Default> Default for Spinlock<T> {
    fn default() -> Spinlock<T> {
        Spinlock::new(Default::default())
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Spinlock<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => write!(f, "Spinlock {{ data: {:?} }}", &*guard),
            None => write!(f, "Spinlock {{ <locked> }}"),
        }
    }
}

pub struct SpinlockGuard<'a, T: ?Sized + 'a>(&'a AtomicBool, &'a mut T);

impl<'a, T: ?Sized> !Send for SpinlockGuard<'a, T> {}

impl<'a, T: ?Sized> Drop for SpinlockGuard<'a, T> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl<'a, T: ?Sized> Deref for SpinlockGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.1
    }
}

impl<'a, T: ?Sized> DerefMut for SpinlockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.1
    }
}
