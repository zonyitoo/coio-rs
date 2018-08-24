// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate coio;

use coio::promise::Promise;
use coio::Scheduler;

fn main() {
    Scheduler::new()
        .with_workers(4)
        .run(|| {
            coio::spawn(|| {
                let r = Promise::spawn(|| if true { Ok(1.23) } else { Err("Final error") })
                    .then(
                        |res| {
                            assert_eq!(res, 1.23);
                            Ok(34)
                        },
                        |err| {
                            assert_eq!(err, "Final error");
                            if true {
                                Ok(35)
                            } else {
                                Err(44u64)
                            }
                        },
                    ).sync();

                assert_eq!(r, Ok(34));
            });
        }).unwrap();
}
