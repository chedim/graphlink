//! The frame counters: `n` and `ack` (docs/frames.md, "Ordering").
use graphlink_core::frame::{Frame, Op, VERSION};
use graphlink_core::order::{Inbox, Outbox, Take};

#[test]
fn frames_taken_in_order_are_accepted_and_a_repeat_is_a_duplicate() {
    let mut i = Inbox::default();
    assert_eq!(i.ack(), 0);
    for n in 1..=3 { assert_eq!(i.take(n), Take::Accept, "{n}"); }
    assert_eq!(i.ack(), 3);
    assert_eq!(i.take(2), Take::Duplicate, "a retried frame is dropped");
    assert_eq!(i.take(3), Take::Duplicate);
    assert_eq!(i.ack(), 3, "a duplicate changes nothing");
}

#[test]
fn a_frame_that_skips_one_is_a_gap_that_names_the_one_expected_and_is_not_taken() {
    let mut i = Inbox::default();
    i.take(1);
    assert_eq!(i.take(3), Take::Gap { expected: 2 });
    assert_eq!(i.ack(), 1, "a gap changes nothing");
    assert_eq!(i.take(2), Take::Accept, "the missing one still fits");
    assert_eq!(i.take(3), Take::Accept);
}

#[test]
fn counting_starts_at_one_so_zero_is_never_a_frame() {
    let mut i = Inbox::default();
    assert_eq!(i.take(0), Take::Duplicate);
    assert_eq!(i.take(2), Take::Gap { expected: 1 });
    assert_eq!(i.take(1), Take::Accept);
}

#[test]
fn an_outbox_numbers_its_frames_from_one_and_carries_what_the_inbox_has_taken() {
    let mut inbox = Inbox::default();
    let mut out = Outbox::default();
    let a = out.frame(VERSION, &inbox, vec![Op::Bye { why: "a".into() }]);
    assert_eq!((a.n, a.ack), (1, 0));
    inbox.take(1); inbox.take(2);
    let b = out.frame(VERSION, &inbox, vec![]);
    assert_eq!((b.n, b.ack), (2, 2));
    assert_eq!(b, Frame { v: VERSION, n: 2, ack: 2, ops: vec![] });
}

#[test]
fn the_outbox_counts_how_many_frames_the_peer_has_not_acknowledged() {
    let inbox = Inbox::default();
    let mut out = Outbox::default();
    for _ in 0..3 { out.frame(VERSION, &inbox, vec![]); }
    assert_eq!(out.unacked(), 3);
    assert!(out.acked(2));
    assert_eq!(out.unacked(), 1);
    assert!(out.acked(2), "the same ack again is fine");
    assert!(!out.acked(1), "an ack that goes back is ignored");
    assert_eq!(out.unacked(), 1);
    assert!(!out.acked(9), "an ack of a frame never sent is refused");
    assert_eq!(out.unacked(), 1);
}
