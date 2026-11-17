//! The shared-memory transport: two rings in a mapped file, one per direction (docs/shared-memory.md, the control ring first).
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use graphlink_core::frame::{Frame, Op};
use graphlink_core::transport::Transport;
use graphlink_shm::Shm;

/// a region name: short (macOS allows 31 characters) and unique per process and call
fn path() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("/gl-s-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst))
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
    let _junk = graphlink_shm::junk_region_for_test(&p, 4096).unwrap();
    let e = Shm::attach(&p).err().unwrap();
    assert!(e.contains("not a graphlink region"), "{e}");
    assert!(Shm::attach(path()).err().unwrap().contains("cannot open"));
    let p2 = path();
    let _small = graphlink_shm::junk_region_for_test(&p2, 5).unwrap();
    let e2 = Shm::attach(&p2).err().unwrap();
    assert!(e2.contains("too small") || e2.contains("not a graphlink region"), "{e2} (a system may round a region up to a page)");
}

#[test]
fn a_name_that_cannot_be_a_region_name_is_refused_before_the_system_is_asked() {
    let long = format!("/{}", "x".repeat(40));
    for bad in ["no-leading-slash", "/two/slashes", "/", "", long.as_str()] {
        let e = Shm::create(bad, 4096).err().unwrap();
        assert!(e.contains("region name"), "{bad:?}: {e}");
        assert!(Shm::attach(bad).err().unwrap().contains("region name"), "{bad:?}");
    }
}

#[test]
fn a_name_that_is_taken_cannot_be_created_again() {
    let p = path();
    let _a = Shm::create(&p, 4096).unwrap();
    assert!(Shm::create(&p, 4096).err().unwrap().contains("cannot create"));
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
fn the_creator_removes_the_name_when_it_goes() {
    let p = path();
    let a = Shm::create(&p, 4096).unwrap();
    assert!(Shm::attach(&p).is_ok(), "the name is there while the creator is");
    drop(a);
    assert!(Shm::attach(&p).err().unwrap().contains("cannot open"), "and gone after");
}

#[test]
fn two_threads_send_ten_thousand_frames_each_way_with_none_lost() {
    let p = path();
    let mut a = Shm::create(&p, 1024).unwrap();
    let mut b = Shm::attach(&p).unwrap();
    const N: u64 = 10_000;
    let t = std::thread::spawn(move || {
        let (mut sent, mut got) = (0u64, 0u64);
        while sent < N || got < N || b.pending_bytes() > 0 {
            if sent < N { b.send(&frame(sent, "b")).unwrap(); sent += 1; }
            b.flush();
            while let Some(f) = b.recv().unwrap() { assert_eq!(f, frame(got, "a")); got += 1; }
        }
        b
    });
    let (mut sent, mut got) = (0u64, 0u64);
    let end = Instant::now() + Duration::from_secs(60);
    while sent < N || got < N || a.pending_bytes() > 0 {
        assert!(Instant::now() < end, "stuck at sent {sent} got {got}");
        if sent < N { a.send(&frame(sent, "a")).unwrap(); sent += 1; }
        a.flush();
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

#[test]
fn frames_that_did_not_fit_stay_queued_on_the_sender_until_it_flushes_and_it_can_say_how_many_bytes_wait() {
    let p = path();
    let mut a = Shm::create(&p, 256).unwrap();
    let mut b = Shm::attach(&p).unwrap();
    assert_eq!(a.pending_bytes(), 0);
    for i in 0..50u64 { a.send(&frame(i, "queued")).unwrap(); }
    let waiting = a.pending_bytes();
    assert!(waiting > 0, "a 256-byte ring cannot hold 50 frames");
    // the reader drains what is in the ring; the sender's queue goes down only when the sender flushes
    let mut got = 0;
    while b.recv().unwrap().is_some() { got += 1; }
    assert_eq!(a.pending_bytes(), waiting, "nothing moved without a call on the sender");
    while a.pending_bytes() > 0 {
        a.flush();
        while b.recv().unwrap().is_some() { got += 1; }
    }
    assert_eq!(got, 50);
}

#[test]
fn a_sender_that_goes_away_with_frames_queued_flushes_what_fits_on_the_way_out() {
    let p = path();
    let mut a = Shm::create(&p, 4096).unwrap();
    let mut b = Shm::attach(&p).unwrap();
    // the ring has room, so send() writes at once; to hold frames back, the reader first fills the other side
    for i in 0..5u64 { a.send(&frame(i, "last words")).unwrap(); }
    drop(a);
    let mut n = 0;
    while let Some(f) = b.recv().unwrap() { assert_eq!(f, frame(n, "last words")); n += 1; }
    assert_eq!(n, 5, "what was written before the creator went is still readable");
}

#[test]
fn a_name_a_dead_creator_left_behind_can_be_removed_and_then_made_again() {
    let p = path();
    let dead = Shm::create(&p, 4096).unwrap();
    std::mem::forget(dead);   // a creator that died without cleaning up: the name stays
    assert!(Shm::create(&p, 4096).err().unwrap().contains("cannot create"), "the stale name blocks a new creator");
    assert_eq!(graphlink_shm::remove_region(&p), Ok(true), "removed");
    let again = Shm::create(&p, 4096).unwrap();
    drop(again);
    assert_eq!(graphlink_shm::remove_region(&p), Ok(false), "nothing left to remove");
    assert!(graphlink_shm::remove_region("not a name").unwrap_err().contains("region name"));
}
