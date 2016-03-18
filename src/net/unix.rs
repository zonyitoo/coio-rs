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

use std::io;
use std::os::unix::io::{FromRawFd, RawFd};
use std::path::Path;

use mio::EventSet;
use mio::unix::PipeReader as MioPipeReader;
use mio::unix::PipeWriter as MioPipeWriter;
use mio::unix::UnixListener as MioUnixListener;
use mio::unix::UnixSocket as MioUnixSocket;
use mio::unix::UnixStream as MioUnixStream;

use scheduler::ReadyType;
use super::{GenericEvented, SyncGuard};

macro_rules! create_unix_listener {
    ($inner:expr) => (UnixListener::new($inner, EventSet::readable()));
}

macro_rules! create_unix_stream {
    ($inner:expr) => (UnixStream::new($inner, EventSet::readable() | EventSet::writable()));
}

macro_rules! create_pipe_reader {
    ($inner:expr) => (PipeReader::new($inner, EventSet::readable()));
}

macro_rules! create_pipe_writer {
    ($inner:expr) => (PipeWriter::new($inner, EventSet::writable()));
}

#[derive(Debug)]
pub struct UnixSocket {
    inner: MioUnixSocket,
}

impl UnixSocket {
    /// Returns a new, unbound, non-blocking Unix domain socket
    pub fn stream() -> io::Result<UnixSocket> {
        Ok(UnixSocket { inner: try!(MioUnixSocket::stream()) })
    }

    /// Connect the socket to the specified address
    pub fn connect<P: AsRef<Path>>(self, path: P) -> io::Result<(UnixStream, bool)> {
        let (inner, completed) = try!(self.inner.connect(path.as_ref()));
        let stream = try!(create_unix_stream!(inner));
        Ok((stream, completed))
    }

    /// Bind the socket to the specified address
    pub fn bind<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        self.inner.bind(path.as_ref())
    }

    /// Listen for incoming requests
    pub fn listen(self, backlog: usize) -> io::Result<UnixListener> {
        let inner = try!(self.inner.listen(backlog));
        create_unix_listener!(inner)
    }

    pub fn try_clone(&self) -> io::Result<UnixSocket> {
        Ok(UnixSocket { inner: try!(self.inner.try_clone()) })
    }
}

impl FromRawFd for UnixSocket {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixSocket {
        UnixSocket { inner: FromRawFd::from_raw_fd(fd) }
    }
}

pub type UnixListener = GenericEvented<MioUnixListener>;

impl UnixListener {
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<UnixListener> {
        let inner = try!(MioUnixListener::bind(path.as_ref()));
        create_unix_listener!(inner)
    }

    pub fn accept(&self) -> io::Result<UnixStream> {
        let mut sync_guard = SyncGuard::new();

        loop {
            match self.inner.accept() {
                Ok(None) => {
                    trace!("UnixListener({:?}): accept() => WouldBlock", self.token);
                }
                Ok(Some(stream)) => {
                    trace!("UnixListener({:?}): accept() => Ok(..)", self.token);
                    return create_unix_stream!(stream);
                }
                Err(err) => {
                    trace!("UnixListener({:?}): accept() => Err(..)", self.token);
                    return Err(err);
                }
            }

            trace!("UnixListener({:?}): wait(Readable)", self.token);
            self.ready_states.wait(ReadyType::Readable);
            sync_guard.disarm();
        }
    }

    pub fn try_clone(&self) -> io::Result<UnixListener> {
        let inner = try!(self.inner.try_clone());
        create_unix_listener!(inner)
    }
}

impl FromRawFd for UnixListener {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixListener {
        let inner = FromRawFd::from_raw_fd(fd);
        create_unix_listener!(inner).unwrap()
    }
}

pub type UnixStream = GenericEvented<MioUnixStream>;

impl UnixStream {
    pub fn connect<P: AsRef<Path>>(path: &P) -> io::Result<UnixStream> {
        let inner = try!(MioUnixStream::connect(path.as_ref()));
        create_unix_stream!(inner)
    }

    pub fn try_clone(&self) -> io::Result<UnixStream> {
        let inner = try!(self.inner.try_clone());
        create_unix_stream!(inner)
    }
}

impl FromRawFd for UnixStream {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixStream {
        let inner = FromRawFd::from_raw_fd(fd);
        create_unix_stream!(inner).unwrap()
    }
}

pub fn pipe() -> io::Result<(PipeReader, PipeWriter)> {
    let (reader, writer) = try!(::mio::unix::pipe());
    let reader = try!(create_pipe_reader!(reader));
    let writer = try!(create_pipe_writer!(writer));
    Ok((reader, writer))
}

pub type PipeReader = GenericEvented<MioPipeReader>;

impl FromRawFd for PipeReader {
    unsafe fn from_raw_fd(fd: RawFd) -> PipeReader {
        let inner = FromRawFd::from_raw_fd(fd);
        create_pipe_reader!(inner).unwrap()
    }
}

pub type PipeWriter = GenericEvented<MioPipeWriter>;

impl FromRawFd for PipeWriter {
    unsafe fn from_raw_fd(fd: RawFd) -> PipeWriter {
        let inner = FromRawFd::from_raw_fd(fd);
        create_pipe_writer!(inner).unwrap()
    }
}
