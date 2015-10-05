/// The code is borrowed from `mio/test/test_echo_server.rs`.

extern crate mio;
#[macro_use]
extern crate log;
extern crate bytes;
extern crate clap;
extern crate env_logger;

use std::io;
use std::net::SocketAddr;

use mio::*;
use mio::tcp::*;
use bytes::{ByteBuf, MutByteBuf};
use mio::util::Slab;

use clap::{Arg, App};

const SERVER: Token = Token(0);

struct EchoConn {
    sock: TcpStream,
    buf: Option<ByteBuf>,
    mut_buf: Option<MutByteBuf>,
    token: Option<Token>,
    interest: EventSet
}

impl EchoConn {
    fn new(sock: TcpStream) -> EchoConn {
        EchoConn {
            sock: sock,
            buf: None,
            mut_buf: Some(ByteBuf::mut_with_capacity(2048)),
            token: None,
            interest: EventSet::hup()
        }
    }

    fn writable(&mut self, event_loop: &mut EventLoop<Echo>) -> io::Result<()> {
        let mut buf = self.buf.take().unwrap();

        match self.sock.try_write_buf(&mut buf) {
            Ok(None) => {
                debug!("client flushing buf; WOULDBLOCK");

                self.buf = Some(buf);
                self.interest.insert(EventSet::writable());
            }
            Ok(Some(r)) => {
                debug!("CONN : we wrote {} bytes!", r);

                self.mut_buf = Some(buf.flip());

                self.interest.insert(EventSet::readable());
                self.interest.remove(EventSet::writable());
            }
            Err(e) => debug!("not implemented; client err={:?}", e),
        }

        event_loop.reregister(&self.sock, self.token.unwrap(), self.interest,
                              PollOpt::edge() | PollOpt::oneshot())
    }

    fn readable(&mut self, event_loop: &mut EventLoop<Echo>) -> io::Result<bool> {
        let mut buf = self.mut_buf.take().unwrap();

        match self.sock.try_read_buf(&mut buf) {
            Ok(None) => {
                debug!("CONN : spurious read wakeup");
                self.mut_buf = Some(buf);
            }
            Ok(Some(0)) => {
                debug!("CONN : EOF");
                try!(event_loop.deregister(&self.sock));
                return Ok(false);
            }
            Ok(Some(r)) => {
                debug!("CONN : we read {} bytes!", r);

                // prepare to provide this to writable
                self.buf = Some(buf.flip());

                self.interest.remove(EventSet::readable());
                self.interest.insert(EventSet::writable());
            }
            Err(e) => {
                debug!("not implemented; client err={:?}", e);
                self.interest.remove(EventSet::readable());
            }

        };

        try!(event_loop.reregister(&self.sock, self.token.unwrap(), self.interest,
                                   PollOpt::edge()));
        Ok(true)
    }
}

struct EchoServer {
    sock: TcpListener,
    conns: Slab<EchoConn>
}

impl EchoServer {
    fn accept(&mut self, event_loop: &mut EventLoop<Echo>) -> io::Result<()> {
        debug!("server accepting socket");

        let sock = self.sock.accept().unwrap().unwrap().0;
        let conn = EchoConn::new(sock,);
        let tok = self.conns.insert(conn)
            .ok().expect("could not add connection to slab");

        // Register the connection
        self.conns[tok].token = Some(tok);
        event_loop.register(&self.conns[tok].sock, tok, EventSet::readable(),
                                PollOpt::edge() | PollOpt::oneshot())
            .ok().expect("could not register socket with event loop");

        Ok(())
    }

    fn conn_readable(&mut self, event_loop: &mut EventLoop<Echo>,
                     tok: Token) -> io::Result<()> {
        debug!("server conn readable; tok={:?}", tok);
        if !try!(self.conn(tok).readable(event_loop)) {
            self.conns.remove(tok);
        }
        Ok(())
    }

    fn conn_writable(&mut self, event_loop: &mut EventLoop<Echo>,
                     tok: Token) -> io::Result<()> {
        debug!("server conn writable; tok={:?}", tok);
        self.conn(tok).writable(event_loop)
    }

    fn conn<'a>(&'a mut self, tok: Token) -> &'a mut EchoConn {
        &mut self.conns[tok]
    }
}

struct Echo {
    server: EchoServer,
}

impl Echo {
    fn new(srv: TcpListener) -> Echo {
        Echo {
            server: EchoServer {
                sock: srv,
                conns: Slab::new_starting_at(Token(2), 1024)
            },
        }
    }
}

impl Handler for Echo {
    type Timeout = usize;
    type Message = ();

    fn ready(&mut self, event_loop: &mut EventLoop<Echo>, token: Token,
             events: EventSet) {
        debug!("ready {:?} {:?}", token, events);
        if events.is_readable() {
            match token {
                SERVER => self.server.accept(event_loop).unwrap(),
                i => self.server.conn_readable(event_loop, i).unwrap()
            }
        }

        if events.is_writable() {
            match token {
                SERVER => panic!("received writable for token 0"),
                _ => self.server.conn_writable(event_loop, token).unwrap()
            };
        }
    }
}

fn main() {
    env_logger::init().unwrap();

    let matches = App::new("mio-tcp-echo")
                      .version(env!("CARGO_PKG_VERSION"))
                      .author("Y. T. Chung <zonyitoo@gmail.com>")
                      .arg(Arg::with_name("BIND")
                               .short("b")
                               .long("bind")
                               .takes_value(true)
                               .required(true)
                               .help("Listening on this address"))
                      .get_matches();

    let addr_str = matches.value_of("BIND").unwrap();
    let addr: SocketAddr = addr_str.parse().unwrap();

    debug!("Starting TEST_ECHO_SERVER");
    let mut event_loop = EventLoop::new().unwrap();

    let srv = TcpListener::bind(&addr).unwrap();

    info!("listen for connections");
    event_loop.register(&srv, SERVER, EventSet::readable(),
                        PollOpt::level()).unwrap();

    // Start the event loop
    event_loop.run(&mut Echo::new(srv)).unwrap();
}
