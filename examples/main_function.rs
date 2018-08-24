// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate coio;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use coio::Scheduler;

fn main() {
    let counter = Arc::new(AtomicUsize::new(0));
    let cloned_counter = counter.clone();

    let result = Scheduler::new().run(move || {
        // Spawn a new coroutine
        Scheduler::spawn(move || {
            struct Guard(Arc<AtomicUsize>);

            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.store(1, Ordering::SeqCst);
                }
            }

            let _guard = Guard(cloned_counter);

            coio::sleep_ms(10_000);
            unreachable!("Not going to run this line");
        });

        Scheduler::sched();

        // Exit right now, which will cause the coroutine to be destroyed.
        panic!("Exit right now!!");
    });

    assert!(result.is_err());
    assert!(counter.load(Ordering::SeqCst) == 1);
}
