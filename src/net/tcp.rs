// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

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
use super::{each_addr, make_timeout, GenericEvented, SyncGuard};

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
            match self.get_inner().accept() {
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
            sync_guard.disarm();

            match *self.read_timeout.lock() {
                None => self.ready_states.wait(ReadyType::Readable),
                Some(t) => {
                    if self.ready_states.wait_timeout(ReadyType::Readable, t) {
                        return Err(make_timeout());
                    }
                }
            }
        }
    }

    pub fn try_clone(&self) -> io::Result<TcpListener> {
        let inner = try!(self.get_inner().try_clone());
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
        let inner = try!(self.get_inner().try_clone());
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
