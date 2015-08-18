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

use mio::EventSet;

use bytes::{Buf, MutBuf, SliceBuf, MutSliceBuf};

use processor::Processor;

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
        super::each_addr(addr, |a| {
            ::mio::udp::UdpSocket::bound(&a)
        }).map(UdpSocket)
    }

    pub fn try_clone(&self) -> io::Result<UdpSocket> {
        Ok(UdpSocket(try!(self.0.try_clone())))
    }

    pub fn send_to<A: ToSocketAddrs>(&self, slice_buf: &[u8], target: A) -> io::Result<usize> {
        let mut buf = SliceBuf::wrap(slice_buf);

        let mut last_err = Ok(0);
        for addr in try!(target.to_socket_addrs()) {
            match self.0.send_to(&mut buf, &addr) {
                Ok(None) => {
                    debug!("UdpSocket send_to WOULDBLOCK");

                    loop {
                        try!(Processor::current().wait_event(&self.0, EventSet::writable()));

                        match self.0.send_to(&mut buf, &addr) {
                            Ok(None) => {
                                warn!("UdpSocket send_to WOULDBLOCK");
                            },
                            Ok(Some(..)) => {
                                return Ok(slice_buf.len() - buf.remaining());
                            },
                            Err(err) => {
                                return Err(err);
                            }
                        }
                    }
                },
                Ok(Some(..)) => {
                    return Ok(slice_buf.len() - buf.remaining());
                },
                Err(err) => last_err = Err(err),
            }
        }

        last_err
    }

    pub fn recv_from(&self, slice_buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let total_len = slice_buf.len();
        let mut buf = MutSliceBuf::wrap(slice_buf);

        match try!(self.0.recv_from(&mut buf)) {
            None => {
                debug!("UdpSocket recv_from WOULDBLOCK");
            },
            Some(addr) => {
                return Ok((total_len - buf.remaining(), addr));
            }
        }

        loop {
            try!(Processor::current().wait_event(&self.0, EventSet::readable()));

            match try!(self.0.recv_from(&mut buf)) {
                None => {
                    warn!("UdpSocket recv_from WOULDBLOCK");
                },
                Some(addr) => {
                    return Ok((total_len - buf.remaining(), addr));
                }
            }
        }
    }
}

impl Deref for UdpSocket {
    type Target = ::mio::udp::UdpSocket;

    fn deref(&self) -> &::mio::udp::UdpSocket {
        return &self.0
    }
}

impl DerefMut for UdpSocket {
    fn deref_mut(&mut self) -> &mut ::mio::udp::UdpSocket {
        return &mut self.0
    }
}
