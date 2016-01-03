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

//! Unix domain socket

use std::io::{self, Read, Write, ErrorKind};
use std::path::Path;
use std::ops::{Deref, DerefMut};
use std::convert::From;
use std::os::unix::io::{RawFd, AsRawFd, FromRawFd};

use mio::{TryRead, TryWrite, TryAccept, EventSet};

use scheduler::Scheduler;

#[derive(Debug)]
pub struct UnixSocket(::mio::unix::UnixSocket);

impl UnixSocket {
    /// Returns a new, unbound, non-blocking Unix domain socket
    pub fn stream() -> io::Result<UnixSocket> {
        ::mio::unix::UnixSocket::stream().map(UnixSocket)
    }

    /// Connect the socket to the specified address
    pub fn connect<P: AsRef<Path> + ?Sized>(self, addr: &P) -> io::Result<(UnixStream, bool)> {
        self.0.connect(addr).map(|(s, completed)| (UnixStream(s), completed))
    }

    /// Bind the socket to the specified address
    pub fn bind<P: AsRef<Path> + ?Sized>(&self, addr: &P) -> io::Result<()> {
        self.0.bind(addr)
    }

    /// Listen for incoming requests
    pub fn listen(self, backlog: usize) -> io::Result<UnixListener> {
        self.0.listen(backlog).map(UnixListener)
    }

    pub fn try_clone(&self) -> io::Result<UnixSocket> {
        self.0.try_clone().map(UnixSocket)
    }
}

impl Deref for UnixSocket {
    type Target = ::mio::unix::UnixSocket;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for UnixSocket {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<::mio::unix::UnixSocket> for UnixSocket {
    fn from(sock: ::mio::unix::UnixSocket) -> UnixSocket {
        UnixSocket(sock)
    }
}

impl AsRawFd for UnixSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl FromRawFd for UnixSocket {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixSocket {
        UnixSocket(FromRawFd::from_raw_fd(fd))
    }
}

#[derive(Debug)]
pub struct UnixStream(::mio::unix::UnixStream);

impl UnixStream {
    pub fn connect<P: AsRef<Path> + ?Sized>(path: &P) -> io::Result<UnixStream> {
        ::mio::unix::UnixStream::connect(path).map(UnixStream)
    }

    pub fn try_clone(&self) -> io::Result<UnixStream> {
        self.0.try_clone().map(UnixStream)
    }
}

impl Read for UnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.try_read(buf) {
            Ok(None) => {
                debug!("UnixStream read WouldBlock");
            }
            Ok(Some(len)) => {
                debug!("UnixStream read {} bytes", len);
                return Ok(len);
            }

            Err(err) => {
                return Err(err);
            }
        }

        loop {
            debug!("Read: Going to register event");
            try!(Scheduler::instance().unwrap().wait_event(&self.0, EventSet::readable()));
            debug!("Read: Got read event");

            match self.0.try_read(buf) {
                Ok(None) => {
                    debug!("UnixStream read WouldBlock");
                }
                Ok(Some(len)) => {
                    debug!("UnixStream read {} bytes", len);
                    return Ok(len);
                }
                Err(err) => {
                    return Err(err);
                }
            }
        }
    }
}

