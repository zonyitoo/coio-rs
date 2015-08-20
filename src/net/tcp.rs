// The MIT License (MIT)

// Copyright (c) 2015 Rustcc Developers

// Permission is hereby granted, free of charge, to any person obtaining a copy of
// this software and associated documentation files (the "Software"), to deal in
// the Software without restriction, including without limitation the rights to
// use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software is furnished to do so,
// subject to the following conditions:

// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.

// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
// FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
// COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
// IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
// CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

//! TCP

use std::io;
use std::net::{ToSocketAddrs, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::convert::From;
use std::iter::Iterator;

use mio::{self, EventSet};

use processor::Processor;

#[derive(Debug)]
pub struct TcpSocket(::mio::tcp::TcpSocket);

impl TcpSocket {
    /// Returns a new, unbound, non-blocking, IPv4 socket
    pub fn v4() -> io::Result<TcpSocket> {
        Ok(TcpSocket(try!(::mio::tcp::TcpSocket::v4())))
    }

    /// Returns a new, unbound, non-blocking, IPv6 socket
    pub fn v6() -> io::Result<TcpSocket> {
        Ok(TcpSocket(try!(::mio::tcp::TcpSocket::v6())))
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<(TcpStream, bool)> {
        super::each_addr(addr, |a| {
            match a {
                &SocketAddr::V4(..) => try!(TcpSocket::v4()).0.connect(a),
                &SocketAddr::V6(..) => try!(TcpSocket::v6()).0.connect(a),
            }
        }).map(|(stream, complete)| (TcpStream(stream), complete))
    }

    pub fn listen(self, backlog: usize) -> io::Result<TcpListener> {
        Ok(TcpListener(try!(self.0.listen(backlog))))
    }
}

impl Deref for TcpSocket {
    type Target = ::mio::tcp::TcpSocket;

    fn deref(&self) -> &::mio::tcp::TcpSocket {
        &self.0
    }
}

impl DerefMut for TcpSocket {
    fn deref_mut(&mut self) -> &mut ::mio::tcp::TcpSocket {
        &mut self.0
    }
}

#[derive(Debug)]
pub struct TcpListener(::mio::tcp::TcpListener);

impl TcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        super::each_addr(addr, ::mio::tcp::TcpListener::bind).map(TcpListener)
    }

    pub fn accept(&self) -> io::Result<TcpStream> {
        match self.0.accept() {
            Ok(None) => {
                debug!("accept WouldBlock; going to register into eventloop");
            },
            Ok(Some(stream)) => {
                return Ok(TcpStream(stream));
            },
            Err(err) => {
                return Err(err);
            }
        }

        loop {
            try!(Processor::current().wait_event(&self.0, EventSet::readable()));

            match self.0.accept() {
                Ok(None) => {
                    warn!("accept WouldBlock; Coroutine was awaked by readable event");
                },
                Ok(Some(stream)) => {
                    return Ok(TcpStream(stream));
                },
                Err(err) => {
                    return Err(err);
                }
            }
        }
    }

    pub fn try_clone(&self) -> io::Result<TcpListener> {
        Ok(TcpListener(try!(self.0.try_clone())))
    }

    pub fn incoming<'a>(&'a self) -> Incoming<'a> {
        Incoming(self)
    }
}

impl Deref for TcpListener {
    type Target = ::mio::tcp::TcpListener;

    fn deref(&self) -> &::mio::tcp::TcpListener {
        &self.0
    }
}

impl DerefMut for TcpListener {
    fn deref_mut(&mut self) -> &mut ::mio::tcp::TcpListener {
        &mut self.0
    }
}

pub struct Incoming<'a>(&'a TcpListener);

impl<'a> Iterator for Incoming<'a> {
    type Item = io::Result<TcpStream>;

    fn next(&mut self) -> Option<io::Result<TcpStream>> {
        Some(self.0.accept())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shutdown {
    /// Further receptions will be disallowed.
    Read,
    /// Further  transmissions will be disallowed.
    Write,
    /// Further receptions and transmissions will be disallowed.
    Both,
}

impl From<Shutdown> for mio::tcp::Shutdown {
    fn from(shutdown: Shutdown) -> mio::tcp::Shutdown {
        match shutdown {
            Shutdown::Read => mio::tcp::Shutdown::Read,
            Shutdown::Write => mio::tcp::Shutdown::Write,
            Shutdown::Both => mio::tcp::Shutdown::Both,
        }
    }
}

#[derive(Debug)]
pub struct TcpStream(mio::tcp::TcpStream);

impl TcpStream {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        match TcpSocket::connect(addr) {
            Ok((stream, completed)) => {
                if !completed {
                    try!(Processor::current().wait_event(&stream.0, EventSet::writable()));
                    try!(stream.take_socket_error());
                }

                Ok(stream)
            },
            Err(err) => Err(err)
        }
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    pub fn try_clone(&self) -> io::Result<TcpStream> {
        let stream = try!(self.0.try_clone());

        Ok(TcpStream(stream))
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.shutdown(From::from(how))
    }

    pub fn take_socket_error(&self) -> io::Result<()> {
        self.0.take_socket_error()
    }
}

impl io::Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use mio::TryRead;

        match self.0.try_read(buf) {
            Ok(None) => {
                debug!("TcpStream read WouldBlock");
            },
            Ok(Some(len)) => {
                debug!("TcpStream read {} bytes", len);
                return Ok(len);
            },
            Err(err) => {
                return Err(err);
            }
        }

        loop {
            debug!("Read: Going to register event");
            try!(Processor::current().wait_event(&self.0, EventSet::readable()));
            debug!("Read: Got read event");

            match self.0.try_read(buf) {
                Ok(None) => {
                    debug!("TcpStream read WouldBlock");
                },
                Ok(Some(len)) => {
                    debug!("TcpStream read {} bytes", len);
                    return Ok(len);
                },
                Err(err) => {
                    return Err(err);
                }
            }
        }
    }
}

impl io::Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use mio::TryWrite;

        match self.0.try_write(buf) {
            Ok(None) => {
                debug!("TcpStream write WouldBlock");
            },
            Ok(Some(len)) => {
                debug!("TcpStream written {} bytes", len);
                return Ok(len);
            },
            Err(err) => {
                return Err(err)
            }
        }

        loop {
            debug!("Write: Going to register event");
            try!(Processor::current().wait_event(&self.0, EventSet::writable()));
            debug!("Write: Got write event");

            match self.0.try_write(buf) {
                Ok(None) => {
                    debug!("TcpStream write WouldBlock");
                },
                Ok(Some(len)) => {
                    debug!("TcpStream written {} bytes", len);
                    return Ok(len);
                },
                Err(err) => {
                    return Err(err)
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Deref for TcpStream {
    type Target = ::mio::tcp::TcpStream;

    fn deref(&self) -> &::mio::tcp::TcpStream {
        &self.0
    }
}

impl DerefMut for TcpStream {
    fn deref_mut(&mut self) -> &mut ::mio::tcp::TcpStream {
        &mut self.0
    }
}
