# Coroutine I/O

[![Build Status](https://img.shields.io/travis/zonyitoo/coio-rs.svg)](https://travis-ci.org/zonyitoo/coio-rs)

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

use coio::{Scheduler, spawn, sched};

fn main() {
    spawn(|| {
        for _ in 0..10 {
            println!("Heil Hydra");
            sched();
        }
    });

    Scheduler::run(1);
}
```

### TCP Echo Server

```rust
extern crate coio;

use std::io::{Read, Write};

use coio::net::TcpListener;
use coio::Scheduler;

fn main() {
    // Spawn a coroutine for accepting new connections
    Scheduler::spawn(move|| {
        let acceptor = TcpListener::bind("127.0.0.1:8080").unwrap();
        println!("Waiting for connection ...");

        for stream in acceptor.incoming() {
            let mut stream = stream.unwrap();

            println!("Got connection from {:?}", stream.peer_addr().unwrap());

            // Spawn a new coroutine to handle the connection
            Scheduler::spawn(move|| {
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
    });

    // Schedule with 4 threads
    Scheduler::run(4);
}
```

## Basic Benchmarks

Run the `http-echo-server` in the example with 4 threads:

```
$ wrk -c 400 -t 2 http://127.0.0.1:8000/
Running 10s test @ http://127.0.0.1:8000/
  2 threads and 400 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     9.57ms   12.11ms 116.17ms   88.03%
    Req/Sec    30.89k     5.68k   41.64k    66.50%
  614593 requests in 10.03s, 52.75MB read
  Socket errors: connect 0, read 208, write 11, timeout 0
Requests/sec:  61292.98
Transfer/sec:      5.26MBB
```

Run the sample HTTP server in Go with `GOMAXPROCS=4`:

```
$ wrk -c 400 -t 2 http://127.0.0.1:8000/
Running 10s test @ http://127.0.0.1:8000/
  2 threads and 400 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     6.19ms    3.29ms  87.45ms   83.29%
    Req/Sec    29.40k     4.97k   44.59k    72.22%
  584380 requests in 10.04s, 75.24MB read
  Socket errors: connect 0, read 57, write 8, timeout 0
Requests/sec:  58219.75
Transfer/sec:      7.50MB
```
