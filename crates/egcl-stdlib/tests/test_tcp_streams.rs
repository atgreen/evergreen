//! Owned TCP streams: R5.113, R5.121–R5.124, on Unix and Windows.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;
use egcl_rt::value::{EOF, NIL, EgclVal};
use egcl_stdlib::streams::*;

#[test]
fn client_buffers_bytes_reports_readiness_and_closes_the_socket() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (send, receive) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        receive.recv_timeout(Duration::from_secs(5)).unwrap();
        peer.write_all(&[65, 200, 255]).unwrap();
        let mut ack = [0];
        peer.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [42]);
        // CLOSE must release the socket now, without waiting for a GC.
        assert_eq!(peer.read(&mut ack).unwrap(), 0);
    });
    let mut stream = socket_connect("127.0.0.1", port, Some(Duration::from_secs(2))).unwrap();
    egcl_rt::rooted_ref!(_stream = &mut stream);
    assert!(input_stream_p(stream) && output_stream_p(stream));
    assert!(!stream_wait_for_input(stream, Some(20)).unwrap());
    assert!(!stream_listen(stream).unwrap());
    assert_eq!(stream_read_char_no_hang(stream).unwrap(), NIL);
    assert_eq!(file_position(stream).unwrap(), NIL);
    assert_eq!(
        set_file_position(stream, EgclVal::from_fixnum(0)).unwrap(),
        NIL
    );
    assert_eq!(set_file_position_to_end(stream).unwrap(), NIL);
    assert_eq!(file_length_fn(stream).unwrap(), NIL);
    socket_set_read_timeout(stream, Some(Duration::from_millis(50))).unwrap();
    assert!(socket_read_timeout(stream).unwrap().is_some());
    assert!(stream_read_byte(stream).is_err());
    socket_set_read_timeout(stream, None).unwrap();
    send.send(()).unwrap();
    assert!(stream_wait_for_input(stream, Some(2000)).unwrap());
    assert!(stream_listen(stream).unwrap());
    assert_eq!(stream_read_byte(stream).unwrap().as_fixnum(), 65);
    assert!(stream_wait_for_input(stream, Some(0)).unwrap());
    assert!(stream_listen(stream).unwrap());
    assert_eq!(stream_read_byte(stream).unwrap().as_fixnum(), 200);
    assert_eq!(stream_read_byte(stream).unwrap().as_fixnum(), 255);
    stream_write_byte(stream, EgclVal::from_fixnum(42)).unwrap();
    close(stream, false).unwrap();
    assert!(!open_stream_p(stream));
    assert!(stream_read_byte(stream).is_err());
    assert!(stream_wait_for_input(stream, Some(0)).is_err());
    assert!(socket_read_timeout(stream).is_err());
    close(stream, false).unwrap();
    peer.join().unwrap();
}

#[test]
fn accepted_socket_reports_eof_and_listener_closes() {
    let listener = socket_listen("127.0.0.1", 0, 4).unwrap();
    let port = socket_local_port(listener).unwrap();
    let peer = std::thread::spawn(move || {
        let mut peer = TcpStream::connect(("127.0.0.1", port)).unwrap();
        peer.write_all(&[123]).unwrap();
    });
    let mut stream = socket_accept(listener).unwrap();
    egcl_rt::rooted_ref!(_stream = &mut stream);
    socket_close_listener(listener);
    assert!(socket_accept(listener).is_err());
    peer.join().unwrap();
    assert!(stream_wait_for_input(stream, Some(2000)).unwrap());
    assert_eq!(stream_read_byte(stream).unwrap().as_fixnum(), 123);
    assert!(stream_wait_for_input(stream, Some(2000)).unwrap());
    assert!(!stream_listen(stream).unwrap());
    assert_eq!(stream_read_byte(stream).unwrap(), EOF);
    close(stream, false).unwrap();
}

#[test]
fn no_hang_preserves_partial_utf8_until_the_remaining_byte_arrives() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (send, receive) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.write_all(&[0xc3]).unwrap();
        if receive.recv_timeout(Duration::from_secs(2)).is_ok() {
            peer.write_all(&[0xa9]).unwrap();
        }
    });
    let mut stream = socket_connect("127.0.0.1", port, None).unwrap();
    egcl_rt::rooted_ref!(_stream = &mut stream);
    socket_set_read_timeout(stream, Some(Duration::from_millis(100))).unwrap();
    assert!(stream_wait_for_input(stream, Some(2000)).unwrap());
    assert_eq!(stream_read_char_no_hang(stream).unwrap(), NIL);
    // The prefix is now buffered; retrying must still return promptly.
    assert_eq!(stream_read_char_no_hang(stream).unwrap(), NIL);
    send.send(()).unwrap();
    peer.join().unwrap();
    assert_eq!(stream_read_char_no_hang(stream).unwrap().as_char(), 'é');
    assert_eq!(stream_read_char_no_hang(stream).unwrap(), EOF);
    close(stream, false).unwrap();
}
