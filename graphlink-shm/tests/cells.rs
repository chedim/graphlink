//! The cell table: one slot per `(eid, prop)` key, a seqlock per slot, CBOR values (docs/shared-memory.md, sections 2 to 4). The owner writes; readers map the file read-only.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use graphlink_shm::cells::{Cell, CellState, Cells, CellsReader};
use serde_json::json;

const DEPTH: usize = 32;
const SPINS: u64 = 1_000_000;

fn path() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!("graphlink-cells-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)))
}
fn pair(slots: u64, key_cap: u32, val_cap: u32) -> (Cells, CellsReader) {
    let p = path();
    let w = Cells::create(&p, slots, key_cap, val_cap, 7).unwrap();
    let r = CellsReader::attach(&p).unwrap();
    (w, r)
}

#[test]
fn a_value_put_by_the_owner_is_read_by_the_reader_with_its_revision() {
    let (mut w, r) = pair(64, 64, 256);
    w.put("kitchen::Lamp:desk\u{1}power", 3.0, &json!({"on": true, "level": [1, 2.5, "x", null], "nested": {"a": {"b": -7}}})).unwrap();
    let c = r.get("kitchen::Lamp:desk\u{1}power", SPINS, DEPTH).unwrap().expect("a cell");
    assert_eq!(c, Cell { rev: 3.0, state: CellState::Live, value: json!({"on": true, "level": [1, 2.5, "x", null], "nested": {"a": {"b": -7}}}) });
}

#[test]
fn a_key_never_put_is_none() {
    let (w, r) = pair(64, 64, 256);
    assert_eq!(r.get("nope", SPINS, DEPTH).unwrap(), None);
    assert_eq!(w.get("nope", SPINS, DEPTH).unwrap(), None, "the owner reads its own table too");
}

#[test]
fn a_new_put_replaces_the_value_and_moves_the_revision() {
    let (mut w, r) = pair(64, 64, 256);
    w.put("k", 1.0, &json!("one")).unwrap();
    w.put("k", 2.0, &json!("a longer second value")).unwrap();
    w.put("k", 3.0, &json!(3)).unwrap();
    let c = r.get("k", SPINS, DEPTH).unwrap().unwrap();
    assert_eq!((c.rev, c.value), (3.0, json!(3)));
}

#[test]
fn a_cell_can_be_gone_or_refused_and_says_why() {
    let (mut w, r) = pair(64, 64, 256);
    w.put("a", 1.0, &json!(1)).unwrap();
    w.mark("a", 2.0, CellState::Gone, "").unwrap();
    w.mark("b", 1.0, CellState::Refused, "not yours").unwrap();
    assert_eq!(r.get("a", SPINS, DEPTH).unwrap().unwrap().state, CellState::Gone);
    let b = r.get("b", SPINS, DEPTH).unwrap().unwrap();
    assert_eq!((b.state, b.value), (CellState::Refused, json!("not yours")));
}

#[test]
fn a_scan_returns_the_cells_whose_key_starts_with_the_prefix() {
    let (mut w, r) = pair(64, 64, 256);
    for (k, v) in [("kitchen::Lamp:desk\u{1}power", 1), ("kitchen::Lamp:desk\u{1}level", 2), ("kitchen::Lamp:bed\u{1}power", 3), ("hall::Door:front\u{1}locked", 4)] { w.put(k, 1.0, &json!(v)).unwrap(); }
    let mut got: Vec<(String, serde_json::Value)> = r.scan("kitchen::Lamp:desk", SPINS, DEPTH).unwrap().into_iter().map(|(k, c)| (k, c.value)).collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got, vec![("kitchen::Lamp:desk\u{1}level".to_string(), json!(2)), ("kitchen::Lamp:desk\u{1}power".to_string(), json!(1))]);
    assert_eq!(r.scan("", SPINS, DEPTH).unwrap().len(), 4);
}

#[test]
fn what_does_not_fit_is_an_error_that_says_which_limit() {
    let (mut w, _r) = pair(4, 16, 32);
    assert!(w.put(&"k".repeat(17), 1.0, &json!(1)).unwrap_err().contains("key"));
    assert!(w.put("k", 1.0, &json!("v".repeat(100))).unwrap_err().contains("value"));
    for i in 0..4 { w.put(&format!("key{i}"), 1.0, &json!(i)).unwrap(); }
    assert!(w.put("one more", 1.0, &json!(1)).unwrap_err().contains("full"), "the table has 4 slots");
    w.put("key2", 2.0, &json!("still fits")).unwrap();   // an existing key is not a new slot
}

#[test]
fn many_keys_in_few_slots_are_all_found_after_collisions() {
    let (mut w, r) = pair(64, 32, 32);
    for i in 0..60 { w.put(&format!("entity-{i}"), 1.0, &json!(i)).unwrap(); }
    for i in 0..60 { assert_eq!(r.get(&format!("entity-{i}"), SPINS, DEPTH).unwrap().unwrap().value, json!(i), "entity-{i}"); }
    assert_eq!(r.get("entity-60", SPINS, DEPTH).unwrap(), None);
}

