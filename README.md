# Coroutine I/O

[![Build Status](https://img.shields.io/travis/zonyitoo/coio-rs.svg)](https://travis-ci.org/zonyitoo/coio-rs)
[![Build Status](https://img.shields.io/appveyor/ci/zonyitoo/coio-rs.svg)](https://ci.appveyor.com/project/zonyitoo/coio-rs)
[![License](https://img.shields.io/github/license/zonyitoo/coio-rs.svg)](https://github.com/zonyitoo/coio-rs)

Coroutine scheduling with work-stealing algorithm.

## Feature

* Non-blocking I/O
* Work-stealing coroutine scheduling
* Asynchronous computing APIs

## Usage

```toml
[dependencies]
git = "https://github.com/zonyitoo/coio-rs.git"
```

### Basic Coroutines

```rust
extern crate coio;

use coio::Scheduler;

fn main() {
    Scheduler::with_workers(1)
        .run(|| {
            for _ in 0..10 {
                println!("Heil Hydra");
                Scheduler::sched();
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
    Scheduler::with_workers(4).run(move|| {
        let acceptor = TcpListener::bind("127.0.0.1:8080").unwrap();
        println!("Waiting for connection ...");

        for stream in acceptor.incoming() {
            let mut stream = stream.unwrap();

            println!("Got connection from {:?}", stream.peer_addr().unwrap());

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

## Exit all pending coroutines when the main function is exited.

```rust
extern crate coio;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use coio::Scheduler;

fn main() {
    let counter = Arc::new(AtomicUsize::new(0));
    let cloned_counter = counter.clone();

    let result = Scheduler::with_workers(1).run(move|| {
        // Spawn a new coroutine
        Scheduler::spawn(move|| {
            struct Guard(Arc<AtomicUsize>);

            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.store(1, Ordering::SeqCst);
                }
            }

            let _guard = Guard(cloned_counter);

            coio::sleep_ms(10_000);
            println!("Not going to run this line");
        });

        // Exit right now, which will cause the coroutine to be destroyed.
        panic!("Exit right now!!");
    });

    assert!(result.is_err() && counter.load(Ordering::SeqCst) == 1);
}
```

## Basic Benchmarks

Run the `tcp-echo-server` in the example with 4 threads:

```
Benchmarking: 127.0.0.1:8000
128 clients, running 26 bytes, 30 sec.

Speed: 71344 request/sec, 71344 response/sec
Requests: 2140348
```

Run the sample TCP server in Go 1.5 with `GOMAXPROCS=4`:

```
Benchmarking: 127.0.0.1:8000
128 clients, running 26 bytes, 30 sec.

Speed: 70789 request/sec, 70789 response/sec
Requests: 2123691
Responses: 2123691
```
