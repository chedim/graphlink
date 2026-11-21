//! The store as the seat sees it: rows are entities `<ns>::<Model>:<id>` with a cell per field, a relation is the id of the related entity, a changed field is rewritten and an unchanged one is not, a row that is gone is
//! retired, a value over the cell size arrives whole, and the seat cannot write. The store is the other project's README store (examples/library/store.rs); nothing here starts a process.
mod common;
use common::*;
use graphlink_core::frame::Op;
use graphlink_core::link::Event;
use graphlink_shm::cells::CellState;
use graphlink_store_bridge::bridge::Bridge;
use graphlink_store_bridge::mirror::{entity_id, ffi};
use serde_json::json;

#[path = "../examples/library/store.rs"]
#[allow(dead_code)]
mod store;
use store::Lib;

struct Rig { seat: FakeSeat, bridge: Bridge<Lib> }
impl Rig {
    fn new(tag: &str, lib: Lib) -> Rig {
        let mut seat = FakeSeat::start(tag);
        let peer = seat.peer();
        let mut bridge = Bridge::new(peer, NS, lib);
        // the peer takes the seat's hello and sends its own; the seat's link answers
        for _ in 0..20 { bridge.tick().unwrap(); seat.pump(); }
        Rig { seat, bridge }
    }
    fn ticks(&mut self) { for _ in 0..5 { self.bridge.tick().unwrap(); self.seat.pump(); } }
}
fn eid(model: &str, id: impl Into<u64>) -> String { entity_id(NS, model, id.into()) }

#[test]
fn rows_are_entities_with_one_cell_per_field() {
    let (lib, l, patrons, checkouts) = Lib::seeded();
    let mut rig = Rig::new("rows", lib);
    rig.ticks();
    let r = rig.seat.reader();
    assert_eq!(cell(&r, &eid("Library", ffi(l)), "name"), Some(json!("City Library")));
    assert_eq!(cell(&r, &eid("Patron", ffi(patrons[0])), "email"), Some(json!("alice@example.com")));
    assert_eq!(cell(&r, &eid("Patron", ffi(patrons[1])), "email"), Some(json!("bob@example.com")));
    assert_eq!(cell(&r, &eid("Checkout", ffi(checkouts[0])), "title"), Some(json!("Rust Atomics")));
    // the node's own cell exists, which is what the seat's driver reads first
    assert_eq!(cell(&r, &eid("Library", ffi(l)), ""), Some(json!(true)));
}

#[test]
fn a_relation_is_the_id_of_the_related_entity() {
    let (lib, l, patrons, checkouts) = Lib::seeded();
    let mut rig = Rig::new("rel", lib);
    rig.ticks();
    let r = rig.seat.reader();
    assert_eq!(cell(&r, &eid("Checkout", ffi(checkouts[0])), "library"), Some(json!(eid("Library", ffi(l)))));
    assert_eq!(cell(&r, &eid("Checkout", ffi(checkouts[0])), "patron"), Some(json!(eid("Patron", ffi(patrons[0])))));
    assert_eq!(cell(&r, &eid("Library", ffi(l)), "checkouts"), Some(json!([eid("Checkout", ffi(checkouts[0])), eid("Checkout", ffi(checkouts[1]))])));
    assert_eq!(cell(&r, &eid("Patron", ffi(patrons[1])), "checkouts"), Some(json!([eid("Checkout", ffi(checkouts[1]))])));
}

#[test]
fn a_changed_field_is_rewritten_and_an_unchanged_one_is_not() {
    let (lib, l, patrons, _) = Lib::seeded();
    let mut rig = Rig::new("chg", lib);
    rig.ticks();
    let before = (rev(&rig.seat.reader(), &eid("Library", ffi(l)), "name").unwrap(), rev(&rig.seat.reader(), &eid("Library", ffi(l)), "checkouts").unwrap(), rev(&rig.seat.reader(), &eid("Patron", ffi(patrons[0])), "email").unwrap());
    rig.bridge.mirror.store.libraries.get_mut(l).unwrap().name = "Main Library".into();
    rig.ticks(); rig.ticks();
    let r = rig.seat.reader();
    assert_eq!(cell(&r, &eid("Library", ffi(l)), "name"), Some(json!("Main Library")));
    assert!(rev(&r, &eid("Library", ffi(l)), "name").unwrap() > before.0, "the changed field has a new revision");
    assert_eq!(rev(&r, &eid("Library", ffi(l)), "checkouts").unwrap(), before.1, "an unchanged field of the same row is not rewritten");
    assert_eq!(rev(&r, &eid("Patron", ffi(patrons[0])), "email").unwrap(), before.2, "nor is a field of another row");
}

#[test]
fn a_new_row_appears_and_a_removed_row_is_retired() {
    let (lib, _, _, checkouts) = Lib::seeded();
    let mut rig = Rig::new("life", lib);
    rig.ticks();
    let carol = rig.bridge.mirror.add_patron("carol@example.com");
    rig.ticks();
    assert_eq!(cell(&rig.seat.reader(), &eid("Patron", ffi(carol)), "email"), Some(json!("carol@example.com")), "a row born later is published");
    rig.bridge.mirror.store.remove_checkout(checkouts[1]).unwrap();
    rig.ticks();
    let r = rig.seat.reader();
    assert_eq!(state(&r, &eid("Checkout", ffi(checkouts[1])), "title"), Some(CellState::Gone), "a row the store no longer has is retired");
    assert_eq!(state(&r, &eid("Checkout", ffi(checkouts[1])), ""), Some(CellState::Gone));
    assert_eq!(cell(&r, &eid("Checkout", ffi(checkouts[0])), "title"), Some(json!("Rust Atomics")), "the other rows stay");
}

#[test]
fn a_value_over_the_cell_size_arrives_whole() {
    let mut lib = Lib::empty();
    let big = "x".repeat(50_000);   // more than twelve cells of 4,096 bytes
    let l = lib.add_library(&big);
    let mut rig = Rig::new("big", lib);
    rig.ticks();
    assert_eq!(cell(&rig.seat.reader(), &eid("Library", ffi(l)), "name"), Some(json!(big)), "graphlink-shm carries it in pieces and the reader rebuilds it");
}

#[test]
fn the_seat_cannot_write_and_is_told_so() {
    let (lib, l, _, _) = Lib::seeded();
    let mut rig = Rig::new("ro", lib);
    rig.ticks();
    rig.seat.write("w1", &eid("Library", ffi(l)), "name", json!("Hacked"));
    rig.ticks(); rig.ticks();
    let refused = rig.seat.events.iter().find_map(|e| if let Event::Op(Op::Res { wid, refused }) = e { (wid == "w1").then(|| refused.clone()) } else { None });
    let why = refused.expect("the peer answers the write").expect("and refuses it");
    assert!(why.contains("read-only"), "the refusal says why: {why}");
    assert_eq!(cell(&rig.seat.reader(), &eid("Library", ffi(l)), "name"), Some(json!("City Library")), "nothing changed");
    assert_eq!(rig.bridge.mirror.store.libraries.get(l).unwrap().name, "City Library", "nor in the store");
}
