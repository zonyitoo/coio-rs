// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Asynchronous network library

pub mod tcp;
pub mod udp;

#[cfg(unix)]
pub mod unix;

pub use self::tcp::{TcpListener, TcpStream, Shutdown};
pub use self::udp::UdpSocket;

#[cfg(unix)]
pub use self::unix::{UnixListener, UnixStream, UnixSocket};

use std::cell::UnsafeCell;
use std::fmt::Debug;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::ops::{Deref, DerefMut};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};

use mio::{Evented, EventSet, Token};

use sync::condvar::WaiterState;
use scheduler::{ReadyStates, ReadyType, Scheduler};
use sync::spinlock::Spinlock;

#[cfg(unix)]
fn make_timeout() -> io::Error {
    use libc;
    io::Error::from_raw_os_error(libc::ETIMEDOUT)
}

#[cfg(windows)]
fn make_timeout() -> io::Error {
    const WSAETIMEDOUT: i32 = 10060;
    io::Error::from_raw_os_error(WSAETIMEDOUT)
}

#[derive(Debug)]
#[doc(hidden)]
pub struct GenericEvented<E: Evented + Debug> {
    inner: UnsafeCell<E>,
    token: Token,
    ready_states: ReadyStates,
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
            token: token,
            ready_states: ready_states,
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
        let scheduler = Scheduler::instance().expect("Cannot drop socket outside of Scheduler");
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

impl<'a, E: Evented + Debug + Read + 'a> GenericEvented<E> {
    #[inline]
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        *self.read_timeout.lock() = dur;
        Ok(())
    }

    #[inline]
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*self.read_timeout.lock())
    }

    fn read_inner(&self, buf: &mut [u8]) -> io::Result<usize> {
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
}

impl<'a, E: Evented + Debug + Write + 'a> GenericEvented<E> {
    #[inline]
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        *self.write_timeout.lock() = dur;
        Ok(())
    }

    #[inline]
    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*self.write_timeout.lock())
    }

    fn write_inner(&self, buf: &[u8]) -> io::Result<usize> {
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

    fn flush_inner(&self) -> io::Result<()> {
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

impl<E: Evented + Debug + Read> Read for GenericEvented<E> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_inner(buf)
    }
}

impl<E: Evented + Debug + Write> Write for GenericEvented<E> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_inner(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.flush_inner()
    }
}

#[cfg(unix)]
impl<E: Evented + Debug + AsRawFd> AsRawFd for GenericEvented<E> {
    fn as_raw_fd(&self) -> RawFd {
        self.get_inner().as_raw_fd()
    }
}

impl<'a, E: Evented + Debug + Read + 'a> Read for &'a GenericEvented<E> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_inner(buf)
    }
}

impl<'a, E: Evented + Debug + Write + 'a> Write for &'a GenericEvented<E> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_inner(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.flush_inner()
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
