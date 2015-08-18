# Coroutine I/O

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
    Scheduler::spawn(|| {
        for _ in 0..10 {
            println!("Heil Hydra");
        }
    });

    Scheduler::run(1);
}
```

### TCP Echo Server

```rust
extern crate simplesched;

use std::io::{Read, Write};

use simplesched::net::TcpListener;
use simplesched::Scheduler;

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
    Latency     7.75ms    7.53ms 103.39ms   86.43%
    Req/Sec    29.98k     3.79k   43.40k    83.50%
  596968 requests in 10.04s, 51.24MB read
  Socket errors: connect 0, read 105, write 0, timeout 0
Requests/sec:  59434.56
Transfer/sec:      5.10MB
```

Run the sample HTTP server in Go with `GOMAXPROCS=4`:

```
$ wrk -c 400 -t 2 http://127.0.0.1:8000/
Running 10s test @ http://127.0.0.1:8000/
  2 threads and 400 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     6.18ms    3.91ms  84.05ms   86.07%
    Req/Sec    28.09k     6.93k   48.67k    71.21%
  560568 requests in 10.08s, 72.17MB read
Requests/sec:  55607.02
Transfer/sec:      7.16MB
```
