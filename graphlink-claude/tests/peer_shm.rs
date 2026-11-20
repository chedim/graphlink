//! The peer over the shared-memory regions (the same contract as membrama's tests/peer_shm.rs). The test plays the seat (tests/common).
mod common;
use common::{key, FakeSeat};
use graphlink_core::frame::{Op, Target};
use graphlink_core::link::Event;
use graphlink_shm::cells::CellsReader;
use serde_json::json;

#[test]
fn the_peer_answers_the_seats_hello_with_its_namespace() {
    let mut seat = FakeSeat::start("a");
    let mut peer = seat.peer();
    let events = seat.run(&mut peer);
    assert!(events.iter().any(|e| matches!(e, Event::Opened { peer_ns, .. } if peer_ns == "claude")), "{events:?}");
    assert!(peer.is_open());
}

#[test]
fn a_node_the_peer_publishes_is_read_by_the_seat_from_the_peers_table() {
    let mut seat = FakeSeat::start("b");
    let mut peer = seat.peer();
    seat.run(&mut peer);
    peer.publish("claude::Reply:7", "state", &json!("running")).unwrap();
    peer.publish("claude::Reply:7", "state", &json!("done")).unwrap();
    let reader = CellsReader::attach(format!("/gl-{}-pc", seat.name)).unwrap();
    let cell = reader.get(&key("claude::Reply:7", "state"), 1_000_000, 32).unwrap().expect("the cell is there");
    assert_eq!(cell.value, json!("done"));
    assert_eq!(cell.rev, 2.0);
}

#[test]
fn a_subscription_reaches_the_seat_as_a_sub_op_and_a_seat_cell_is_read_by_the_peer() {
    let mut seat = FakeSeat::start("c");
    let mut peer = seat.peer();
    seat.run(&mut peer);
    peer.subscribe(Target::Prefix("membrane::LlmRequest:".into()), None).unwrap();
    let events = seat.run(&mut peer);
    assert!(events.iter().any(|e| matches!(e, Event::Op(Op::Sub { target: Target::Prefix(p), .. }) if p == "membrane::LlmRequest:")), "{events:?}");
    seat.put("membrane::LlmRequest:1-1", "server", json!("claude"));
    assert_eq!(peer.read("membrane::LlmRequest:1-1", "server").unwrap(), Some(json!("claude")));
    assert_eq!(peer.scan("membrane::LlmRequest:").unwrap().len(), 1);
}

#[test]
fn the_peer_sees_the_seats_end_close() {
    use graphlink_shm::{remove_region, Shm};
    let name = format!("gc{}z", std::process::id() % 100000);
    for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{name}-{r}")); }
    let ctl = Shm::create(format!("/gl-{name}-ctl"), 65536).unwrap();
    let _table = graphlink_shm::cells::Cells::create(format!("/gl-{name}-sc"), 64, 128, 4096, 1).unwrap();
    let peer = graphlink_claude::peer::Peer::attach(&name, "claude", &common::NUMBERS).unwrap();
    assert!(!peer.is_closed());
    drop(ctl);
    assert!(peer.is_closed());
    for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{name}-{r}")); }
}

#[test]
fn the_peers_hello_carries_the_token_the_seat_made_for_it() {
    let mut seat = FakeSeat::start("f");
    let mut peer = seat.peer().with_token("the-token".to_string());
    let events = seat.run(&mut peer);
    assert!(events.iter().any(|e| matches!(e, Event::Opened { peer_token: Some(t), .. } if t == "the-token")), "{events:?}");
}
