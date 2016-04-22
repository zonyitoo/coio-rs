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

//! Asynchronous network library

pub mod tcp;
pub mod udp;

#[cfg(unix)]
pub mod unix;

pub use self::tcp::{TcpListener, TcpStream, Shutdown};
pub use self::udp::UdpSocket;

#[cfg(unix)]
pub use self::unix::{UnixListener, UnixStream, UnixSocket};

use std::fmt::Debug;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::ops::{Deref, DerefMut};

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};

use mio::{Evented, EventSet, Token};

use scheduler::{ReadyStates, ReadyType, Scheduler};


#[derive(Debug)]
#[doc(hidden)]
pub struct GenericEvented<E: Evented + Debug> {
    inner: E,
    ready_states: ReadyStates,
    token: Token,
}

impl<E: Evented + Debug> GenericEvented<E> {
    #[doc(hidden)]
    pub fn new(inner: E, interest: EventSet) -> io::Result<GenericEvented<E>> {
        let scheduler = try!(Scheduler::instance_or_err());
        let (token, ready_states) = try!(scheduler.register(&inner, interest));

        Ok(GenericEvented {
            inner: inner,
            ready_states: ready_states,
            token: token,
        })
    }
}

impl<E: Evented + Debug> Drop for GenericEvented<E> {
    fn drop(&mut self) {
        let scheduler = Scheduler::instance().unwrap();
        scheduler.deregister(&self.inner, self.token).unwrap();
    }
}

impl<E: Evented + Debug> Deref for GenericEvented<E> {
    type Target = E;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<E: Evented + Debug> DerefMut for GenericEvented<E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<E: Evented + Debug + Read> Read for GenericEvented<E> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.inner.read(buf) {
                Ok(len) => {
                    trace!("GenericEvented({:?}): read() => Ok({})", self.token, len);
                    return Ok(len);
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    trace!("GenericEvented({:?}): read() => WouldBlock", self.token);
                }
                Err(ref err) if err.kind() == io::ErrorKind::NotConnected => {
                    trace!("GenericEvented({:?}): read() => NotConnected", self.token);
                }
                Err(err) => {
                    trace!("GenericEvented({:?}): read() => Err(..)", self.token);
                    return Err(err);
                }
            }

            trace!("GenericEvented({:?}): wait(Readable)", self.token);
            self.ready_states.wait(ReadyType::Readable);
            sync_guard.disarm();
        }
    }
}

impl<E: Evented + Debug + Write> Write for GenericEvented<E> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.inner.write(buf) {
                Ok(len) => {
                    trace!("GenericEvented({:?}): write() => Ok({})", self.token, len);
                    return Ok(len);
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    trace!("GenericEvented({:?}): write() => WouldBlock", self.token);
                }
                Err(ref err) if err.kind() == io::ErrorKind::NotConnected => {
                    trace!("GenericEvented({:?}): write() => NotConnected", self.token);
                }
                Err(err) => {
                    trace!("GenericEvented({:?}): write() => Err(..)", self.token);
                    return Err(err);
                }
            }

            trace!("GenericEvented({:?}): wait(Writable)", self.token);
            self.ready_states.wait(ReadyType::Writable);
            sync_guard.disarm();
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.inner.flush() {
                Ok(()) => {
                    trace!("GenericEvented({:?}): write() => Ok(())", self.token);
                    return Ok(());
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    trace!("GenericEvented({:?}): flush() => WouldBlock", self.token);
                }
                Err(ref err) if err.kind() == io::ErrorKind::NotConnected => {
                    trace!("GenericEvented({:?}): flush() => NotConnected", self.token);
                }
                Err(err) => {
                    trace!("GenericEvented({:?}): flush() => Err(..)", self.token);
                    return Err(err);
                }
            }

            trace!("GenericEvented({:?}): wait(Writable)", self.token);
            self.ready_states.wait(ReadyType::Writable);
            sync_guard.disarm();
        }
    }
}

#[cfg(unix)]
impl<E: Evented + Debug + AsRawFd> AsRawFd for GenericEvented<E> {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}


struct SyncGuard(bool);

impl SyncGuard {
    #[inline]
    pub fn new() -> SyncGuard {
        SyncGuard(true)
    }

    #[inline]
    pub fn disarm(&mut self) {
        self.0 = false;
    }
}

impl Drop for SyncGuard {
    fn drop(&mut self) {
        if self.0 {
            Scheduler::sched();
        }
    }
}


// Credit goes to std::net::each_addr
fn each_addr<A: ToSocketAddrs, F, T>(addr: A, mut f: F) -> io::Result<T>
    where F: FnMut(&SocketAddr) -> io::Result<T>
{
    let mut last_err = None;

    for addr in try!(addr.to_socket_addrs()) {
        match f(&addr) {
            Ok(l) => return Ok(l),
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput,
                       "could not resolve to any addresses")
    }))
}
