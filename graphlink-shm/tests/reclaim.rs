//! Reclaiming slots (docs/shared-memory.md, section 4b): the owner stamps each removal with a number and keeps the count in the header; the reader says, in a cell of its
//! own table, up to which removal it has seen; the owner then gives those `Gone` slots to new keys.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use graphlink_shm::cells::{CellState, Cells, CellsReader};
use serde_json::{json, Value};

const DEPTH: usize = 32;
const SPINS: u64 = 1_000_000;

fn path() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("/gl-r-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst))
}
fn pair(slots: u64) -> (Cells, CellsReader) {
    let p = path();
    let w = Cells::create(&p, slots, 64, 256, 1).unwrap();
    let r = CellsReader::attach(&p).unwrap();
    (w, r)
}
fn gone(w: &mut Cells, k: &str, rev: f64) { w.mark(k, rev, CellState::Gone, "").unwrap(); }

#[test]
fn each_removal_is_counted_once_and_the_reader_sees_the_count() {
    let (mut w, r) = pair(8);
    assert_eq!((w.gone_seq(), r.gone_seq()), (0, 0));
    w.put("a", 1.0, &json!(1)).unwrap(); gone(&mut w, "a", 2.0);
    assert_eq!((w.gone_seq(), r.gone_seq()), (1, 1));
    gone(&mut w, "a", 3.0);                                   // already gone: not a new removal
    assert_eq!(w.gone_seq(), 1);
    w.put("b", 1.0, &json!(1)).unwrap(); gone(&mut w, "b", 2.0);
    assert_eq!(r.gone_seq(), 2);
    w.put("a", 4.0, &json!(2)).unwrap(); gone(&mut w, "a", 5.0);   // came back and went again: a new removal
    assert_eq!(r.gone_seq(), 3);
}

#[test]
fn a_gone_cell_still_reads_as_gone_with_a_null_value() {
    let (mut w, r) = pair(8);
    w.put("a", 1.0, &json!(1)).unwrap(); gone(&mut w, "a", 2.0);
    let c = r.get("a", SPINS, DEPTH).unwrap().unwrap();
    assert_eq!((c.state, c.value, c.rev), (CellState::Gone, Value::Null, 2.0));
}

#[test]
fn a_table_full_of_gone_cells_takes_a_new_key_only_in_a_slot_the_reader_has_acked() {
    let (mut w, r) = pair(4);
    for i in 0..4 { w.put(&format!("k{i}"), 1.0, &json!(i)).unwrap(); }
    for i in 0..4 { gone(&mut w, &format!("k{i}"), 2.0); }       // stamps 1, 2, 3, 4
    assert!(w.put("n0", 1.0, &json!(0)).unwrap_err().contains("full"), "nothing acked: still full");
    w.reuse_gone_up_to(2);
    w.put("n0", 1.0, &json!(0)).unwrap(); w.put("n1", 1.0, &json!(1)).unwrap();
    assert!(w.put("n2", 1.0, &json!(2)).unwrap_err().contains("full"), "only two slots were acked");
    assert_eq!(r.get("n0", SPINS, DEPTH).unwrap().unwrap().state, CellState::Live);
    w.reuse_gone_up_to(4);
    w.put("n2", 1.0, &json!(2)).unwrap(); w.put("n3", 1.0, &json!(3)).unwrap();
    for k in ["n0", "n1", "n2", "n3"] { assert_eq!(r.get(k, SPINS, DEPTH).unwrap().unwrap().state, CellState::Live, "{k}"); }
}

#[test]
fn a_live_cell_is_never_given_away() {
    let (mut w, _r) = pair(4);
    for i in 0..4 { w.put(&format!("k{i}"), 1.0, &json!(i)).unwrap(); }
    w.reuse_gone_up_to(u64::MAX);
    assert!(w.put("n", 1.0, &json!(0)).unwrap_err().contains("full"));
}

