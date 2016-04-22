#[macro_use]
extern crate log;

extern crate clap;
extern crate coio;

use clap::{Arg, App};
use coio::net::tcp::TcpListener;
use coio::Scheduler;

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

    let bind_addr = matches.value_of("bind").unwrap().to_owned();

    Scheduler::new()
        .with_workers(matches.value_of("threads").unwrap_or("1").parse().unwrap())
        .run(move || {
            let server = TcpListener::bind(&bind_addr[..]).unwrap();

            info!("Listening on {:?}", server.local_addr().unwrap());

            for stream in server.incoming() {
                use std::io::{Read, Write};

                let (mut stream, addr) = stream.unwrap();
                info!("Accept connection: {:?}", addr);

                Scheduler::spawn(move || {
                    let mut buf = [0u8; 16 * 1024];

                    loop {
                        debug!("Trying to Read...");
                        match stream.read(&mut buf) {
                            Ok(0) => {
                                debug!("EOF received, going to close");
                                break;
                            }
                            Ok(len) => {
                                info!("Read {} bytes, echo back!", len);
                                stream.write_all(&buf[0..len]).unwrap();
                            }
                            Err(err) => {
                                println!("An error occured: {:?}", err);
                                break;
                            }
                        }
                    }

                    info!("{:?} closed", addr);
                });
            }
        })
        .unwrap();
}
