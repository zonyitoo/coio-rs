// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate coio;

use std::sync::Arc;

use coio::{JoinHandle, Scheduler};

fn invoke_every_ms<F>(ms: u64, f: F) -> JoinHandle<()>
    where F: Fn() + Sync + Send + 'static
{
    let f = Arc::new(f);
    coio::spawn(move || {
        loop {
            coio::sleep_ms(ms);
            let f = f.clone();
            coio::spawn(move || (*f)());
        }
    })
}

fn invoke_after_ms<F>(ms: u64, f: F) -> JoinHandle<()>
    where F: FnOnce() + Send + 'static
{
    coio::spawn(move || {
        coio::sleep_ms(ms);
        f();
    })
}

fn main() {
    Scheduler::new()
        .with_workers(2)
        .run(|| {
            coio::spawn(|| {
                for i in 0..10 {
                    println!("Qua  {}", i);
                    coio::sleep_ms(2000);
                }
            });

            let hdls = vec![
                invoke_every_ms(100, || println!("Purr :P")),
                invoke_after_ms(1000, || println!("Tadaaaaaaa")),
            ];

            for hdl in hdls {
                hdl.join().unwrap();
            }
        })
        .unwrap();
}
