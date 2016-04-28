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
use std::time::Duration;
use std::cell::UnsafeCell;

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};

use mio::{Evented, EventSet, Token};

use scheduler::{ReadyStates, ReadyType, Scheduler};
use sync::spinlock::Spinlock;
use runtime::notifier::WaiterState;

#[cfg(unix)]
fn make_timeout() -> io::Error {
    use libc;
    io::Error::from_raw_os_error(libc::ETIMEDOUT)
}

#[cfg(windows)]
fn make_timeout() -> io::Error {
    const WSAETIMEDOUT: u32 = 10060;
    io::Error::from_raw_os_error(WSAETIMEDOUT)
}

#[derive(Debug)]
#[doc(hidden)]
pub struct GenericEvented<E: Evented + Debug> {
    inner: UnsafeCell<E>,
    ready_states: ReadyStates,
    token: Token,

    read_timeout: Spinlock<Option<Duration>>,
    write_timeout: Spinlock<Option<Duration>>,
}

impl<E: Evented + Debug> GenericEvented<E> {
    #[doc(hidden)]
    pub fn new(inner: E, interest: EventSet) -> io::Result<GenericEvented<E>> {
        let scheduler = try!(Scheduler::instance_or_err());
        let (token, ready_states) = try!(scheduler.register(&inner, interest));

        Ok(GenericEvented {
            inner: UnsafeCell::new(inner),
            ready_states: ready_states,
            token: token,

            read_timeout: Spinlock::default(),
            write_timeout: Spinlock::default(),
        })
    }

    #[inline]
    fn get_inner_mut(&self) -> &mut E {
        unsafe { &mut *self.inner.get() }
    }

    #[inline]
    fn get_inner(&self) -> &E {
        unsafe { &*self.inner.get() }
    }
}

impl<E: Evented + Debug> Drop for GenericEvented<E> {
    fn drop(&mut self) {
        let scheduler = Scheduler::instance().unwrap();
        scheduler.deregister(self.get_inner(), self.token).unwrap();
    }
}

impl<E: Evented + Debug> Deref for GenericEvented<E> {
    type Target = E;

    fn deref(&self) -> &Self::Target {
        self.get_inner()
    }
}

impl<E: Evented + Debug> DerefMut for GenericEvented<E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.get_inner_mut()
    }
}

impl<E: Evented + Debug + Read> GenericEvented<E> {
    #[inline]
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        *self.read_timeout.lock() = dur;
        Ok(())
    }

    #[inline]
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*self.read_timeout.lock())
    }
}

impl<E: Evented + Debug + Write> GenericEvented<E> {
    #[inline]
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        *self.write_timeout.lock() = dur;
        Ok(())
    }

    #[inline]
    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*self.write_timeout.lock())
    }
}

impl<E: Evented + Debug + Read> Read for GenericEvented<E> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.get_inner_mut().read(buf) {
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

            let state = match *self.read_timeout.lock() {
                None => self.ready_states.wait(ReadyType::Readable),
                Some(t) => self.ready_states.wait_timeout(ReadyType::Readable, t),
            };

            match state {
                WaiterState::Error => {
                    // TODO: How to deal with error?
                }
                WaiterState::Timedout => return Err(make_timeout()),
                _ => {},
            }

            sync_guard.disarm();
        }
    }
}

impl<E: Evented + Debug + Write> Write for GenericEvented<E> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.get_inner_mut().write(buf) {
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
            let state = match *self.write_timeout.lock() {
                None => self.ready_states.wait(ReadyType::Writable),
                Some(t) => self.ready_states.wait_timeout(ReadyType::Writable, t),
            };
            match state {
                WaiterState::Error => {
                    // TODO: How to deal with error?
                }
                WaiterState::Timedout => return Err(make_timeout()),
                _ => {}
            }

            sync_guard.disarm();
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.get_inner_mut().flush() {
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
            let state = match *self.write_timeout.lock() {
                None => self.ready_states.wait(ReadyType::Writable),
                Some(t) => self.ready_states.wait_timeout(ReadyType::Writable, t),
            };
            match state {
                WaiterState::Error => {
                    // TODO: How to deal with error?
                }
                WaiterState::Timedout => return Err(make_timeout()),
                _ => {},
            }

            sync_guard.disarm();
        }
    }
}

#[cfg(unix)]
impl<E: Evented + Debug + AsRawFd> AsRawFd for GenericEvented<E> {
    fn as_raw_fd(&self) -> RawFd {
        self.get_inner().as_raw_fd()
    }
}

impl<'a, E: Evented + Debug + Read + 'a> Read for &'a GenericEvented<E> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.get_inner_mut().read(buf) {
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

            let state = match *self.read_timeout.lock() {
                None => self.ready_states.wait(ReadyType::Readable),
                Some(t) => self.ready_states.wait_timeout(ReadyType::Readable, t),
            };

            match state {
                WaiterState::Error => {
                    // TODO: How to deal with error?
                }
                WaiterState::Timedout => return Err(make_timeout()),
                _ => {},
            }

            sync_guard.disarm();
        }
    }
}

impl<'a, E: Evented + Debug + Write + 'a> Write for &'a GenericEvented<E> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.get_inner_mut().write(buf) {
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
            let state = match *self.write_timeout.lock() {
                None => self.ready_states.wait(ReadyType::Writable),
                Some(t) => self.ready_states.wait_timeout(ReadyType::Writable, t),
            };
            match state {
                WaiterState::Error => {
                    // TODO: How to deal with error?
                }
                WaiterState::Timedout => return Err(make_timeout()),
                _ => {}
            }

            sync_guard.disarm();
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.get_inner_mut().flush() {
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
            let state = match *self.write_timeout.lock() {
                None => self.ready_states.wait(ReadyType::Writable),
                Some(t) => self.ready_states.wait_timeout(ReadyType::Writable, t),
            };
            match state {
                WaiterState::Error => {
                    // TODO: How to deal with error?
                }
                WaiterState::Timedout => return Err(make_timeout()),
                _ => {},
            }

            sync_guard.disarm();
        }
    }
}

unsafe impl<E: Evented + Debug> Send for GenericEvented<E> {}
unsafe impl<E: Evented + Debug> Sync for GenericEvented<E> {}

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
