//! The link state machine and the transport trait: two links over an in-memory pipe (docs/frames.md, "A link").
use graphlink_core::frame::{Frame, Op, Target};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::{pipe, Transport};

fn me(ns: &str, support: &[u32]) -> Me { Me { ns: ns.into(), build: "t".into(), epoch: 1.0, support: support.to_vec() } }

/// move everything a link has to say through the transport, and what arrives back into the link; returns the events it produced
fn pump<T: Transport>(link: &mut Link, t: &mut T) -> Vec<Event> {
    let mut events = vec![];
    while let Some(f) = t.recv().unwrap() {
        let (ev, reply) = link.receive(f);
        events.extend(ev);
        if let Some(r) = reply { t.send(&r).unwrap(); }
    }
    events
}

fn open_pair(a_support: &[u32], b_support: &[u32]) -> (Link, Link, impl Transport, impl Transport) {
    let (mut ta, mut tb) = pipe();
    let (mut a, mut b) = (Link::new(me("seat", a_support)), Link::new(me("kitchen", b_support)));
    ta.send(&a.open()).unwrap();
    tb.send(&b.open()).unwrap();
    (a, b, ta, tb)
}

#[test]
fn two_links_open_with_a_hello_each_and_agree_on_the_highest_shared_version() {
    let (mut a, mut b, mut ta, mut tb) = open_pair(&[1, 2], &[2, 3]);
    let ea = pump(&mut a, &mut ta);
    let eb = pump(&mut b, &mut tb);
    assert!(matches!(&ea[..], [Event::Opened { peer_ns, version: 2, .. }] if peer_ns == "kitchen"), "{ea:?}");
    assert!(matches!(&eb[..], [Event::Opened { peer_ns, version: 2, .. }] if peer_ns == "seat"), "{eb:?}");
    assert!(a.is_open() && b.is_open());
}

#[test]
fn with_no_version_in_common_both_sides_fail_and_nothing_opens() {
    let (mut a, mut b, mut ta, mut tb) = open_pair(&[1], &[2]);
    let ea = pump(&mut a, &mut ta);
    assert!(matches!(&ea[..], [Event::Failed { .. }]), "{ea:?}");
    let eb = pump(&mut b, &mut tb);
    assert!(matches!(&eb[..], [Event::Failed { .. }]), "{eb:?}");
    // the failed op each side answered arrives at the other and is not a second event for a closed link
    assert!(!a.is_open() && !b.is_open());
    assert!(pump(&mut a, &mut ta).is_empty());
}

#[test]
fn ops_sent_after_open_arrive_in_order_numbered_and_acknowledged() {
    let (mut a, mut b, mut ta, mut tb) = open_pair(&[1], &[1]);
    pump(&mut a, &mut ta); pump(&mut b, &mut tb);
    let f1 = a.send(vec![Op::Sub { target: Target::Id("kitchen::Lamp:desk".into()), props: None, limit: None }]).unwrap();
    let f2 = a.send(vec![Op::Unsub { target: Target::Id("kitchen::Lamp:desk".into()) }]).unwrap();
    assert_eq!((f1.n, f2.n), (2, 3), "the hello was frame 1");
    ta.send(&f1).unwrap(); ta.send(&f2).unwrap();
    let eb = pump(&mut b, &mut tb);
    assert!(matches!(&eb[0], Event::Op(Op::Sub { .. })), "{eb:?}");
    assert!(matches!(&eb[1], Event::Op(Op::Unsub { .. })));
    let reply = b.send(vec![]).unwrap();
    assert_eq!(reply.ack, 3, "b has taken everything a sent");
}

#[test]
fn a_frame_taken_twice_gives_its_ops_once() {
    let (mut a, mut b, mut ta, mut tb) = open_pair(&[1], &[1]);
    pump(&mut a, &mut ta); pump(&mut b, &mut tb);
    let f = a.send(vec![Op::Unsub { target: Target::Id("e".into()) }]).unwrap();
    ta.send(&f).unwrap(); ta.send(&f).unwrap();
    let eb = pump(&mut b, &mut tb);
    assert_eq!(eb.iter().filter(|e| matches!(e, Event::Op(_))).count(), 1, "{eb:?}");
    assert!(eb.contains(&Event::Duplicate { n: f.n }));
}

#[test]
fn a_frame_that_skips_one_is_reported_as_a_gap_and_its_ops_are_not_delivered() {
    let (mut a, mut b, mut ta, mut tb) = open_pair(&[1], &[1]);
    pump(&mut a, &mut ta); pump(&mut b, &mut tb);
    let _lost = a.send(vec![Op::Unsub { target: Target::Id("lost".into()) }]).unwrap();
    let f = a.send(vec![Op::Unsub { target: Target::Id("after".into()) }]).unwrap();
    ta.send(&f).unwrap();
    let eb = pump(&mut b, &mut tb);
    assert_eq!(eb, vec![Event::Gap { expected: 2 }]);
}

#[test]
fn nothing_can_be_sent_before_the_link_is_open() {
    let mut l = Link::new(me("seat", &[1]));
    assert!(l.send(vec![]).is_err());
    let hello = l.open();
    assert_eq!(hello.n, 1);
    assert!(matches!(&hello.ops[..], [Op::Hello { support, .. }] if support == &vec![1]));
    assert!(l.send(vec![]).is_err(), "still not open: the peer has not answered");
}

#[test]
fn a_bye_from_the_peer_closes_the_link() {
    let (mut a, mut b, mut ta, mut tb) = open_pair(&[1], &[1]);
    pump(&mut a, &mut ta); pump(&mut b, &mut tb);
    let f = a.send(vec![Op::Bye { why: "done".into() }]).unwrap();
    ta.send(&f).unwrap();
    let eb = pump(&mut b, &mut tb);
    assert!(eb.contains(&Event::Closed { why: "done".into() }), "{eb:?}");
    assert!(!b.is_open());
}

#[test]
fn a_frame_in_a_version_not_negotiated_is_refused() {
    let (mut a, mut b, mut ta, mut tb) = open_pair(&[1], &[1]);
    pump(&mut a, &mut ta); pump(&mut b, &mut tb);
    let bad = Frame { v: 7, n: 2, ack: 0, ops: vec![Op::Bye { why: "x".into() }] };
    ta.send(&bad).unwrap();
    let eb = pump(&mut b, &mut tb);
    assert!(matches!(&eb[..], [Event::Refused { .. }]), "{eb:?}");
}
