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

use std::ops::{Deref, DerefMut};
use std::io;
use std::net::{ToSocketAddrs, SocketAddr};

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};

use mio::EventSet;

use scheduler::Scheduler;

pub struct UdpSocket(::mio::udp::UdpSocket);

impl UdpSocket {
    /// Returns a new, unbound, non-blocking, IPv4 UDP socket
    pub fn v4() -> io::Result<UdpSocket> {
        Ok(UdpSocket(try!(::mio::udp::UdpSocket::v4())))
    }

    /// Returns a new, unbound, non-blocking, IPv6 UDP socket
    pub fn v6() -> io::Result<UdpSocket> {
        Ok(UdpSocket(try!(::mio::udp::UdpSocket::v6())))
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        super::each_addr(addr, |a| ::mio::udp::UdpSocket::bound(&a)).map(UdpSocket)
    }

    pub fn try_clone(&self) -> io::Result<UdpSocket> {
        Ok(UdpSocket(try!(self.0.try_clone())))
    }

    pub fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> io::Result<usize> {
        let mut last_err = Ok(0);
        for addr in try!(target.to_socket_addrs()) {
            match self.0.send_to(buf, &addr) {
                Ok(None) => {
                    debug!("UdpSocket send_to WOULDBLOCK");

                    loop {
                        try!(Scheduler::instance()
                                 .unwrap()
                                 .wait_event(&self.0, EventSet::writable()));

                        match self.0.send_to(buf, &addr) {
                            Ok(None) => {
                                warn!("UdpSocket send_to WOULDBLOCK");
                            }
                            Ok(Some(len)) => {
                                return Ok(len);
                            }
                            Err(err) => {
                                return Err(err);
                            }
                        }
                    }
                }
                Ok(Some(len)) => {
                    return Ok(len);
                }
                Err(err) => last_err = Err(err),
            }
        }

        last_err
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        match try!(self.0.recv_from(buf)) {
            None => {
                debug!("UdpSocket recv_from WOULDBLOCK");
            }
            Some(ret) => {
                return Ok(ret);
            }
        }

        loop {
            try!(Scheduler::instance().unwrap().wait_event(&self.0, EventSet::readable()));

            match try!(self.0.recv_from(buf)) {
                None => {
                    warn!("UdpSocket recv_from WOULDBLOCK");
                }
                Some(ret) => {
                    return Ok(ret);
                }
            }
        }
    }
}

impl Deref for UdpSocket {
    type Target = ::mio::udp::UdpSocket;

    fn deref(&self) -> &::mio::udp::UdpSocket {
        &self.0
    }
}

impl DerefMut for UdpSocket {
    fn deref_mut(&mut self) -> &mut ::mio::udp::UdpSocket {
        &mut self.0
    }
}

#[cfg(unix)]
impl AsRawFd for UdpSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(unix)]
impl FromRawFd for UdpSocket {
    unsafe fn from_raw_fd(fd: RawFd) -> UdpSocket {
        UdpSocket(FromRawFd::from_raw_fd(fd))
    }
}
