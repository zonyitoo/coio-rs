#[macro_use]
extern crate log;

extern crate clap;
extern crate coio;
extern crate hyper;

use clap::{Arg, App};

use coio::net::tcp::TcpListener;
use coio::Scheduler;
use hyper::buffer::BufReader;
use hyper::header::Headers;
use hyper::http;
use hyper::server::Response;

fn main() {
    let matches = App::new("coio-http-echo")
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
                let (mut stream, addr) = stream.unwrap();
                info!("Accept connection: {:?}", addr);

                Scheduler::spawn(move || {
                    let mut bufreader = BufReader::new(stream.try_clone().unwrap());

                    loop {
                        let req = match http::h1::parse_request(&mut bufreader) {
                            Err(..) => {
                                // error!("Failed to parse request: {:?}", err);
                                break;
                            }
                            Ok(req) => req,
                        };

                        let should_keep_alive = http::should_keep_alive(req.version, &req.headers);

                        let mut header = Headers::new();
                        let response = Response::new(&mut stream, &mut header);
                        if let Err(err) = response.send(b"Hello World!\n") {
                            error!("Failed to write response: {:?}", err);
                            break;
                        }

                        if !should_keep_alive {
                            break;
                        }
                    }
                });
            }
        })
        .unwrap();
}
