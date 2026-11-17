//! The TCP transport: one JSON frame per line over a non-blocking socket.
use std::io::Write;
use std::time::{Duration, Instant};

use graphlink_core::frame::{Frame, Op};
use graphlink_core::transport::Transport;
use graphlink_net::{Listener, Tcp};

fn frame(n: u64, why: &str) -> Frame { Frame { v: 1, n, ack: 0, ops: vec![Op::Bye { why: why.into() }] } }

fn recv_within(t: &mut Tcp, ms: u64) -> Option<Frame> {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if let Some(f) = t.recv().unwrap() { return Some(f); }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

fn pair() -> (Tcp, Tcp) {
    let l = Listener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let client = Tcp::connect(&addr.to_string()).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(server) = l.accept().unwrap() { return (client, server); }
        assert!(Instant::now() < end, "no connection arrived");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn frames_cross_in_both_directions_in_order() {
    let (mut c, mut s) = pair();
    c.send(&frame(1, "a")).unwrap();
    c.send(&frame(2, "b")).unwrap();
    s.send(&frame(1, "back")).unwrap();
    assert_eq!(recv_within(&mut s, 2000).unwrap(), frame(1, "a"));
    assert_eq!(recv_within(&mut s, 2000).unwrap(), frame(2, "b"));
    assert_eq!(recv_within(&mut c, 2000).unwrap(), frame(1, "back"));
}

#[test]
fn recv_never_waits_and_answers_none_when_nothing_is_there() {
    let (mut c, _s) = pair();
    let t = Instant::now();
    assert!(c.recv().unwrap().is_none());
    assert!(t.elapsed() < Duration::from_millis(200));
}

#[test]
fn a_frame_that_arrives_in_pieces_is_taken_once_whole() {
    let l = Listener::bind("127.0.0.1:0").unwrap();
    let mut raw = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let mut s = loop { if let Some(s) = l.accept().unwrap() { break s; } std::thread::sleep(Duration::from_millis(2)); };
    let line = format!("{}\n", frame(1, "whole").encode());
    let (a, b) = line.split_at(7);
    raw.write_all(a.as_bytes()).unwrap(); raw.flush().unwrap();
    assert!(recv_within(&mut s, 100).is_none(), "half a line is not a frame");
    raw.write_all(b.as_bytes()).unwrap(); raw.flush().unwrap();
    assert_eq!(recv_within(&mut s, 2000).unwrap(), frame(1, "whole"));
}

#[test]
fn a_line_that_is_not_a_frame_is_an_error_naming_what_is_wrong() {
    let l = Listener::bind("127.0.0.1:0").unwrap();
    let mut raw = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let mut s = loop { if let Some(s) = l.accept().unwrap() { break s; } std::thread::sleep(Duration::from_millis(2)); };
    raw.write_all(b"{\"v\":1}\n").unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    let err = loop {
        match s.recv() { Err(e) => break e, Ok(_) => { assert!(Instant::now() < end, "no error"); std::thread::sleep(Duration::from_millis(2)); } }
    };
    assert!(err.contains("`n`") || err.contains("ops"), "{err}");
}

#[test]
fn a_closed_peer_is_noticed() {
    let (c, mut s) = pair();
    drop(c);
    let end = Instant::now() + Duration::from_secs(2);
    while !s.is_closed() {
        let _ = s.recv();
        assert!(Instant::now() < end, "the close was never noticed");
        std::thread::sleep(Duration::from_millis(2));
    }
}
