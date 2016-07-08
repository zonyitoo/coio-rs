// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! UDP

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

#[cfg(unix)]
use std::os::unix::io::{FromRawFd, RawFd};

use mio::EventSet;
use mio::udp::UdpSocket as MioUdpSocket;

use scheduler::ReadyType;
use super::{each_addr, make_timeout, GenericEvented, SyncGuard};

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
        let inner = try!(self.get_inner().try_clone());
        create_udp_socket!(inner)
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.get_inner_mut().recv_from(buf) {
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

    pub fn send_to(&self, buf: &[u8], target: &SocketAddr) -> io::Result<usize> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.get_inner_mut().send_to(buf, target) {
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
            sync_guard.disarm();

            match *self.read_timeout.lock() {
                None => self.ready_states.wait(ReadyType::Writable),
                Some(t) => {
                    if self.ready_states.wait_timeout(ReadyType::Writable, t) {
                        return Err(make_timeout());
                    }
                }
            }
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