#[test]
fn a_reader_never_sees_a_half_written_value_while_the_owner_writes_as_fast_as_it_can() {
    let (mut w, r) = pair(8, 16, 512);
    w.put("c", 0.0, &json!({"n": 0, "pad": "0".repeat(0)})).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = stop.clone();
    let reader = std::thread::spawn(move || {
        let (mut last, mut reads) = (-1i64, 0u64);
        while !s2.load(Ordering::Relaxed) {
            let c = r.get("c", SPINS, DEPTH).unwrap().unwrap();
            let n = c.value["n"].as_i64().unwrap();
            let pad = c.value["pad"].as_str().unwrap();
            assert_eq!(pad.len() as i64, n % 200, "a torn value: n {n}, pad {}", pad.len());
            assert!(pad.bytes().all(|b| b == b'x'));
            assert!(c.rev as i64 == n, "the revision and the value come from the same write: rev {} n {n}", c.rev);
            assert!(n >= last, "the value went back: {n} after {last}");
            last = n; reads += 1;
        }
        reads
    });
    for n in 1..=100_000i64 { w.put("c", n as f64, &json!({"n": n, "pad": "x".repeat((n % 200) as usize)})).unwrap(); }
    stop.store(true, Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
}

#[test]
fn a_slot_whose_writer_never_finishes_is_an_error_after_the_spins_the_caller_allowed() {
    let (mut w, r) = pair(8, 16, 64);
    w.put("k", 1.0, &json!(1)).unwrap();
    w.leave_slot_mid_write_for_test("k");
    let e = r.get("k", 1000, DEPTH).unwrap_err();
    assert!(e.contains("never settled"), "{e}");
}

#[test]
fn a_hostile_slot_is_an_error_and_not_a_crash() {
    let (mut w, r) = pair(8, 16, 64);
    w.put("k", 1.0, &json!("fine")).unwrap();
    w.corrupt_slot_for_test("k");
    let e = r.get("k", SPINS, DEPTH).unwrap_err();
    assert!(e.contains("corrupt"), "{e}");
    assert!(r.scan("", SPINS, DEPTH).is_err());
}

#[test]
fn the_epoch_and_the_closed_flag_are_readable_and_the_owner_removes_the_file() {
    let p = path();
    let w = Cells::create(&p, 8, 16, 64, 42).unwrap();
    let r = CellsReader::attach(&p).unwrap();
    assert_eq!(r.epoch(), 42);
    assert!(!r.is_closed());
    drop(w);
    assert!(r.is_closed());
    assert!(!p.exists());
}

#[test]
fn attaching_to_something_that_is_not_a_table_says_what() {
    let p = path();
    std::fs::write(&p, vec![0u8; 4096]).unwrap();
    assert!(CellsReader::attach(&p).err().unwrap().contains("not a graphlink cell table"));
    assert!(CellsReader::attach(path()).err().unwrap().contains("cannot open"));
    assert!(Cells::create(path(), 0, 16, 16, 1).err().unwrap().contains("slot"));
}

/// the child of the next test: it reads the key the parent wrote, and writes nothing (a reader's mapping is read-only)
#[test]
fn child_reads() {
    let Ok(p) = std::env::var("GRAPHLINK_CELLS_CHILD") else { return };
    let r = loop { match CellsReader::attach(&p) { Ok(r) => break r, Err(_) => std::thread::sleep(Duration::from_millis(5)) } };
    let end = Instant::now() + Duration::from_secs(20);
    let mut last = 0i64;
    while !r.is_closed() && Instant::now() < end {
        if let Some(c) = r.get("count", SPINS, DEPTH).unwrap() { last = c.value.as_i64().unwrap(); }
        if last == 1000 { std::process::exit(0); }
        std::thread::sleep(Duration::from_micros(100));
    }
    std::process::exit(3);
}

#[test]
fn another_process_reads_what_the_owner_writes() {
    let p = path();
    let mut w = Cells::create(&p, 8, 16, 64, 1).unwrap();
    w.put("count", 0.0, &json!(0)).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap()).args(["child_reads", "--exact", "--nocapture"]).env("GRAPHLINK_CELLS_CHILD", &p).stdout(std::process::Stdio::null()).spawn().unwrap();
    for i in 1..=1000i64 { w.put("count", i as f64, &json!(i)).unwrap(); std::thread::sleep(Duration::from_micros(50)); }
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end { if let Some(st) = child.try_wait().unwrap() { assert!(st.success(), "the child saw {st:?}"); return; } std::thread::sleep(Duration::from_millis(10)); }
    let _ = child.kill();
    panic!("the child never saw the last value");
}
