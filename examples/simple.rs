// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate coio;
extern crate env_logger;

use coio::Scheduler;

fn main() {
    env_logger::init().unwrap();

    Scheduler::new()
        .run(|| {
            for _ in 0..10 {
                println!("Heil Hydra");
                Scheduler::sched();
            }
        }).unwrap();
}
