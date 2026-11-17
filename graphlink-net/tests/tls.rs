//! TLS for the TCP transport: the server's certificate is checked by the client (a certificate file it names), the connection is encrypted, and a plain or mistrusting client gets no frame through.
#![cfg(feature = "tls")]
use std::time::{Duration, Instant};

use graphlink_core::frame::{Frame, Op, VERSION};
use graphlink_core::transport::Transport;
use graphlink_net::tls::{make_certs, TlsClient, TlsServer};
use graphlink_net::{Listener, Tcp};

fn dir(tag: &str) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!("graphlink-tls-{tag}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn sans() -> Vec<String> { vec!["localhost".into(), "127.0.0.1".into()] }
fn hello(ns: &str) -> Frame { Frame { v: 1, n: 1, ack: 0, ops: vec![Op::Hello { ns: ns.into(), build: "t".into(), epoch: 1.0, have: Default::default(), support: vec![VERSION], token: None }] } }

fn servers(d: &std::path::Path) -> (Listener, String) {
    let server = TlsServer::from_files(&d.join("cert.pem"), &d.join("key.pem")).unwrap();
    let l = Listener::bind("127.0.0.1:0").unwrap().with_tls(server);
    let addr = l.local_addr().unwrap().to_string();
    (l, addr)
}
fn accept(l: &Listener) -> Tcp { let end = Instant::now() + Duration::from_secs(5); loop { if let Some(t) = l.accept().unwrap() { return t; } assert!(Instant::now() < end, "nobody connected"); std::thread::sleep(Duration::from_millis(5)); } }
/// poll both ends until `t` gets a frame (or `ms` pass)
fn get(t: &mut Tcp, other: &mut Tcp, ms: u64) -> Option<Frame> {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if let Ok(Some(f)) = t.recv() { return Some(f); }
        let _ = other.recv();   // drive the other end's handshake and writes
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

#[test]
fn certs_are_made_once_the_key_is_private_and_a_second_call_keeps_them() {
    use std::os::unix::fs::PermissionsExt;
    let d = dir("make");
    make_certs(&d, &sans()).unwrap();
    for f in ["cert.pem", "key.pem"] { assert!(d.join(f).is_file(), "{f}"); }
    assert_eq!(std::fs::metadata(d.join("key.pem")).unwrap().permissions().mode() & 0o777, 0o600);
    let before = std::fs::read(d.join("cert.pem")).unwrap();
    make_certs(&d, &sans()).unwrap();
    assert_eq!(std::fs::read(d.join("cert.pem")).unwrap(), before, "a second call does not replace what is there");
}

#[test]
fn a_frame_crosses_a_tls_connection_both_ways_when_the_client_trusts_the_certificate() {
    let d = dir("ok");
    make_certs(&d, &sans()).unwrap();
    let (l, addr) = servers(&d);
    let client = TlsClient::trusting(&d.join("cert.pem")).unwrap();
    let mut c = Tcp::connect_tls(addr.as_str(), "localhost", &client).unwrap();
    let mut s = accept(&l);
    c.send(&hello("client")).unwrap();
    let got = get(&mut s, &mut c, 5000).expect("the server never got the client's frame");
    assert_eq!(got, hello("client"));
    s.send(&hello("server")).unwrap();
    assert_eq!(get(&mut c, &mut s, 5000).expect("the client never got the server's frame"), hello("server"));
}

#[test]
fn a_client_that_does_not_trust_the_servers_certificate_gets_nothing_through() {
    let (d, other) = (dir("mine"), dir("other"));
    make_certs(&d, &sans()).unwrap();
    make_certs(&other, &sans()).unwrap();
    let (l, addr) = servers(&d);
    let client = TlsClient::trusting(&other.join("cert.pem")).unwrap();   // another certificate
    let mut c = Tcp::connect_tls(addr.as_str(), "localhost", &client).unwrap();
    let mut s = accept(&l);
    let _ = c.send(&hello("client"));
    assert!(get(&mut s, &mut c, 1500).is_none(), "a frame crossed an untrusted connection");
    assert!(c.is_closed() || s.is_closed(), "the handshake should have failed");
}

#[test]
fn a_name_the_certificate_does_not_cover_is_refused() {
    let d = dir("name");
    make_certs(&d, &sans()).unwrap();
    let (l, addr) = servers(&d);
    let client = TlsClient::trusting(&d.join("cert.pem")).unwrap();
    let mut c = Tcp::connect_tls(addr.as_str(), "elsewhere.example", &client).unwrap();
    let mut s = accept(&l);
    let _ = c.send(&hello("client"));
    assert!(get(&mut s, &mut c, 1500).is_none());
}

#[test]
fn a_plain_client_gets_no_frame_through_a_tls_listener() {
    let d = dir("plain");
    make_certs(&d, &sans()).unwrap();
    let (l, addr) = servers(&d);
    let mut c = Tcp::connect(addr.as_str()).unwrap();
    let mut s = accept(&l);
    let _ = c.send(&hello("client"));
    assert!(get(&mut s, &mut c, 1500).is_none());
}
