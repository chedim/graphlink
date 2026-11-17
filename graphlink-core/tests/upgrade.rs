//! The upgrade: two peers that began on one transport move to another (shared memory) or to another encoding (CBOR) without losing their place (docs/frames.md, "Upgrade").
use graphlink_core::frame::{Frame, Op, Proposal, Target, VERSION};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::{pipe, Transport};
use serde_json::json;

fn me(ns: &str, initiator: bool) -> Me { Me { ns: ns.into(), build: "t".into(), epoch: 1.0, support: vec![VERSION], initiator } }

fn shm() -> Proposal { Proposal::Shm { name: "/gl-kitchen-ab12".into(), ring_bytes: 4096, slots: 64, key_cap: 64, val_cap: 256 } }

fn open_pair() -> (Link, Link, impl Transport, impl Transport) {
    let (mut ta, mut tb) = pipe();
    let (mut a, mut b) = (Link::new(me("seat", true)), Link::new(me("kitchen", false)));
    ta.send(&a.open()).unwrap(); tb.send(&b.open()).unwrap();
    let pump = |l: &mut Link, t: &mut dyn FnMut() -> Option<Frame>| { while let Some(f) = t() { l.receive(f); } };
    pump(&mut a, &mut || ta.recv().unwrap()); pump(&mut b, &mut || tb.recv().unwrap());
    (a, b, ta, tb)
}

fn deliver(from: &mut impl Transport, f: Frame, to: &mut Link, t_to: &mut impl Transport) -> Vec<Event> {
    from.send(&f).unwrap();
    let mut ev = vec![];
    while let Some(g) = t_to.recv().unwrap() { let (e, reply) = to.receive(g); ev.extend(e); if let Some(r) = reply { from.send(&r).ok(); } }
    ev
}

#[test]
fn the_upgrade_ops_have_the_wire_shape_of_the_spec_and_survive_a_roundtrip() {
    let f = Frame { v: 1, n: 5, ack: 4, ops: vec![Op::Upgrade(shm()), Op::Upgrade(Proposal::Encoding("cbor".into())), Op::Upgraded(shm()), Op::Upgraded(Proposal::Encoding("cbor".into())), Op::UpgradeRefused { why: "no such region".into() }] };
    assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
    assert_eq!(f.encode()["ops"], json!([
        {"op": "upgrade", "transport": "shm", "name": "/gl-kitchen-ab12", "ring_bytes": 4096, "slots": 64, "key_cap": 64, "val_cap": 256},
        {"op": "upgrade", "encoding": "cbor"},
        {"op": "upgraded", "transport": "shm", "name": "/gl-kitchen-ab12", "ring_bytes": 4096, "slots": 64, "key_cap": 64, "val_cap": 256},
        {"op": "upgraded", "encoding": "cbor"},
        {"op": "upgrade_refused", "why": "no such region"},
    ]));
}

#[test]
fn an_upgrade_names_a_transport_or_an_encoding_never_both_and_never_neither() {
    let bad = |op: serde_json::Value| Frame::decode(&json!({"v": 1, "n": 1, "ack": 0, "ops": [op]})).is_err();
    assert!(bad(json!({"op": "upgrade"})));
    assert!(bad(json!({"op": "upgrade", "transport": "shm", "encoding": "cbor"})));
    assert!(bad(json!({"op": "upgrade", "transport": "shm", "name": "/x"})), "a shm proposal needs its capacities");
    assert!(bad(json!({"op": "upgrade", "transport": "carrier-pigeon", "name": "/x", "ring_bytes": 1, "slots": 1, "key_cap": 1, "val_cap": 1})), "a transport this library does not know");
    assert!(bad(json!({"op": "upgrade", "encoding": "xml"})));
}

