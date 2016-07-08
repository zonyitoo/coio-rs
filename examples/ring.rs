// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate coio;

use std::time::Instant;

use coio::{spawn, Scheduler};
use coio::sync::mpsc::{channel, Sender};

fn create_node(next: Sender<usize>) -> Sender<usize> {
    let (send, recv) = channel::<usize>();
    spawn(move || {
        loop {
            let i = recv.recv().unwrap();
            if i == 0 {
                break;
            }
            next.send(i + 1).unwrap();
        }
        next.send(0).unwrap();
    });
    send
}

fn master(iters: usize, size: usize) {
    let t0 = Instant::now();
    let (mut send, recv) = channel::<usize>();
    for _ in 0..size - 1 {
        send = create_node(send);
    }
    let t1 = Instant::now();
    println!("Ring Created");
    let mut i = 0;
    for _ in 0..iters {
        send.send(i + 1).unwrap();
        i = recv.recv().unwrap();
    }
    let t2 = Instant::now();
    println!("{}", i);
    send.send(0).unwrap();
    recv.recv().unwrap();
    println!("Creation time: {:?}", t1 - t0);
    println!("Messaging time: {:?}", t2 - t1);
}

fn main() {
    let mut args = std::env::args();
    let name = args.next().unwrap();
    let (iters, size, procs) = match (args.next(), args.next(), args.next()) {
        (Some(iters), Some(size), Some(procs)) => {
            (iters.parse().unwrap(), size.parse().unwrap(), procs.parse().unwrap())
        }
        _ => panic!("{} <iters> <size> <procs>", name),
    };

    let _ = Scheduler::new().with_workers(procs).run(move || {
        master(iters, size);
    });
}
