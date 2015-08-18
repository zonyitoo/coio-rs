extern crate coio;
extern crate clap;
#[macro_use] extern crate log;
extern crate env_logger;
extern crate hyper;

use std::io;

use clap::{Arg, App};

use coio::scheduler::Scheduler;
use coio::net::http::Client;

use hyper::header::Connection;

fn main() {
    env_logger::init().unwrap();

    let matches = App::new("http-client")
            .version(env!("CARGO_PKG_VERSION"))
            .author("Y. T. Chung <zonyitoo@gmail.com>")
            .arg(Arg::with_name("ADDR").short("a").long("addr").takes_value(true).required(true)
                    .help("Address to connect"))
            .get_matches();


    let addr = matches.value_of("ADDR").unwrap().to_owned();

    Scheduler::spawn(move|| {
        let client = Client::new();

        let mut res = client.get(&addr).header(Connection::close()).send().unwrap();

        println!("Response: {}", res.status);
        println!("Headers:\n{}", res.headers);
        io::copy(&mut res, &mut io::stdout()).unwrap();
    });

    Scheduler::run(1);
}
