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

//! UDP

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

#[cfg(unix)]
use std::os::unix::io::{FromRawFd, RawFd};

use mio::EventSet;
use mio::udp::UdpSocket as MioUdpSocket;

use scheduler::ReadyType;
use super::{each_addr, GenericEvented, SyncGuard};

macro_rules! create_udp_socket {
    ($inner:expr) => (UdpSocket::new($inner, EventSet::readable() | EventSet::writable()));
}

pub type UdpSocket = GenericEvented<MioUdpSocket>;

impl UdpSocket {
    /// Returns a new, unbound, non-blocking, IPv4 UDP socket
    pub fn v4() -> io::Result<UdpSocket> {
        let inner = try!(MioUdpSocket::v4());
        create_udp_socket!(inner)
    }

    /// Returns a new, unbound, non-blocking, IPv6 UDP socket
    pub fn v6() -> io::Result<UdpSocket> {
        let inner = try!(MioUdpSocket::v6());
        create_udp_socket!(inner)
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        each_addr(addr, |addr| {
            let sock = try!(match *addr {
                SocketAddr::V4(..) => Self::v4(),
                SocketAddr::V6(..) => Self::v6(),
            });

            try!(sock.bind(addr));

            Ok(sock)
        })
    }

    pub fn try_clone(&self) -> io::Result<UdpSocket> {
        let inner = try!(self.inner.try_clone());
        create_udp_socket!(inner)
    }

    pub fn send_to(&self, buf: &[u8], target: &SocketAddr) -> io::Result<usize> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.inner.send_to(buf, target) {
                Ok(None) => {
                    trace!("UdpSocket({:?}): send_to() => WouldBlock", self.token);
                }
                Ok(Some(len)) => {
                    trace!("UdpSocket({:?}): send_to() => Ok({})", self.token, len);
                    return Ok(len);
                }
                Err(err) => {
                    trace!("UdpSocket({:?}): send_to() => Err(..)", self.token);
                    return Err(err);
                }
            }

            trace!("UdpSocket({:?}): wait(Writable)", self.token);
            self.ready_states.wait(ReadyType::Writable);
            sync_guard.disarm();
        }
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.inner.recv_from(buf) {
                Ok(None) => {
                    trace!("UdpSocket({:?}): recv_from() => WouldBlock", self.token);
                }
                Ok(Some(t)) => {
                    trace!("UdpSocket({:?}): recv_from() => Ok(..)", self.token);
                    return Ok(t);
                }
                Err(err) => {
                    trace!("UdpSocket({:?}): recv_from() => Err(..)", self.token);
                    return Err(err);
                }
            }

            trace!("UdpSocket({:?}): wait(Readable)", self.token);
            self.ready_states.wait(ReadyType::Readable);
            sync_guard.disarm();
        }
    }
}

#[cfg(unix)]
impl FromRawFd for UdpSocket {
    unsafe fn from_raw_fd(fd: RawFd) -> UdpSocket {
        let inner = FromRawFd::from_raw_fd(fd);
        create_udp_socket!(inner).unwrap()
    }
}
