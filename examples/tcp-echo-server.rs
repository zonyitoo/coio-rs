extern crate clap;
#[macro_use] extern crate log;
extern crate env_logger;
extern crate mio;

extern crate coio;

use std::net::SocketAddr;

use clap::{Arg, App};

use coio::Scheduler;
use coio::net::tcp::TcpSocket;

fn main() {
    env_logger::init().unwrap();

    let matches = App::new("tcp-echo")
            .version(env!("CARGO_PKG_VERSION"))
            .author("Y. T. Chung <zonyitoo@gmail.com>")
            .arg(Arg::with_name("BIND").short("b").long("bind").takes_value(true).required(true)
                    .help("Listening on this address"))
            .arg(Arg::with_name("THREADS").short("t").long("threads").takes_value(true)
                    .help("Number of threads"))
            .get_matches();

    let bind_addr = matches.value_of("BIND").unwrap().to_owned();

    Scheduler::spawn(move|| {
        let addr = bind_addr.parse().unwrap();
        let server = match &addr {
            &SocketAddr::V4(..) => TcpSocket::v4(),
            &SocketAddr::V6(..) => TcpSocket::v6(),
        }.unwrap();
        server.set_reuseaddr(true).unwrap();
        server.bind(&addr).unwrap();
        let server = server.listen(64).unwrap();

        info!("Listening on {:?}", server.local_addr().unwrap());

        for stream in server.incoming() {
            use std::io::{Read, Write};

            let mut stream = stream.unwrap();
            info!("Accept connection: {:?}", stream.peer_addr().unwrap());

            Scheduler::spawn(move|| {
                let mut buf = [0; 10240];

                loop {
                    debug!("Trying to Read...");
                    match stream.read(&mut buf) {
                        Ok(0) => {
                            debug!("EOF received, going to close");
                            break;
                        },
                        Ok(len) => {
                            info!("Read {} bytes, echo back!", len);
                            stream.write_all(&buf[0..len]).unwrap();
                        },
                        Err(err) => {
                            panic!("Error occurs: {:?}", err);
                        }
                    }
                }

                info!("{:?} closed", stream.peer_addr().unwrap());
            });
        }
    });

    Scheduler::run(matches.value_of("THREADS").unwrap_or("1").parse().unwrap());
}
