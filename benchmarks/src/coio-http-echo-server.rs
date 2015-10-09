extern crate clap;
#[macro_use]
extern crate log;
extern crate env_logger;

extern crate coio;

use clap::{Arg, App};

use coio::Scheduler;
use coio::net::tcp::TcpListener;

const RESPONSE: &'static [u8] = b"HTTP/1.1 200 OK\r
Content-Length: 14\r
\r
Hello World\r
\r";

fn main() {
    env_logger::init().unwrap();

    let matches = App::new("coio-http-echo")
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

            info!("Listening on {:?}", server.local_addr().unwrap());

            for stream in server.incoming() {
                use std::io::Write;

                let (mut stream, addr) = stream.unwrap();
                info!("Accept connection: {:?}", addr);

                Scheduler::spawn(move || {
                    loop {
                        if let Err(..) = stream.write_all(RESPONSE) {
                            break;
                        }
                    }
                });
            }
        })
        .unwrap();
}
