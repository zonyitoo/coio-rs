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

use std::ops::{Deref, DerefMut};

use hyper::client;
use hyper::net::{NetworkConnector, NetworkStream};
use hyper::http::Protocol;
use hyper::http::h1::Http11Protocol;

use net::http::conn::DefaultConnector;

pub struct Client(client::Client);

impl Client {
    pub fn new() -> Client {
        Client::with_connector(DefaultConnector::default())
    }

    /// Create a new client with a specific connector.
    pub fn with_connector<C, S>(connector: C) -> Client
        where C: NetworkConnector<Stream=S> + Send + Sync + 'static,
              S: NetworkStream + Send
    {
        Client::with_protocol(Http11Protocol::with_connector(connector))
    }

    /// Create a new client with a specific `Protocol`.
    pub fn with_protocol<P: Protocol + Send + Sync + 'static>(protocol: P) -> Client {
        Client(client::Client::with_protocol(protocol))
    }
}

impl Deref for Client {
    type Target = client::Client;

    fn deref(&self) -> &client::Client {
        &self.0
    }
}

impl DerefMut for Client {
    fn deref_mut(&mut self) -> &mut client::Client {
        &mut self.0
    }
}
