// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate clap;
#[macro_use]
extern crate log;
extern crate env_logger;
extern crate mio;

extern crate coio;

use std::net::SocketAddr;

use clap::{App, Arg};

use coio::net::udp::UdpSocket;
use coio::Scheduler;

fn main() {
    env_logger::init();

    let matches = App::new("udp-echo")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Y. T. Chung <zonyitoo@gmail.com>")
        .arg(
            Arg::with_name("BIND")
                .short("b")
                .long("bind")
                .takes_value(true)
                .required(true)
                .help("Listening on this address"),
        ).arg(
            Arg::with_name("THREADS")
                .short("t")
                .long("threads")
                .takes_value(true)
                .help("Number of threads"),
        ).get_matches();

    let bind_addr = matches.value_of("BIND").unwrap().to_owned();

    Scheduler::new()
        .with_workers(matches.value_of("THREADS").unwrap_or("1").parse().unwrap())
        .run(move || {
            let addr: SocketAddr = bind_addr.parse().unwrap();
            let server = UdpSocket::bind(&addr).unwrap();

            info!("Listening on {:?}", server.local_addr().unwrap());

            let mut buf = [0u8; 1024];

            loop {
                let (len, peer_addr) = server.recv_from(&mut buf).unwrap();
                info!("Accept connection: {:?}", peer_addr);

                server.send_to(&mut buf[..len], &peer_addr).unwrap();
            }
        }).unwrap();
}
