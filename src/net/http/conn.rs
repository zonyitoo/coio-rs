// The MIT License (MIT)

// Copyright (c) 2015 Y. T. Chung <zonyitoo@gmail.com>

//  Permission is hereby granted, free of charge, to any person obtaining a
//  copy of this software and associated documentation files (the "Software"),
//  to deal in the Software without restriction, including without limitation
//  the rights to use, copy, modify, merge, publish, distribute, sublicense,
//  and/or sell copies of the Software, and to permit persons to whom the
//  Software is furnished to do so, subject to the following conditions:
//
//  The above copyright notice and this permission notice shall be included in
//  all copies or substantial portions of the Software.
//
//  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//  OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//  FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//  AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//  LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//  FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//  DEALINGS IN THE SOFTWARE.

#![allow(dead_code)]

use std::io::{self, Read, Write};
use std::net::{ToSocketAddrs, SocketAddr, Shutdown};
use std::fmt;
use std::convert::From;

use hyper;
use hyper::net::{NetworkListener, NetworkStream, NetworkConnector};

use net::tcp::{TcpStream, TcpListener};
use net;

pub struct HttpListener(TcpListener);

impl Clone for HttpListener {
    fn clone(&self) -> HttpListener {
        HttpListener(self.0.try_clone().unwrap())
    }
}

impl HttpListener {

    /// Start listening to an address over HTTP.
    pub fn new<To: ToSocketAddrs>(addr: To) -> io::Result<HttpListener> {
        Ok(HttpListener(try!(TcpListener::bind(addr))))
    }

}

impl NetworkListener for HttpListener {
    type Stream = HttpStream;

    #[inline]
    fn accept(&mut self) -> hyper::Result<HttpStream> {
        Ok(HttpStream(try!(self.0.accept())))
    }

    #[inline]
    fn local_addr(&mut self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}

pub struct HttpStream(TcpStream);

impl Clone for HttpStream {
    fn clone(&self) -> HttpStream {
        HttpStream(self.0.try_clone().unwrap())
    }
}

impl Read for HttpStream {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for HttpStream {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl fmt::Debug for HttpStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "HttpStream(_)")
    }
}

impl NetworkStream for HttpStream {
    #[inline]
    fn peer_addr(&mut self) -> io::Result<SocketAddr> {
        self.0.peer_addr()
    }

    #[inline]
    fn close(&mut self, how: Shutdown) -> io::Result<()> {
        let how = match how {
            Shutdown::Read => net::Shutdown::Read,
            Shutdown::Write => net::Shutdown::Write,
            Shutdown::Both => net::Shutdown::Both,
        };

        self.0.shutdown(how)
    }
}

/// A connector that will produce HttpStreams.
#[derive(Debug, Clone, Default)]
pub struct HttpConnector;

impl NetworkConnector for HttpConnector {
    type Stream = HttpStream;

    fn connect(&self, host: &str, port: u16, scheme: &str) -> hyper::Result<HttpStream> {
        let addr = &(host, port);
        Ok(try!(match scheme {
            "http" => {
                debug!("http scheme");
                Ok(HttpStream(try!(TcpStream::connect(addr))))
            },
            _ => {
                Err(io::Error::new(io::ErrorKind::InvalidInput,
                                   "Invalid scheme for Http"))
            }
        }))
    }
}

/// A stream over the HTTP protocol, possibly protected by SSL.
#[derive(Debug, Clone)]
pub enum HttpsStream<S: NetworkStream> {
    /// A plain text stream.
    Http(HttpStream),
    /// A stream protected by SSL.
    Https(S)
}

impl<S: NetworkStream> Read for HttpsStream<S> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match *self {
            HttpsStream::Http(ref mut s) => s.read(buf),
            HttpsStream::Https(ref mut s) => s.read(buf)
        }
    }
}

impl<S: NetworkStream> Write for HttpsStream<S> {
    #[inline]
    fn write(&mut self, msg: &[u8]) -> io::Result<usize> {
        match *self {
            HttpsStream::Http(ref mut s) => s.write(msg),
            HttpsStream::Https(ref mut s) => s.write(msg)
        }
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        match *self {
            HttpsStream::Http(ref mut s) => s.flush(),
            HttpsStream::Https(ref mut s) => s.flush()
        }
    }
}

impl<S: NetworkStream> NetworkStream for HttpsStream<S> {
    #[inline]
    fn peer_addr(&mut self) -> io::Result<SocketAddr> {
        match *self {
            HttpsStream::Http(ref mut s) => s.peer_addr(),
            HttpsStream::Https(ref mut s) => s.peer_addr()
        }
    }

    #[inline]
    fn close(&mut self, how: Shutdown) -> io::Result<()> {
        match *self {
            HttpsStream::Http(ref mut s) => s.close(how),
            HttpsStream::Https(ref mut s) => s.close(how)
        }
    }
}

/// A Http Listener over SSL.
#[derive(Clone)]
pub struct HttpsListener<S: Ssl> {
    listener: HttpListener,
    ssl: S,
}

