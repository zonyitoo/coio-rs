#[macro_use]
extern crate log;

extern crate clap;
extern crate coio;

use std::io::Write;

use clap::{Arg, App};

use coio::net::tcp::TcpListener;
use coio::Scheduler;

const HELLO_RESPONSE: &'static str = "HTTP/1.1 200 OK\r
Content-Length: 14\r
\r
Hello World\r
\r";

fn main() {
    let matches = App::new("coio-cheated-http-echo")
                      .version(env!("CARGO_PKG_VERSION"))
                      .author("Y. T. Chung <zonyitoo@gmail.com>")
                      .arg(Arg::with_name("BIND")
                               .short("b")
                               .long("bind")
                               .takes_value(true)
                               .required(true)
                               .help("Listening on this address"))
                      .arg(Arg::with_name("THREADS")
                               .short("t")
                               .long("threads")
                               .takes_value(true)
                               .help("Number of threads"))
                      .get_matches();

    let bind_addr = matches.value_of("BIND").unwrap().to_owned();

    Scheduler::new()
        .with_workers(matches.value_of("THREADS").unwrap_or("1").parse().unwrap())
        .run(move || {
            let server = TcpListener::bind(&bind_addr[..]).unwrap();

            trace!("Listening on {:?}", server.local_addr().unwrap());

            for stream in server.incoming() {
                let (mut stream, addr) = stream.unwrap();
                trace!("Accept connection: {:?}", addr);

                Scheduler::spawn(move || {
                    // Just write
                    let _ = stream.write_all(&HELLO_RESPONSE.as_bytes());
                });
            }
        })
        .unwrap();
}
