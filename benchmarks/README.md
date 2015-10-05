# Benchmarks

* TCP Echo Server, one connection per coroutine (`coio-tcp-echo-server.rs`)

* TCP Echo Server, one connection per thread (`threaded-tcp-echo-server.rs`)

* TCP Echo Server, plain MIO with single thread (`mio-tcp-echo-server.rs`)

* TCP Echo Server, standard Go implementation (`go-tcp-echo-server.go`)

## OS X

### Environment

* OS X 10.11

* 2.4 GHz Intel Core i5

* 8 GB 1600 MHz DDR3

* Rustc: `rustc 1.5.0-nightly (130851e03 2015-10-03)`

* Go: `go version go1.5.1 darwin/amd64`

### Configuration

* Coio and Go servers are configured to run with 4 worker threads.

* MIO server is running in a single thread

* Run servers with

    - `cargo run --bin coio-tcp-echo-server --release -- --bind 127.0.0.1:3000 -t 4`

    - `cargo run --bin threaded-tcp-echo-server --release -- --bind 127.0.0.1:3000`

    - `cargo run --bin mio-tcp-echo-server --release -- --bind 127.0.0.1:3000`

    - `GOMAXPROCS=4 go run src/go-tcp-echo-server.go`

* Run bench client with

    - `go run tcp_bench.go -c 128 -t 30 -a "127.0.0.1:3000" -l 4096`

    - Benchmark program is copied from [mioco bench2](https://github.com/dpc/mioco#benchmarks).

### Result

| Server        | Requests     | Responses | Requests/sec | Responses/sec |
| ------------- | ------------ | --------- | ------------ | ------------- |
| coio          | 2062243      | 2062243   | 68741        | 68741         |
| threaded      | 1958508      | 1958508   | 65283        | 65283         |
| mio           | 936787       | 93678     | 31226        | 31226         |
| go            | 2049316      | 2049316   | 68310        | 68310         |
