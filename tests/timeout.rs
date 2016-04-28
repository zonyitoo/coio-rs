extern crate coio;

use std::io::{Read, Write};
use std::time::Duration;

use coio::Scheduler;
use coio::net::{TcpListener, TcpStream};
use coio::sleep;

#[test]
fn test_tcp_echo() {
    Scheduler::new()
        .run(move || {
            let acceptor = TcpListener::bind("127.0.0.1:6789").unwrap();

            // Listener
            let listen_fut = Scheduler::spawn(move || {
                let (mut stream, _) = acceptor.accept().unwrap();

                let mut buf = [0u8; 1024];
                while let Ok(len) = stream.read(&mut buf) {
                    if len == 0 {
                        // EOF
                        break;
                    }

                    sleep(Duration::from_secs(1));
                    break;
                }
            });

            let sender_fut = Scheduler::spawn(move || {
                let mut stream = TcpStream::connect("127.0.0.1:6789").unwrap();
                stream.write_all(b"abcdefg")
                      .and_then(|_| stream.flush())
                      .unwrap();

                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));

                let mut buf = [0u8; 1024];
                assert!(stream.read(&mut buf).is_err());
            });

            listen_fut.join().unwrap();
            sender_fut.join().unwrap();
        })
        .unwrap();
}
