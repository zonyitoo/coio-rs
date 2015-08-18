// The MIT License (MIT)

// Copyright (c) 2015 Y. T. Chung <zonyitoo@gmail.com>

//  Permission is hereby granted, free of charge, to any person obtaining a
//  copy of this software and associated documentation files (the "Software"),
//  to deal in the Software without restriction, including without limitation
//  the rights to use, copy, modify, merge, publish, distribute, sublicense,
//  and/or sell copies of the Software, and to permit persons to whom the
//  Software is furnished to do so, subject to the following conditions:
//
//  The above copyright notice and this permission notice shall be included in
//  all copies or substantial portions of the Software.
//
//  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//  OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//  FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//  AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//  LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//  FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//  DEALINGS IN THE SOFTWARE.

use std::net::{ToSocketAddrs, SocketAddr};
use std::convert::From;
use std::io::{self, Write, BufWriter};

use hyper;
use hyper::http;
use hyper::buffer::BufReader;
use hyper::server::{Request, Response, Handler};
use hyper::header::{Connection, Headers, Expect};
use hyper::version::HttpVersion;
use hyper::net::{NetworkListener, NetworkStream};
use hyper::status::StatusCode;
use hyper::error::Error;

use net::http::conn::{HttpListener, HttpsListener, Ssl};

use scheduler::Scheduler;

/// A server can listen on a TCP socket.
///
/// Once listening, it will create a `Request`/`Response` pair for each
/// incoming connection, and hand them to the provided handler.
#[derive(Debug)]
pub struct Server<L = HttpListener> {
    listener: L,
}

impl<L: NetworkListener> Server<L> {
    /// Creates a new server with the provided handler.
    #[inline]
    pub fn new(listener: L) -> Server<L> {
        Server {
            listener: listener,
        }
    }
}

impl Server<HttpListener> {
    /// Creates a new server that will handle `HttpStream`s.
    pub fn http<To: ToSocketAddrs>(addr: To) -> hyper::Result<Server<HttpListener>> {
        HttpListener::new(addr).map(Server::new).map_err(From::from)
    }
}

impl<S: Ssl + Clone + Send> Server<HttpsListener<S>> {
    /// Creates a new server that will handle `HttpStream`s over SSL.
    ///
    /// You can use any SSL implementation, as long as implements `hyper::net::Ssl`.
    pub fn https<A: ToSocketAddrs>(addr: A, ssl: S) -> hyper::Result<Server<HttpsListener<S>>> {
        HttpsListener::new(addr, ssl).map(Server::new)
    }
}

impl<L: NetworkListener + Send + 'static> Server<L> {
    /// Binds to a socket.
    pub fn listen<H: Handler + 'static>(mut self, handler: H) -> hyper::Result<SocketAddr> {
        let socket = try!(self.listener.local_addr());

        Scheduler::spawn(move|| {
            use std::sync::Arc;

            let handler = Arc::new(handler);
            loop {
                let mut stream = self.listener.accept().unwrap();

                let handler = handler.clone();
                Scheduler::spawn(move|| Worker(&*handler).handle_connection(&mut stream));
            }
        });

        Ok(socket)
    }
}

struct Worker<'a, H: Handler + 'static>(&'a H);

impl<'a, H: Handler + 'static> Worker<'a, H> {

    fn handle_connection<S>(&self, mut stream: &mut S) where S: NetworkStream + Clone {
        debug!("Incoming stream");
        let addr = match stream.peer_addr() {
            Ok(addr) => addr,
            Err(e) => {
                error!("Peer Name error: {:?}", e);
                return;
            }
        };

        // FIXME: Use Type ascription
        let stream_clone: &mut NetworkStream = &mut stream.clone();
        let rdr = BufReader::new(stream_clone);
        let wrt = BufWriter::new(stream);

        self.keep_alive_loop(rdr, wrt, addr);
        debug!("keep_alive loop ending for {}", addr);
    }

    fn keep_alive_loop<W: Write>(&self, mut rdr: BufReader<&mut NetworkStream>, mut wrt: W, addr: SocketAddr) {
        let mut keep_alive = true;
        while keep_alive {
            let req = match Request::new(&mut rdr, addr) {
                Ok(req) => req,
                Err(Error::Io(ref e)) if e.kind() == io::ErrorKind::ConnectionAborted => {
                    trace!("tcp closed, cancelling keep-alive loop");
                    break;
                }
                Err(Error::Io(e)) => {
                    debug!("ioerror in keepalive loop = {:?}", e);
                    break;
                }
                Err(e) => {
                    //TODO: send a 400 response
                    error!("request error = {:?}", e);
                    break;
                }
            };


            if !self.handle_expect(&req, &mut wrt) {
                break;
            }

            keep_alive = http::should_keep_alive(req.version, &req.headers);
            let version = req.version;
            let mut res_headers = Headers::new();
            if !keep_alive {
                res_headers.set(Connection::close());
            }
            {
                let mut res = Response::new(&mut wrt, &mut res_headers);
                res.version = version;
                self.0.handle(req, res);
            }

            // if the request was keep-alive, we need to check that the server agrees
            // if it wasn't, then the server cannot force it to be true anyways
            if keep_alive {
                keep_alive = http::should_keep_alive(version, &res_headers);
            }

            debug!("keep_alive = {:?} for {}", keep_alive, addr);
        }

    }

    fn handle_expect<W: Write>(&self, req: &Request, wrt: &mut W) -> bool {
         if req.version == HttpVersion::Http11 && req.headers.get() == Some(&Expect::Continue) {
            let status = self.0.check_continue((&req.method, &req.uri, &req.headers));
            match write!(wrt, "{} {}\r\n\r\n", HttpVersion::Http11, status) {
                Ok(..) => (),
                Err(e) => {
                    error!("error writing 100-continue: {:?}", e);
                    return false;
                }
            }

            if status != StatusCode::Continue {
                debug!("non-100 status ({}) for Expect 100 request", status);
                return false;
            }
        }

        true
    }
}

macro_rules! try_return(
    ($e:expr) => {{
        match $e {
            Ok(v) => v,
            Err(e) => { println!("Error: {}", e); return; }
        }
    }}
);
