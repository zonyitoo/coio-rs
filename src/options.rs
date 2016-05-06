// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Coroutine options

use std::default::Default;

/// Coroutine options
#[derive(Debug, Clone)]
pub struct Options {
    pub stack_size: usize,
    pub name: Option<String>,
}

/// Default coroutine stack size, 128KB
pub const DEFAULT_STACK: usize = 128 * 1024; // 128KB

impl Options {
    pub fn new() -> Options {
        Options {
            stack_size: DEFAULT_STACK,
            name: None,
        }
    }

    pub fn stack_size(&mut self, size: usize) -> &mut Options {
        self.stack_size = size;
        self
    }

    pub fn name(&mut self, name: String) -> &mut Options {
        self.name = Some(name);
        self
    }
}

impl Default for Options {
    fn default() -> Options {
        Options::new()
    }
}