impl Write for UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.0.try_write(buf) {
            Ok(None) => {
                debug!("UnixStream write WouldBlock");
            }
            Ok(Some(len)) => {
                debug!("UnixStream written {} bytes", len);
                return Ok(len);
            }
            Err(err) => return Err(err),
        }

        loop {
            debug!("Write: Going to register event");
            try!(Scheduler::instance().unwrap().wait_event(&self.0, EventSet::writable()));
            debug!("Write: Got write event");

            match self.0.try_write(buf) {
                Ok(None) => {
                    debug!("UnixStream write WouldBlock");
                }
                Ok(Some(len)) => {
                    debug!("UnixStream written {} bytes", len);
                    return Ok(len);
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.0.flush() {
            Ok(..) => return Ok(()),
            Err(ref err) if err.kind() == ErrorKind::WouldBlock => {
                debug!("UnixStream flush WouldBlock");
            }
            Err(err) => return Err(err),
        }

        loop {
            debug!("Write: Going to register event");
            try!(Scheduler::instance().unwrap().wait_event(&self.0, EventSet::writable()));
            debug!("Write: Got write event");

            match self.0.flush() {
                Ok(..) => return Ok(()),
                Err(ref err) if err.kind() == ErrorKind::WouldBlock => {
                    debug!("UnixStream flush WouldBlock");
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl Deref for UnixStream {
    type Target = ::mio::unix::UnixStream;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for UnixStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<::mio::unix::UnixStream> for UnixStream {
    fn from(sock: ::mio::unix::UnixStream) -> UnixStream {
        UnixStream(sock)
    }
}

impl AsRawFd for UnixStream {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl FromRawFd for UnixStream {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixStream {
        UnixStream(FromRawFd::from_raw_fd(fd))
    }
}

#[derive(Debug)]
pub struct UnixListener(::mio::unix::UnixListener);

impl UnixListener {
    pub fn bind<P: AsRef<Path> + ?Sized>(addr: &P) -> io::Result<UnixListener> {
        ::mio::unix::UnixListener::bind(addr).map(UnixListener)
    }

    pub fn accept(&self) -> io::Result<UnixStream> {
        match self.0.accept() {
            Ok(None) => {
                debug!("UnixListener accept WouldBlock; going to register into eventloop");
            }
            Ok(Some(stream)) => {
                return Ok(UnixStream(stream));
            }
            Err(err) => {
                return Err(err);
            }
        }

        loop {
            try!(Scheduler::instance().unwrap().wait_event(&self.0, EventSet::readable()));

            match self.0.accept() {
                Ok(None) => {
                    warn!("UnixListener accept WouldBlock; Coroutine was awaked by readable event");
                }
                Ok(Some(stream)) => {
                    return Ok(UnixStream(stream));
                }
                Err(err) => {
                    return Err(err);
                }
            }
        }
    }

    pub fn try_clone(&self) -> io::Result<UnixListener> {
        self.0.try_clone().map(UnixListener)
    }
}

impl Deref for UnixListener {
    type Target = ::mio::unix::UnixListener;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for UnixListener {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<::mio::unix::UnixListener> for UnixListener {
    fn from(listener: ::mio::unix::UnixListener) -> UnixListener {
        UnixListener(listener)
    }
}

impl AsRawFd for UnixListener {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl FromRawFd for UnixListener {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixListener {
        UnixListener(FromRawFd::from_raw_fd(fd))
    }
}

pub fn pipe() -> io::Result<(PipeReader, PipeWriter)> {
    ::mio::unix::pipe().map(|(r, w)| (PipeReader(r), PipeWriter(w)))
}

#[derive(Debug)]
pub struct PipeReader(::mio::unix::PipeReader);

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.try_read(buf) {
            Ok(None) => {
                debug!("PipeReader read WouldBlock");
            }
            Ok(Some(len)) => {
                debug!("PipeReader read {} bytes", len);
                return Ok(len);
            }

            Err(err) => {
                return Err(err);
            }
        }

        loop {
            debug!("Read: Going to register event");
            try!(Scheduler::instance().unwrap().wait_event(&self.0, EventSet::readable()));
            debug!("Read: Got read event");

            match self.0.try_read(buf) {
                Ok(None) => {
                    debug!("PipeReader read WouldBlock");
                }
                Ok(Some(len)) => {
                    debug!("PipeReader read {} bytes", len);
                    return Ok(len);
                }
                Err(err) => {
                    return Err(err);
                }
            }
        }
    }
}

impl Deref for PipeReader {
    type Target = ::mio::unix::PipeReader;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PipeReader {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<::mio::unix::PipeReader> for PipeReader {
    fn from(listener: ::mio::unix::PipeReader) -> PipeReader {
        PipeReader(listener)
    }
}

impl AsRawFd for PipeReader {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl FromRawFd for PipeReader {
    unsafe fn from_raw_fd(fd: RawFd) -> PipeReader {
        PipeReader(FromRawFd::from_raw_fd(fd))
    }
}

#[derive(Debug)]
pub struct PipeWriter(::mio::unix::PipeWriter);

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.0.try_write(buf) {
            Ok(None) => {
                debug!("PipeWriter write WouldBlock");
            }
            Ok(Some(len)) => {
                debug!("PipeWriter written {} bytes", len);
                return Ok(len);
            }
            Err(err) => return Err(err),
        }

        loop {
            debug!("Write: Going to register event");
            try!(Scheduler::instance().unwrap().wait_event(&self.0, EventSet::writable()));
            debug!("Write: Got write event");

            match self.0.try_write(buf) {
                Ok(None) => {
                    debug!("PipeWriter write WouldBlock");
                }
                Ok(Some(len)) => {
                    debug!("PipeWriter written {} bytes", len);
                    return Ok(len);
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.0.flush() {
            Ok(..) => return Ok(()),
            Err(ref err) if err.kind() == ErrorKind::WouldBlock => {
                debug!("PipeWriter flush WouldBlock");
            }
            Err(err) => return Err(err),
        }

        loop {
            debug!("Write: Going to register event");
            try!(Scheduler::instance().unwrap().wait_event(&self.0, EventSet::writable()));
            debug!("Write: Got write event");

            match self.0.flush() {
                Ok(..) => return Ok(()),
                Err(ref err) if err.kind() == ErrorKind::WouldBlock => {
                    debug!("PipeWriter flush WouldBlock");
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl Deref for PipeWriter {
    type Target = ::mio::unix::PipeWriter;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PipeWriter {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<::mio::unix::PipeWriter> for PipeWriter {
    fn from(listener: ::mio::unix::PipeWriter) -> PipeWriter {
        PipeWriter(listener)
    }
}

impl AsRawFd for PipeWriter {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl FromRawFd for PipeWriter {
    unsafe fn from_raw_fd(fd: RawFd) -> PipeWriter {
        PipeWriter(FromRawFd::from_raw_fd(fd))
    }
}
