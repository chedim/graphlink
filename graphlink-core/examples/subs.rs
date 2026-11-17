//! One engine's side of a link: which foreign values it reads, and what to tell the owner at the end of each round.
//! Run it with `cargo run -p graphlink-core --example subs`.
use std::collections::BTreeSet;

use graphlink_core::{LinkSubs, LINK_GRACE_S};

fn keys(ks: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    ks.iter().map(|(e, p)| (e.to_string(), p.to_string())).collect()
}

fn main() {
    let mut subs = LinkSubs::default();

    // Round 1 (engine time 10.0): a rule of this engine reads two props of an entity owned by the peer `kitchen`.
    // `held` = the foreign keys that rules keep reading; `read` = the foreign keys read in this round only.
    subs.update(&keys(&[("kitchen::Lamp:desk", "power"), ("kitchen::Lamp:desk", "level")]), &keys(&[]), 10.0);
    let (subscribe, unsubscribe) = subs.drain();
    println!("round 1: send sub {subscribe:?}, unsub {unsubscribe:?}");

    // Round 2 (11.0): nothing reads them any more. They stay for the grace, so a rule that reads them again does not resubscribe.
    subs.update(&keys(&[]), &keys(&[]), 11.0);
    println!("round 2: next decision at {:?} (grace {LINK_GRACE_S} s)", subs.due());
    assert_eq!(subs.drain(), (vec![], vec![]));

    // Round 3: the grace is over, so the owner is told to stop sending.
    subs.update(&keys(&[]), &keys(&[]), 11.0 + LINK_GRACE_S);
    let (subscribe, unsubscribe) = subs.drain();
    println!("round 3: send sub {subscribe:?}, unsub {unsubscribe:?}");

    // The peer left: forget everything it owned, no notice (the owner is gone).
    subs.update(&keys(&[("kitchen::Lamp:desk", "power")]), &keys(&[]), 20.0);
    subs.forget_prefix("kitchen::");
    assert_eq!(subs.drain(), (vec![], vec![]));
}