#[test]
fn a_proposal_the_peer_accepts_ends_with_both_links_told_and_the_counters_going_on() {
    let (mut a, mut b, mut ta, mut tb) = open_pair();
    let f = a.propose(shm()).unwrap();
    let eb = deliver(&mut ta, f, &mut b, &mut tb);
    assert_eq!(eb, vec![Event::UpgradeProposed(shm())]);
    let accepted = b.accept_upgrade().unwrap();
    assert_eq!(accepted.n, 2, "the counter goes on: the hello was frame 1");
    let ea = deliver(&mut tb, accepted, &mut a, &mut ta);
    assert_eq!(ea, vec![Event::Upgraded(shm())]);
    // after it both can send again, and the numbers did not restart
    let next = a.send(vec![Op::Bye { why: "x".into() }]).unwrap();
    assert_eq!(next.n, 3);
    assert!(b.send(vec![]).is_ok());
}

#[test]
fn a_refused_proposal_leaves_the_link_where_it_was() {
    let (mut a, mut b, mut ta, mut tb) = open_pair();
    let f = a.propose(shm()).unwrap();
    deliver(&mut ta, f, &mut b, &mut tb);
    let refused = b.refuse_upgrade("not on this machine").unwrap();
    let ea = deliver(&mut tb, refused, &mut a, &mut ta);
    assert_eq!(ea, vec![Event::UpgradeRefused { why: "not on this machine".into() }]);
    assert!(a.send(vec![]).is_ok(), "the transport it had is still good");
}

#[test]
fn nothing_can_be_sent_while_an_upgrade_is_pending_and_a_second_proposal_is_an_error() {
    let (mut a, _b, _ta, _tb) = open_pair();
    a.propose(shm()).unwrap();
    assert!(a.send(vec![]).unwrap_err().contains("upgrade"));
    assert!(a.propose(Proposal::Encoding("cbor".into())).unwrap_err().contains("pending"));
}

#[test]
fn a_link_that_is_not_open_cannot_propose_and_a_peer_with_nothing_proposed_cannot_accept() {
    let mut l = Link::new(me("seat", true));
    assert!(l.propose(shm()).is_err());
    let (_a, mut b, _ta, _tb) = open_pair();
    assert!(b.accept_upgrade().is_err());
    assert!(b.refuse_upgrade("x").is_err());
}

#[test]
fn an_upgraded_nobody_asked_for_is_refused_and_does_not_move_anything() {
    let (mut a, mut b, mut ta, mut tb) = open_pair();
    let f = b.send(vec![Op::Upgraded(shm())]).unwrap();
    let ea = deliver(&mut tb, f, &mut a, &mut ta);
    assert!(matches!(&ea[..], [Event::Refused { .. }]), "{ea:?}");
    assert!(a.send(vec![]).is_ok());
    let _ = &mut b;
}

#[test]
fn ops_other_than_the_upgrade_still_arrive_in_order_around_it() {
    let (mut a, mut b, mut ta, mut tb) = open_pair();
    let first = a.send(vec![Op::Unsub { target: Target::Id("e1".into()) }]).unwrap();
    let up = a.propose(Proposal::Encoding("cbor".into())).unwrap();
    let mut eb = deliver(&mut ta, first, &mut b, &mut tb);
    eb.extend(deliver(&mut ta, up, &mut b, &mut tb));
    assert!(matches!(&eb[0], Event::Op(Op::Unsub { .. })));
    assert_eq!(eb[1], Event::UpgradeProposed(Proposal::Encoding("cbor".into())));
}

#[test]
fn when_both_peers_propose_at_once_the_initiators_proposal_stands_and_the_other_gives_up_its_own() {
    let (mut a, mut b, mut ta, mut tb) = open_pair();   // a is the initiator
    let fa = a.propose(shm()).unwrap();
    let fb = b.propose(Proposal::Encoding("cbor".into())).unwrap();
    // the initiator's proposal reaches b: b drops its own and is asked to answer the initiator's
    let eb = deliver(&mut ta, fa, &mut b, &mut tb);
    assert_eq!(eb, vec![Event::UpgradeProposed(shm())]);
    // b's proposal reaches the initiator: it is ignored, nothing for the caller to do
    let ea = deliver(&mut tb, fb, &mut a, &mut ta);
    assert!(ea.is_empty(), "{ea:?}");
    let accepted = b.accept_upgrade().unwrap();
    assert_eq!(deliver(&mut tb, accepted, &mut a, &mut ta), vec![Event::Upgraded(shm())]);
    assert!(a.send(vec![]).is_ok() && b.send(vec![]).is_ok(), "both can send again");
}
