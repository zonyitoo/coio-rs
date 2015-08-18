extern crate clap;
#[macro_use] extern crate log;
extern crate env_logger;
extern crate hyper;

extern crate coio;

use std::io;

use clap::{Arg, App};

use coio::Scheduler;
use coio::net::http::Server;

use hyper::{Get, Post};
use hyper::server::{Request, Response};
use hyper::uri::RequestUri::AbsolutePath;

macro_rules! try_return(
    ($e:expr) => {{
        match $e {
            Ok(v) => v,
            Err(e) => { println!("Error: {}", e); return; }
        }
    }}
);

fn echo(mut req: Request, mut res: Response) {
    match req.uri {
        AbsolutePath(ref path) => match (&req.method, &path[..]) {
            (&Get, "/") | (&Get, "/echo") => {
                try_return!(res.send(b"Try POST /echo"));
                return;
            },
            (&Post, "/echo") => (), // fall through, fighting mutable borrows
            _ => {
                *res.status_mut() = hyper::NotFound;
                return;
            }
        },
        _ => {
            return;
        }
    };

    let mut res = try_return!(res.start());
    try_return!(io::copy(&mut req, &mut res));
}

fn main() {
    env_logger::init().unwrap();

    let matches = App::new("http-echo")
            .version(env!("CARGO_PKG_VERSION"))
            .author("Y. T. Chung <zonyitoo@gmail.com>")
            .arg(Arg::with_name("BIND").short("b").long("bind").takes_value(true).required(true)
                    .help("Listening on this address"))
            .arg(Arg::with_name("THREADS").short("t").long("threads").takes_value(true)
                    .help("Number of threads"))
            .arg(Arg::with_name("KEEPALIVE").short("c").long("keep-alive").takes_value(false)
                    .help("Keep alive"))
            .get_matches();

    let bind_addr = matches.value_of("BIND").unwrap().to_owned();

    let server = Server::http(&bind_addr[..]).unwrap();
    server.listen(echo).unwrap();

    Scheduler::run(matches.value_of("THREADS").unwrap_or("1").parse().unwrap());
}
