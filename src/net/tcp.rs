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

pub use mio::tcp::Shutdown;

use std::io;
use std::iter::Iterator;
use std::net::{SocketAddr, ToSocketAddrs};

#[cfg(unix)]
use std::os::unix::io::{FromRawFd, RawFd};

use mio::EventSet;
use mio::tcp::{TcpListener as MioTcpListener, TcpStream as MioTcpStream};

use scheduler::ReadyType;
use super::{each_addr, GenericEvented, SyncGuard};

macro_rules! create_tcp_listener {
    ($inner:expr) => (TcpListener::new($inner, EventSet::readable()));
}

macro_rules! create_tcp_stream {
    ($inner:expr) => (TcpStream::new($inner, EventSet::readable() | EventSet::writable()));
}

pub type TcpListener = GenericEvented<MioTcpListener>;

impl TcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        each_addr(addr, |addr| {
            let inner = try!(MioTcpListener::bind(addr));
            create_tcp_listener!(inner)
        })
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.inner.accept() {
                Ok(None) => {
                    trace!("TcpListener({:?}): accept() => WouldBlock", self.token);
                }
                Ok(Some((stream, addr))) => {
                    trace!("TcpListener({:?}): accept() => Ok(..)", self.token);
                    return create_tcp_stream!(stream).map(|stream| (stream, addr));
                }
                Err(err) => {
                    trace!("TcpListener({:?}): accept() => Err(..)", self.token);
                    return Err(err);
                }
            }

            trace!("TcpListener({:?}): wait(Readable)", self.token);
            self.ready_states.wait(ReadyType::Readable);
            sync_guard.disarm();
        }
    }

    pub fn try_clone(&self) -> io::Result<TcpListener> {
        let inner = try!(self.inner.try_clone());
        create_tcp_listener!(inner)
    }

    pub fn incoming(&self) -> Incoming {
        Incoming(self)
    }
}

#[cfg(unix)]
impl FromRawFd for TcpListener {
    unsafe fn from_raw_fd(fd: RawFd) -> TcpListener {
        let inner = FromRawFd::from_raw_fd(fd);
        create_tcp_listener!(inner).unwrap()
    }
}


pub struct Incoming<'a>(&'a TcpListener);

impl<'a> Iterator for Incoming<'a> {
    type Item = io::Result<(TcpStream, SocketAddr)>;

    fn next(&mut self) -> Option<io::Result<(TcpStream, SocketAddr)>> {
        Some(self.0.accept())
    }
}

pub type TcpStream = GenericEvented<MioTcpStream>;

impl TcpStream {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        each_addr(addr, |addr| {
            let inner = try!(MioTcpStream::connect(addr));
            create_tcp_stream!(inner)
        })
    }

    pub fn try_clone(&self) -> io::Result<TcpStream> {
        let inner = try!(self.inner.try_clone());
        create_tcp_stream!(inner)
    }
}

#[cfg(unix)]
impl FromRawFd for TcpStream {
    unsafe fn from_raw_fd(fd: RawFd) -> TcpStream {
        let inner = FromRawFd::from_raw_fd(fd);
        create_tcp_stream!(inner).unwrap()
    }
}