#[test]
fn survivors_and_new_keys_are_all_found_and_a_recycled_key_is_never_live() {
    let (mut w, r) = pair(8);
    for i in 0..8 { w.put(&format!("k{i}"), 1.0, &json!(i)).unwrap(); }
    for i in [1, 3, 4, 6] { gone(&mut w, &format!("k{i}"), 2.0); }
    w.reuse_gone_up_to(u64::MAX);
    for i in 0..4 { w.put(&format!("n{i}"), 1.0, &json!(i)).unwrap(); }
    for i in [0, 2, 5, 7] { assert_eq!(r.get(&format!("k{i}"), SPINS, DEPTH).unwrap().unwrap().state, CellState::Live, "survivor k{i}"); }
    for i in 0..4 { assert_eq!(r.get(&format!("n{i}"), SPINS, DEPTH).unwrap().unwrap().value, json!(i), "new n{i}"); }
    for i in [1, 3, 4, 6] {
        match r.get(&format!("k{i}"), SPINS, DEPTH).unwrap() { None => {} Some(c) => assert_eq!(c.state, CellState::Gone, "k{i} must not read as live") }
    }
}

#[test]
fn a_gone_key_that_comes_back_takes_its_own_slot_without_any_ack() {
    let (mut w, r) = pair(4);
    for i in 0..4 { w.put(&format!("k{i}"), 1.0, &json!(i)).unwrap(); }
    gone(&mut w, "k0", 2.0);
    w.put("k0", 3.0, &json!("back")).unwrap();
    assert_eq!(r.get("k0", SPINS, DEPTH).unwrap().unwrap().value, json!("back"));
}

#[test]
fn the_ack_is_a_cell_in_the_readers_own_table_read_by_the_owner_and_hidden_from_scans() {
    let p = path();
    let mut mine = Cells::create(&p, 4, 64, 256, 1).unwrap();     // the reader's own table
    let theirs = CellsReader::attach(&p).unwrap();                  // the owner of the other table reads it
    assert_eq!(theirs.peer_ack(SPINS, DEPTH).unwrap(), 0, "nothing said yet");
    mine.ack_seen(5).unwrap();
    assert_eq!(theirs.peer_ack(SPINS, DEPTH).unwrap(), 5);
    for g in 6..200 { mine.ack_seen(g).unwrap(); }                  // the same cell each time: never fills the table
    assert_eq!(theirs.peer_ack(SPINS, DEPTH).unwrap(), 199);
    mine.put("a", 1.0, &json!(1)).unwrap();
    let keys: Vec<String> = theirs.scan("", SPINS, DEPTH).unwrap().into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, vec!["a".to_string()], "the ack key is not one of the owner's nodes");
}

#[test]
fn keys_churn_through_a_few_slots_when_the_reader_acks() {
    let (mut w, r) = pair(4);
    let p = path();
    let mut readers_own = Cells::create(&p, 4, 64, 256, 1).unwrap();
    let owner_reads_ack = CellsReader::attach(&p).unwrap();
    for round in 0..5 {
        for i in 0..4 { w.put(&format!("r{round}k{i}"), 1.0, &json!(i)).unwrap(); }
        for i in 0..4 { gone(&mut w, &format!("r{round}k{i}"), 2.0); }
        // the reader: read the count, then scan, then say it has seen up to the count
        let g = r.gone_seq();
        let seen = r.scan("", SPINS, DEPTH).unwrap();
        assert_eq!(seen.iter().filter(|(_, c)| c.state == CellState::Gone).count(), 4);
        readers_own.ack_seen(g).unwrap();
        // the owner: read what the reader said and allow those slots
        w.reuse_gone_up_to(owner_reads_ack.peer_ack(SPINS, DEPTH).unwrap());
    }
}

#[test]
fn a_removal_counted_in_the_header_is_already_a_gone_cell_in_the_table() {
    // the order that makes the ack safe: the slot is written before the count is raised, so a reader that read the count and then scans sees every removal up to it
    let (mut w, r) = pair(4096);
    let stop = Arc::new(AtomicBool::new(false));
    let t = { let stop = stop.clone(); std::thread::spawn(move || {
        for i in 0..3000 { let k = format!("k{i}"); w.put(&k, 1.0, &json!(i)).unwrap(); w.mark(&k, 2.0, CellState::Gone, "").unwrap(); }
        stop.store(true, Ordering::SeqCst);
    }) };
    // a point lookup of the newest counted removals, as often as the reader can: a scan is too slow to land in the window between the count and the slot
    let mut checks = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let g = r.gone_seq();
        for i in g.saturating_sub(2)..g {
            let c = r.get(&format!("k{i}"), SPINS, DEPTH).unwrap();
            assert!(matches!(c, Some(ref c) if c.state == CellState::Gone), "removal {} of {g} is counted but its cell is {c:?}", i + 1);
        }
        checks += 1;
    }
    assert!(checks > 10, "the reader ran only {checks} times");
    t.join().unwrap();
}
