//! The shared-memory transport: two rings in a mapped file, one per direction (docs/shared-memory.md, the control ring first).
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use graphlink_core::frame::{Frame, Op};
use graphlink_core::transport::Transport;
use graphlink_shm::Shm;

fn path() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!("graphlink-shm-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)))
}
fn frame(n: u64, why: &str) -> Frame { Frame { v: 1, n, ack: 0, ops: vec![Op::Bye { why: why.into() }] } }
fn recv_within(t: &mut Shm, ms: u64) -> Option<Frame> {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end { if let Some(f) = t.recv().unwrap() { return Some(f); } std::thread::sleep(Duration::from_micros(200)); }
    None
}

#[test]
fn frames_cross_in_both_directions_in_order() {
    let p = path();
    let mut a = Shm::create(&p, 4096).unwrap();
    let mut b = Shm::attach(&p).unwrap();
    a.send(&frame(1, "one")).unwrap();
    a.send(&frame(2, "two")).unwrap();
    b.send(&frame(1, "back")).unwrap();
    assert_eq!(recv_within(&mut b, 1000).unwrap(), frame(1, "one"));
    assert_eq!(recv_within(&mut b, 1000).unwrap(), frame(2, "two"));
    assert_eq!(recv_within(&mut a, 1000).unwrap(), frame(1, "back"));
}

#[test]
fn recv_never_waits_and_answers_none_when_nothing_is_there() {
    let p = path();
    let mut a = Shm::create(&p, 4096).unwrap();
    let t = Instant::now();
    assert!(a.recv().unwrap().is_none());
    assert!(t.elapsed() < Duration::from_millis(100));
}

#[test]
fn a_ring_that_wraps_around_keeps_every_frame_intact() {
    let p = path();
    let mut a = Shm::create(&p, 256).unwrap();
    let mut b = Shm::attach(&p).unwrap();
    for i in 0..2000u64 {
        a.send(&frame(i, &format!("frame number {i} with some padding to cross the end of the ring"))).unwrap();
        assert_eq!(recv_within(&mut b, 1000).unwrap(), frame(i, &format!("frame number {i} with some padding to cross the end of the ring")), "frame {i}");
    }
}

#[test]
fn a_full_ring_queues_on_the_sender_and_nothing_is_lost_or_reordered() {
    let p = path();
    let mut a = Shm::create(&p, 256).unwrap();
    let mut b = Shm::attach(&p).unwrap();
    for i in 0..100u64 { a.send(&frame(i, "x")).unwrap(); }   // far more than 256 bytes hold
    for i in 0..100u64 {
        let f = loop { a.recv().unwrap(); if let Some(f) = b.recv().unwrap() { break f; } std::thread::sleep(Duration::from_micros(100)); };   // a.recv() flushes what a still holds
        assert_eq!(f, frame(i, "x"), "frame {i}");
    }
}

#[test]
fn a_frame_bigger_than_the_ring_is_an_error_naming_the_size() {
    let p = path();
    let mut a = Shm::create(&p, 128).unwrap();
    let e = a.send(&frame(1, &"y".repeat(500))).unwrap_err();
    assert!(e.contains("larger than the ring"), "{e}");
}

#[test]
fn attaching_to_something_that_is_not_a_region_is_an_error_that_says_what() {
    let p = path();
    std::fs::write(&p, vec![0u8; 4096]).unwrap();
    let e = Shm::attach(&p).err().unwrap();
    assert!(e.contains("not a graphlink region"), "{e}");
    assert!(Shm::attach(path()).err().unwrap().contains("cannot open"));
    let p2 = path();
    std::fs::write(&p2, b"short").unwrap();
    assert!(Shm::attach(&p2).err().unwrap().contains("too small"));
}

#[test]
fn a_peer_that_goes_away_is_noticed() {
    let p = path();
    let a = Shm::create(&p, 4096).unwrap();
    let b = Shm::attach(&p).unwrap();
    assert!(!a.is_closed() && !b.is_closed());
    drop(b);
    assert!(a.is_closed(), "the creator sees the attacher leave");
    let p2 = path();
    let a2 = Shm::create(&p2, 4096).unwrap();
    let b2 = Shm::attach(&p2).unwrap();
    drop(a2);
    assert!(b2.is_closed(), "the attacher sees the creator leave");
}

#[test]
fn the_creator_removes_the_file_when_it_goes() {
    let p = path();
    let a = Shm::create(&p, 4096).unwrap();
    assert!(p.exists());
    drop(a);
    assert!(!p.exists());
}

#[test]
fn two_threads_send_ten_thousand_frames_each_way_with_none_lost() {
    let p = path();
    let mut a = Shm::create(&p, 1024).unwrap();
    let mut b = Shm::attach(&p).unwrap();
    const N: u64 = 10_000;
    let t = std::thread::spawn(move || {
        let (mut sent, mut got) = (0u64, 0u64);
        while sent < N || got < N {
            if sent < N { b.send(&frame(sent, "b")).unwrap(); sent += 1; }
            while let Some(f) = b.recv().unwrap() { assert_eq!(f, frame(got, "a")); got += 1; }
        }
        b
    });
    let (mut sent, mut got) = (0u64, 0u64);
    let end = Instant::now() + Duration::from_secs(60);
    while sent < N || got < N {
        assert!(Instant::now() < end, "stuck at sent {sent} got {got}");
        if sent < N { a.send(&frame(sent, "a")).unwrap(); sent += 1; }
        while let Some(f) = a.recv().unwrap() { assert_eq!(f, frame(got, "b")); got += 1; }
    }
    let _b = t.join().unwrap();
}

/// the child process of the next test: it attaches to the region named in the environment and echoes every frame back until the creator leaves
#[test]
fn child_echo() {
    let Ok(p) = std::env::var("GRAPHLINK_SHM_CHILD") else { return };
    let mut s = loop { match Shm::attach(&p) { Ok(s) => break s, Err(_) => std::thread::sleep(Duration::from_millis(5)) } };
    while !s.is_closed() {
        while let Some(f) = s.recv().unwrap() { s.send(&f).unwrap(); }
        std::thread::sleep(Duration::from_micros(200));
    }
}

#[test]
fn another_process_attaches_and_frames_cross_between_the_two_processes() {
    let p = path();
    let mut a = Shm::create(&p, 4096).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["child_echo", "--exact", "--nocapture", "--test-threads=1"]).env("GRAPHLINK_SHM_CHILD", &p).stdout(std::process::Stdio::null()).spawn().unwrap();
    for i in 0..50u64 { a.send(&frame(i, "across")).unwrap(); }
    for i in 0..50u64 { assert_eq!(recv_within(&mut a, 10_000).unwrap_or_else(|| panic!("no echo for {i}")), frame(i, "across")); }
    drop(a);   // the creator leaves: the child sees it and ends
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end { if child.try_wait().unwrap().is_some() { return; } std::thread::sleep(Duration::from_millis(10)); }
    let _ = child.kill();
    panic!("the child did not end when the creator left");
}
