extern crate coio;
extern crate env_logger;

use std::io::{Read, Write};
use std::time::Duration;

use coio::Scheduler;
use coio::net::{TcpListener, TcpStream};
use coio::sleep;

#[test]
fn test_tcp_echo() {
    env_logger::init().unwrap();

    Scheduler::new()
        .run(move || {
            let acceptor = TcpListener::bind("127.0.0.1:6789").unwrap();

            // Listener
            let listen_fut = Scheduler::spawn(move || {
                println!("LISTEN: Started");

                let (mut stream, addr) = acceptor.accept().unwrap();
                println!("LISTEN: Accepted {:?}", addr);

                let mut buf = [0u8; 1024];
                while let Ok(len) = stream.read(&mut buf) {
                    if len == 0 {
                        // EOF
                        break;
                    }

                    println!("LISTEN: Going to sleep");
                    sleep(Duration::from_secs(1));
                    break;
                }

                println!("LISTEN: Finished");
            });

            let sender_fut = Scheduler::spawn(move || {
                println!("SEND: Started");

                let mut stream = TcpStream::connect("127.0.0.1:6789").unwrap();
                stream.write_all(b"abcdefg")
                    .and_then(|_| stream.flush())
                    .unwrap();

                println!("SEND: Written");

                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));

                println!("SEND: Waiting to read");
                let mut buf = [0u8; 1024];
                assert!(stream.read(&mut buf).is_err());

                println!("SEND: Finished");
            });

            listen_fut.join().unwrap();
            sender_fut.join().unwrap();
        })
        .unwrap();
}
