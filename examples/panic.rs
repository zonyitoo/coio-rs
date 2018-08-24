// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate coio;

use coio::Scheduler;

fn main() {
    Scheduler::new()
        .run(|| {
            let handle = Scheduler::spawn(|| {
                panic!("Panicked inside");
            });

            assert!(handle.join().is_err());
        }).unwrap();
}
