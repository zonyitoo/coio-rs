# Coroutine I/O

[![Build Status](https://travis-ci.org/zonyitoo/coio-rs.svg?branch=master)](https://travis-ci.org/zonyitoo/coio-rs)
[![License](https://img.shields.io/github/license/zonyitoo/coio-rs.svg)](https://github.com/zonyitoo/coio-rs)

Coroutine scheduling with work-stealing algorithm.

## Feature

* Non-blocking I/O
* Work-stealing coroutine scheduling
* Asynchronous computing APIs

## Usage

Note: You must use [Nightly Rust](https://doc.rust-lang.org/book/nightly-rust.html) to build this Project.

```toml
[dependencies.coio]
git = "https://github.com/zonyitoo/coio-rs.git"
```

### Basic Coroutines

```rust
extern crate coio;

use coio::Scheduler;

fn main() {
    Scheduler::new()
        .run(|| {
            for _ in 0..10 {
                println!("Heil Hydra");
                Scheduler::sched(); // Yields the current coroutine
            }
        })
        .unwrap();
}
```

### TCP Echo Server

```rust
extern crate coio;

use std::io::{Read, Write};

use coio::net::TcpListener;
use coio::{spawn, Scheduler};

fn main() {
    // Spawn a coroutine for accepting new connections
    Scheduler::new().with_workers(4).run(move|| {
        let acceptor = TcpListener::bind("127.0.0.1:8080").unwrap();
        println!("Waiting for connection ...");

        for stream in acceptor.incoming() {
            let (mut stream, addr) = stream.unwrap();

            println!("Got connection from {:?}", addr);

            // Spawn a new coroutine to handle the connection
            spawn(move|| {
                let mut buf = [0; 1024];

                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => {
                            println!("EOF");
                            break;
                        },
                        Ok(len) => {
                            println!("Read {} bytes, echo back", len);
                            stream.write_all(&buf[0..len]).unwrap();
                        },
                        Err(err) => {
                            println!("Error occurs: {:?}", err);
                            break;
                        }
                    }
                }

                println!("Client closed");
            });
        }
    }).unwrap();
}
```

### Exit the main function

Will cause all pending coroutines to be killed.

```rust
extern crate coio;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use coio::Scheduler;

fn main() {
    let counter = Arc::new(AtomicUsize::new(0));
    let cloned_counter = counter.clone();

    let result = Scheduler::new().run(move|| {
        // Spawn a new coroutine
        Scheduler::spawn(move|| {
            struct Guard(Arc<AtomicUsize>);

            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.store(1, Ordering::SeqCst);
                }
            }

            // If the _guard is dropped, it will store 1 to the counter
            let _guard = Guard(cloned_counter);

            coio::sleep(Duration::from_secs(10));
            println!("Not going to run this line");
        });

        // Exit right now, which will cause the coroutine to be destroyed.
        panic!("Exit right now!!");
    });

    // The coroutine's stack is unwound properly
    assert!(result.is_err() && counter.load(Ordering::SeqCst) == 1);
}
```

## Basic Benchmarks

See [benchmarks](benchmarks) for more details.

## Projects using coio

* [shadowsocks-rust](https://github.com/zonyitoo/shadowsocks-rust) - shadowsocks is a fast tunnel proxy that helps you bypass firewalls.
