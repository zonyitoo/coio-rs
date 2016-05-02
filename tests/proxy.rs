#[macro_use]
extern crate log;
extern crate env_logger;
extern crate mio;

extern crate coio;

use std::sync::Arc;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::sync::atomic::{AtomicBool, Ordering};

use coio::Scheduler;
use coio::net::tcp::{TcpListener, TcpStream};

const LOCAL_ADDR: &'static str = "127.0.0.1:3020";
const REMOTE_ADDR: &'static str = "127.0.0.1:3010";

#[test]
fn proxy_shared_tcp_stream() {
    env_logger::init().unwrap();

    Scheduler::new()
        .with_workers(2)
        .run(|| {
            let guard = Arc::new(AtomicBool::new(false));
            let cloned_guard = guard.clone();
            let remote = Scheduler::spawn(move || {
                println!("REMOTE started");

                let listener = TcpListener::bind(REMOTE_ADDR).unwrap();

                cloned_guard.store(true, Ordering::Release);

                let (mut stream, _) = listener.accept().unwrap();

                // Echo
                let mut buf = [0u8; 1024];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => {
                            trace!("REMOTE: read EOF");
                            break;
                        }
                        Ok(len) => {
                            stream.write_all(&buf[..len]).unwrap();
                            trace!("REMOTE: write {}", len);
                        }
                        Err(err) => {
                            panic!("Failed to read {:?}", err);
                        }
                    }
                }

                println!("REMOTE finished");
            });

            while !guard.load(Ordering::Acquire) {
                // NOTE: Ensure REMOTE is started
                Scheduler::sched();
            }

            let guard = Arc::new(AtomicBool::new(false));
            let cloned_guard = guard.clone();
            let proxy = Scheduler::spawn(move || {
                println!("PROXY started");

                let listener = TcpListener::bind(LOCAL_ADDR).unwrap();

                cloned_guard.store(true, Ordering::Release);

                let (stream, _) = listener.accept().unwrap();
                let stream = Arc::new(stream);

                let remote = TcpStream::connect(REMOTE_ADDR).unwrap();
                let remote = Arc::new(remote);

                let cloned_stream = stream.clone();
                let cloned_remote = remote.clone();
                Scheduler::spawn(move || {
                    let mut buf = [0u8; 1024];

                    loop {
                        match (&*stream).read(&mut buf) {
                            Ok(0) => {
                                trace!("LOCAL -> REMOTE: read EOF");
                                break;
                            }
                            Ok(len) => {
                                (&*remote).write_all(&buf[..len]).unwrap();
                                trace!("LOCAL -> REMOTE: read {} and -> write", len);
                            }
                            Err(err) => {
                                panic!("Failed to read: {:?}", err);
                            }
                        }
                    }

                    let _ = stream.shutdown(Shutdown::Read);
                    let _ = remote.shutdown(Shutdown::Write);

                    println!("LOCAL -> REMOTE finished");
                });

                Scheduler::spawn(move || {
                    let mut buf = [0u8; 1024];

                    loop {
                        match (&*cloned_remote).read(&mut buf) {
                            Ok(0) => {
                                trace!("REMOTE -> LOCAL: read EOF");
                                break;
                            }
                            Ok(len) => {
                                (&*cloned_stream).write_all(&buf[..len]).unwrap();
                                trace!("REMOTE -> LOCAL: read {} and -> write", len);
                            }
                            Err(err) => {
                                panic!("Failed to read: {:?}", err);
                            }
                        }
                    }

                    let _ = cloned_remote.shutdown(Shutdown::Read);
                    let _ = cloned_stream.shutdown(Shutdown::Write);

                    println!("REMOTE -> LOCAL finished");
                });
            });

            while !guard.load(Ordering::Acquire) {
                // NOTE: Ensure REMOTE is started
                Scheduler::sched();
            }

            let local = Scheduler::spawn(|| {
                println!("LOCAL started");

                const TOTAL_BYTES: usize = 100 * 1024 * 1024; // 100M
                const DATA: [u8; 4096] = [0u8; 4096];

                let stream = TcpStream::connect(LOCAL_ADDR).unwrap();
                let stream = Arc::new(stream);
                let cloned_stream = stream.clone();

                let reader = Scheduler::spawn(move || {
                    let mut buf = [0u8; 1024];

                    let mut total_recv = 0;
                    while total_recv < TOTAL_BYTES {
                        match (&*stream).read(&mut buf).unwrap() {
                            0 => break,
                            len => {
                                trace!("LOCAL: read {}", len);
                                total_recv += len;
                            }
                        }
                    }
                });

                let writer = Scheduler::spawn(move || {
                    let mut total_sent = 0;
                    while total_sent < TOTAL_BYTES {
                        trace!("LOCAL: write {}", DATA.len());

                        (&*cloned_stream).write_all(&DATA[..]).unwrap();
                        total_sent += DATA.len();
                    }
                });

                reader.join().unwrap();
                writer.join().unwrap();

                println!("LOCAL finished");
            });

            proxy.join().unwrap();
            local.join().unwrap();
            remote.join().unwrap();
        })
        .unwrap();
}
