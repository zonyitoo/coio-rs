/// One connection per thread

extern crate clap;
#[macro_use]
extern crate log;
extern crate env_logger;

use std::thread;
use std::net::TcpListener;

use clap::{Arg, App};

fn main() {
    env_logger::init().unwrap();

    let matches = App::new("threaded-tcp-echo")
                      .version(env!("CARGO_PKG_VERSION"))
                      .author("Y. T. Chung <zonyitoo@gmail.com>")
                      .arg(Arg::with_name("BIND")
                               .short("b")
                               .long("bind")
                               .takes_value(true)
                               .required(true)
                               .help("Listening on this address"))
                      .get_matches();

    let bind_addr = matches.value_of("BIND").unwrap();

    loop {
        let server = TcpListener::bind(bind_addr).unwrap();
        info!("Listening on {:?}", server.local_addr().unwrap());

        for stream in server.incoming() {
            use std::io::{Read, Write};

            let mut stream = stream.unwrap();
            let addr = stream.peer_addr().unwrap();
            info!("Accept connection: {:?}", addr);

            thread::spawn(move || {
                let mut buf = [0; 1024*16];

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
                            panic!("Error occurs: {:?}", err);
                        }
                    }
                }

                info!("{:?} closed", addr);
            });
        }
    }
}
