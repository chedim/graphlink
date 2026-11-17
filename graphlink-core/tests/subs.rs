//! The subscription table and the replica notes, on their own (moved from cliffvm's `link.rs`, #1143).
use std::collections::BTreeSet;

use graphlink_core::{is_special, LinkSubs, Replica, ReplicaState, LINK_GRACE_S};
use serde_json::json;

fn keys(ks: &[(&str, &str)]) -> BTreeSet<(String, String)> { ks.iter().map(|(e, p)| (e.to_string(), p.to_string())) .collect() }

#[test]
fn a_held_key_is_subscribed_once_and_announced_once() {
    let mut s = LinkSubs::default();
    s.update(&keys(&[("p1::A:x", "v"), ("p1::A:x", "w")]), &keys(&[]), 1.0);
    let (subs, unsubs) = s.drain();
    assert_eq!(subs, vec![json!({"eid": "p1::A:x", "props": ["v", "w"]})]);
    assert!(unsubs.is_empty());
    s.update(&keys(&[("p1::A:x", "v"), ("p1::A:x", "w")]), &keys(&[]), 2.0);
    assert_eq!(s.drain(), (vec![], vec![]), "nothing new: nothing announced");
}

#[test]
fn a_key_nobody_reads_any_more_is_dropped_after_the_grace_and_not_before() {
    let mut s = LinkSubs::default();
    s.update(&keys(&[("o::E:1", "a")]), &keys(&[]), 10.0);
    s.drain();
    s.update(&keys(&[]), &keys(&[]), 11.0);
    assert!(s.engaged.contains_key(&("o::E:1".into(), "a".into())), "released at 11.0, still there");
    assert_eq!(s.due(), Some(11.0 + LINK_GRACE_S));
    s.update(&keys(&[]), &keys(&[]), 11.0 + LINK_GRACE_S - 0.01);
    assert!(!s.engaged.is_empty(), "just inside the grace");
    s.update(&keys(&[]), &keys(&[]), 11.0 + LINK_GRACE_S);
    assert!(s.engaged.is_empty());
    let (subs, unsubs) = s.drain();
    assert!(subs.is_empty());
    assert_eq!(unsubs, vec![json!({"eid": "o::E:1", "props": ["a"]})]);
}

#[test]
fn a_key_read_in_the_round_but_not_held_lives_through_the_grace() {
    let mut s = LinkSubs::default();
    s.update(&keys(&[]), &keys(&[("o::E:1", "a")]), 5.0);
    assert_eq!(s.due(), Some(5.0 + LINK_GRACE_S));
    assert_eq!(s.props_of("o::E:1"), vec!["a".to_string()]);
}

#[test]
fn a_retired_namespace_is_forgotten_with_no_notice() {
    let mut s = LinkSubs::default();
    s.update(&keys(&[("p7::A:1", "v"), ("q::B:1", "v")]), &keys(&[]), 1.0);
    s.drain();
    s.forget_prefix("p7::");
    assert_eq!(s.drain(), (vec![], vec![]), "the owner is gone: no unsubscribe is sent");
    assert_eq!(s.props_of("p7::A:1"), Vec::<String>::new());
    assert_eq!(s.props_of("q::B:1"), vec!["v".to_string()]);
}

#[test]
fn the_vm_provided_attributes_are_never_replicated_and_a_replica_note_keeps_its_verdict() {
    for a in ["eid", "cls", "position", "updated", "touched", "pinned"] { assert!(is_special(a), "{a}"); }
    assert!(!is_special("text"));
    let r = Replica { rev: 3.0, have: ["v".to_string()].into_iter().collect(), state: ReplicaState::Refused("no".into()) };
    assert_eq!(r.state, ReplicaState::Refused("no".into()));
    assert_ne!(ReplicaState::Live, ReplicaState::Gone);
}
