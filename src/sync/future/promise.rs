// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::sync::Arc;

use super::Core;

pub struct Promise<To, Eo>(Arc<Core<To, Eo>>);
unsafe impl<To, Eo> Send for Promise<To, Eo> {}

impl<To, Eo> Promise<To, Eo> {
    pub fn with_core(arc: Arc<Core<To, Eo>>) -> Promise<To, Eo> {
        Promise(arc)
    }

    pub fn resolve(self, val: To) {
        self.0.settle(Ok(val))
    }

    pub fn reject(self, val: Eo) {
        self.0.settle(Err(val))
    }
}