impl<S: Ssl> HttpsListener<S> {

    /// Start listening to an address over HTTPS.
    pub fn new<To: ToSocketAddrs>(addr: To, ssl: S) -> hyper::Result<HttpsListener<S>> {
        HttpListener::new(addr).map(|l| HttpsListener {
            listener: l,
            ssl: ssl
        }).map_err(From::from)
    }

}

impl<S: Ssl + Clone> NetworkListener for HttpsListener<S> {
    type Stream = S::Stream;

    #[inline]
    fn accept(&mut self) -> hyper::Result<S::Stream> {
        self.listener.accept().and_then(|s| self.ssl.wrap_server(s))
    }

    #[inline]
    fn local_addr(&mut self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

/// A connector that can protect HTTP streams using SSL.
#[derive(Debug, Default)]
pub struct HttpsConnector<S: Ssl> {
    ssl: S
}

impl<S: Ssl> HttpsConnector<S> {
    /// Create a new connector using the provided SSL implementation.
    pub fn new(s: S) -> HttpsConnector<S> {
        HttpsConnector { ssl: s }
    }
}

impl<S: Ssl> NetworkConnector for HttpsConnector<S> {
    type Stream = HttpsStream<S::Stream>;

    fn connect(&self, host: &str, port: u16, scheme: &str) -> hyper::Result<Self::Stream> {
        let addr = &(host, port);
        if scheme == "https" {
            debug!("https scheme");
            let stream = HttpStream(try!(TcpStream::connect(addr)));
            self.ssl.wrap_client(stream, host).map(HttpsStream::Https)
        } else {
            HttpConnector.connect(host, port, scheme).map(HttpsStream::Http)
        }
    }
}

/// An abstraction to allow any SSL implementation to be used with HttpsStreams.
pub trait Ssl {
    /// The protected stream.
    type Stream: NetworkStream + Send + Clone;
    /// Wrap a client stream with SSL.
    fn wrap_client(&self, stream: HttpStream, host: &str) -> hyper::Result<Self::Stream>;
    /// Wrap a server stream with SSL.
    fn wrap_server(&self, stream: HttpStream) -> hyper::Result<Self::Stream>;
}

#[cfg(not(feature = "openssl"))]
#[doc(hidden)]
pub type DefaultConnector = HttpConnector;

#[cfg(feature = "openssl")]
#[doc(hidden)]
pub type DefaultConnector = HttpsConnector<self::openssl::Openssl>;

#[cfg(feature = "openssl")]
mod openssl {
    use std::io;
    use std::path::Path;
    use std::sync::Arc;
    use openssl::ssl::{Ssl, SslContext, SslStream, SslMethod, SSL_VERIFY_NONE};
    use openssl::ssl::error::StreamError as SslIoError;
    use openssl::ssl::error::SslError;
    use openssl::x509::X509FileType;
    use super::HttpStream;
    use hyper;


    #[derive(Debug, Clone)]
    pub struct Openssl {
        /// The `SslContext` from openssl crate.
        pub context: Arc<SslContext>
    }

    impl Default for Openssl {
        fn default() -> Openssl {
            Openssl {
                context: Arc::new(SslContext::new(SslMethod::Sslv23).unwrap_or_else(|e| {
                    // if we cannot create a SslContext, that's because of a
                    // serious problem. just crash.
                    panic!("{}", e)
                }))
            }
        }
    }

    impl Openssl {
        /// Ease creating an `Openssl` with a certificate and key.
        pub fn with_cert_and_key<C, K>(cert: C, key: K) -> Result<Openssl, SslError>
                where C: AsRef<Path>, K: AsRef<Path>
        {
            let mut ctx = try!(SslContext::new(SslMethod::Sslv23));
            try!(ctx.set_cipher_list("DEFAULT"));
            try!(ctx.set_certificate_file(cert.as_ref(), X509FileType::PEM));
            try!(ctx.set_private_key_file(key.as_ref(), X509FileType::PEM));
            ctx.set_verify(SSL_VERIFY_NONE, None);
            Ok(Openssl { context: Arc::new(ctx) })
        }
    }

    impl super::Ssl for Openssl {
        type Stream = SslStream<HttpStream>;

        fn wrap_client(&self, stream: HttpStream, host: &str) -> hyper::Result<Self::Stream> {
            //if let Some(ref verifier) = self.verifier {
            //    verifier(&mut context);
            //}
            let ssl = try!(Ssl::new(&self.context));
            try!(ssl.set_hostname(host));
            SslStream::new_from(ssl, stream).map_err(From::from)
        }

        fn wrap_server(&self, stream: HttpStream) -> hyper::Result<Self::Stream> {
            match SslStream::new_server(&self.context, stream) {
                Ok(ssl_stream) => Ok(ssl_stream),
                Err(SslIoError(e)) => {
                    Err(io::Error::new(io::ErrorKind::ConnectionAborted, e).into())
                },
                Err(e) => Err(e.into())
            }
        }
    }
}
