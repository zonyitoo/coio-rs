extern crate coio;

use std::io::{Read, Write};

use coio::Scheduler;
use coio::net::{TcpListener, TcpStream, UdpSocket};

#[cfg(unix)]
use coio::net::{UnixStream, UnixListener};

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

                    stream.write_all(&buf[..len])
                          .and_then(|_| stream.flush())
                          .unwrap();
                }
            });

            let sender_fut = Scheduler::spawn(move || {
                let mut stream = TcpStream::connect("127.0.0.1:6789").unwrap();
                stream.write_all(b"abcdefg")
                      .and_then(|_| stream.flush())
                      .unwrap();

                let mut buf = [0u8; 1024];
                let len = stream.read(&mut buf).unwrap();

                assert_eq!(&buf[..len], b"abcdefg");
            });

            listen_fut.join().unwrap();
            sender_fut.join().unwrap();
        })
        .unwrap();
}

#[test]
fn test_udp_echo() {
    Scheduler::new()
        .run(move || {
            const TEST_SLICE: &'static [u8] = b"abcdefg";
            let acceptor = UdpSocket::bind("127.0.0.1:0").unwrap();
            let acceptor_addr = acceptor.local_addr().unwrap();

            // Listener
            let listen_fut = Scheduler::spawn(move || {
                let mut buf = [0u8; 1024];
                let (len, addr) = acceptor.recv_from(&mut buf).unwrap();
                acceptor.send_to(&buf[..len], addr).unwrap();
            });

            let sender_fut = Scheduler::spawn(move || {
                let sender = UdpSocket::bind("127.0.0.1:0").unwrap();

                let mut buf = [0u8; 1024];
                sender.send_to(TEST_SLICE, acceptor_addr).unwrap();
                let (len, _) = sender.recv_from(&mut buf).unwrap();

                assert_eq!(&buf[..len], TEST_SLICE);
            });

            listen_fut.join().unwrap();
            sender_fut.join().unwrap();
        })
        .unwrap();
}

#[cfg(unix)]
#[test]
fn test_unix_socket_echo() {
    use std::fs;

    Scheduler::new()
        .run(move || {
            const FILE_PATH_STR: &'static str = "/tmp/coio-unix-socket-test.sock";

            let _ = fs::remove_file(FILE_PATH_STR);
            let acceptor = UnixListener::bind(FILE_PATH_STR).unwrap();

            // Listener
            let listen_fut = Scheduler::spawn(move || {
                let mut stream = acceptor.accept().unwrap();

                let mut buf = [0u8; 1024];
                let len = stream.read(&mut buf).unwrap();
                stream.write_all(&buf[..len])
                      .and_then(|_| stream.flush())
                      .unwrap();
            });

            let sender_fut = Scheduler::spawn(move || {
                let mut stream = UnixStream::connect(FILE_PATH_STR).unwrap();
                stream.write_all(b"abcdefg")
                      .and_then(|_| stream.flush())
                      .unwrap();

                let mut buf = [0u8; 1024];
                let len = stream.read(&mut buf).unwrap();

                assert_eq!(&buf[..len], b"abcdefg");
            });

            listen_fut.join().unwrap();
            sender_fut.join().unwrap();
        })
        .unwrap();
}
