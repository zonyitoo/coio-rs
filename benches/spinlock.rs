// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate coio;
extern crate num_cpus;
extern crate time;

use coio::sync::spinlock::*;
use std::mem;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

const NS_PER_MS: usize = 1_000_000;

#[derive(Clone, Copy)]
struct Result {
    duration: usize,
    iters: usize,
}

#[inline]
fn rdiv(a: usize, b: usize) -> usize {
    (a + (b / 2)) / b
}

fn run_test(thread_count: usize) -> Vec<Result> {
    const ITER_COUNT: usize = 10_000_000;
    const EMPTY: Result = Result {
        duration: 0,
        iters: 0,
    };

    let total_count = thread_count * ITER_COUNT;
    let barriers = Arc::new(Barrier::new(thread_count));
    let lock = Arc::new(Spinlock::new(0usize));
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut threads = Vec::with_capacity(thread_count);

    results.lock().unwrap().resize(thread_count, EMPTY);

    for i in 0..thread_count {
        let barriers = barriers.clone();
        let lock = lock.clone();
        let results = results.clone();

        threads.push(thread::spawn(move || {
            barriers.wait();

            let beg = time::precise_time_ns();
            let mut cnt = 0usize;

            loop {
                let mut inner = lock.lock();
                let n = *inner;

                *inner = n + 1;
                cnt += 1;

                if n >= total_count {
                    break;
                }
            }

            let end = time::precise_time_ns();
            let dur = (end - beg) as usize;

            results.lock().unwrap()[i] = Result {
                duration: dur,
                iters: cnt,
            };
        }));
    }

    for t in threads.drain(..) {
        t.join().unwrap();
    }

    let mut results = results.lock().unwrap();
    mem::replace(&mut *results, Vec::new())
}

// Run this test with
//   cargo bench --bench spinlock -- --csv
// to get a parsable output.
// The first column will contain the thread count for that data plot and
// the second column will contain the ns/iter.
// You can feed that data into Excel for instance and create a boxplot graph.
fn main() {
    let csv = std::env::args().any(|arg| arg == "--csv");

    for i in 1..(num_cpus::get() + 1) {
        let results = run_test(i);

        if csv {
            for r in results.iter() {
                println!("{};{}", i, rdiv(r.duration, r.iters));
            }
        } else {
            let perf_sum = results.iter().fold(0, |acc, r| acc + rdiv(r.duration, r.iters));
            let perf_avg = rdiv(perf_sum, results.len());
            let deviation_sum = results.iter().fold(0, |acc, r| {
                let avg = perf_avg as isize;
                let perf = rdiv(r.duration, r.iters) as isize;
                let diff = avg - perf;
                acc + (diff * diff) as usize
            });
            let variance = rdiv(deviation_sum, results.len());

            println!("\n==== {} Threads ====\n", i);

            for (i, r) in results.iter().enumerate() {
                println!("Thread {}: {} acquisitions in {} ms => {} ns/iter",
                         i,
                         r.iters,
                         rdiv(r.duration, NS_PER_MS),
                         rdiv(r.duration, r.iters));
            }

            println!("Avg: {} ns/iter, Var: {}", perf_avg, variance);
        }
    }
}
