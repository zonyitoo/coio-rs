// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// This file falls under the copyright of https://github.com/dpc/mioco and Dawid Ciężarkiewicz.

extern crate mioco;
extern crate clap;

use clap::{Arg, App};
use std::io::{self, Read, Write};
use mioco::tcp::TcpListener;

fn main() {
    let matches = App::new("coio-tcp-echo")
                      .version(env!("CARGO_PKG_VERSION"))
                      .author("Y. T. Chung <zonyitoo@gmail.com>")
                      .arg(Arg::with_name("bind")
                               .short("b")
                               .long("bind")
                               .takes_value(true)
                               .required(true)
                               .help("Listening on this address"))
                      .arg(Arg::with_name("threads")
                               .short("t")
                               .long("threads")
                               .takes_value(true)
                               .help("Number of threads"))
                      .get_matches();

    let addr = matches.value_of("bind").unwrap().parse().unwrap();
    let threads = matches.value_of("threads").unwrap_or("1").parse().unwrap();

    mioco::start_threads(threads, move || {
        let listener = TcpListener::bind(&addr).unwrap();

        loop {
            let mut conn = listener.accept().unwrap();

            mioco::spawn(move || -> io::Result<()> {
                let mut buf = [0u8; 1024 * 16];
                loop {
                    let size = try!(conn.read(&mut buf));
                    if size == 0 {
                        break;
                    }
                    let _ = try!(conn.write_all(&mut buf[0..size]));
                }

                Ok(())
            });
        }
    })
        .unwrap();
}
