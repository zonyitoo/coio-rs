// The MIT License (MIT)

// Copyright (c) 2016 Y. T. Chung <zonyitoo@gmail.com>

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

//! Stack pool

use std::ops::{Deref, DerefMut};

use linked_hash_map::LinkedHashMap;

use context::stack::ProtectedFixedSizeStack;

/// Stack representation
pub struct Stack {
    inner: ProtectedFixedSizeStack,
    size: usize,
}

impl Stack {
    fn new(s: ProtectedFixedSizeStack, size: usize) -> Stack {
        Stack {
            inner: s,
            size: size,
        }
    }
}

impl Deref for Stack {
    type Target = ProtectedFixedSizeStack;
    fn deref(&self) -> &ProtectedFixedSizeStack {
        &self.inner
    }
}

impl DerefMut for Stack {
    fn deref_mut(&mut self) -> &mut ProtectedFixedSizeStack {
        &mut self.inner
    }
}

/// Stackpool
pub struct StackPool {
    inner: LinkedHashMap<usize, Vec<Stack>>,

    total_size: usize,
    higher_water_mark: Option<usize>,
    lower_water_mark: Option<usize>,
}

impl StackPool {
    /// Create a new stack pool with *low water mark* and *high water mark*.
    ///
    /// When the total stack size in the pool reach *high water mark*, it will deallocate stacks by LRU strategy
    /// until it reaches *low water mark*.
    pub fn new(lwm: Option<usize>, hwm: Option<usize>) -> StackPool {
        StackPool {
            inner: LinkedHashMap::new(),

            total_size: 0,
            higher_water_mark: hwm,
            lower_water_mark: lwm,
        }
    }

    /// Allocate stack by directly creation
    pub fn raw_allocate(size: usize) -> Stack {
        trace!("allocating {} bytes from raw", size);
        Stack::new(ProtectedFixedSizeStack::new(size).expect("failed to acquire stack"),
                   size)
    }

    /// Create a stack from pool, create if we don't have stack in pool
    pub fn allocate(&mut self, size: usize) -> Stack {
        let stack = match self.inner.get_refresh(&size) {
            Some(cached) => {
                match cached.pop() {
                    Some(stack) => {
                        trace!("allocating {} bytes stack from pool", size);
                        self.total_size -= size;
                        stack
                    }
                    None => StackPool::raw_allocate(size),
                }
            }
            None => StackPool::raw_allocate(size),
        };

        self.try_shrink();

        trace!("allocated, total size: {} bytes, buckets: {}",
               self.total_size,
               self.inner.len());

        stack
    }

    /// Deallocate stack into pool
    pub fn deallocate(&mut self, stack: Stack) {
        let size = stack.size;

        let raw_inner: *mut LinkedHashMap<usize, Vec<Stack>> = &mut self.inner;

        match self.inner.get_refresh(&size) {
            Some(cached) => {
                cached.push(stack);
            }
            None => {
                let cached = vec![stack];

                // FIXME: Very annonying that LinkedHashMap doesn't provide .entry API
                // Issue: https://github.com/contain-rs/linked-hash-map/issues/5
                unsafe { &mut *raw_inner }.insert(size, cached);
            }
        }

        self.total_size += size;
        self.try_shrink();
        trace!("deallocated, total size: {} bytes, buckets: {}",
               self.total_size,
               self.inner.len());
    }

    /// Shrink the pool to low water mark if total size > high water mark
    fn try_shrink(&mut self) {
        let hwm = if let Some(hwm) = self.higher_water_mark {
            if self.total_size <= hwm {
                return;
            }
            hwm
        } else {
            return;
        };

        let lower_bound = if let Some(lwm) = self.lower_water_mark {
            lwm
        } else {
            hwm
        };

        let old_size = self.total_size;

        'outer: while self.total_size > lower_bound {
            match self.inner.pop_back() {
                Some((size, mut cached)) => {
                    while let Some(..) = cached.pop() {
                        self.total_size -= size;

                        if self.total_size <= lower_bound {
                            // We still have some stacks inside, put it back
                            if !cached.is_empty() {
                                self.inner.insert(size, cached);
                            }

                            break 'outer;
                        }
                    }
                }
                None => {
                    break;
                }
            }
        }

        trace!("shrinked, total size: {} bytes, released {} bytes",
               self.total_size,
               old_size - self.total_size);
    }

    #[inline]
    pub fn total_size(&self) -> usize {
        self.total_size
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn stack_pool_basic() {
        let mut pool = StackPool::new(None, None);
        let stack = pool.allocate(1024);
        pool.deallocate(stack);
    }

    #[test]
    fn stack_pool_strink() {
        let mut pool = StackPool::new(Some(1024), Some(2048));
        let stack1 = pool.allocate(1024);
        let stack2 = pool.allocate(1024);
        let stack3 = pool.allocate(1024);

        assert_eq!(pool.total_size(), 0);

        pool.deallocate(stack1);
        assert_eq!(pool.total_size(), 1024);

        pool.deallocate(stack2);
        assert_eq!(pool.total_size(), 2048);

        pool.deallocate(stack3);
        assert_eq!(pool.total_size(), 1024);
    }

    #[test]
    fn stack_pool_strink_without_lwm() {
        let mut pool = StackPool::new(None, Some(2048));
        let stack1 = pool.allocate(1024);
        let stack2 = pool.allocate(1024);
        let stack3 = pool.allocate(1024);

        assert_eq!(pool.total_size(), 0);

        pool.deallocate(stack1);
        assert_eq!(pool.total_size(), 1024);

        pool.deallocate(stack2);
        assert_eq!(pool.total_size(), 2048);

        pool.deallocate(stack3);
        assert_eq!(pool.total_size(), 2048);
    }
}
