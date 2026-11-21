//! A value larger than a cell (docs/shared-memory.md, section 4c): the owner stores it in more slots and a reader gets it back whole, so a consumer never sees the size of a slot.
//! Small values are unchanged. The slots of a big value are all freed when it goes.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use graphlink_shm::cells::{CellState, Cells, CellsReader};
use serde_json::{json, Value as J};

const DEPTH: usize = 32;
const SPINS: u64 = 1_000_000;
const VAL_CAP: u32 = 256;

fn path() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("/gl-sp-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst))
}
fn pair(slots: u64) -> (Cells, CellsReader, String) {
    let p = path();
    let w = Cells::create(&p, slots, 64, VAL_CAP, 7).unwrap();
    let r = CellsReader::attach(&p).unwrap();
    (w, r, p)
}
fn text(n: usize, c: char) -> J { J::String(std::iter::repeat(c).take(n).collect()) }
/// the owner has been told every removal was seen: the `Gone` slots may be given to new keys
fn acked(w: &mut Cells) { let g = w.gone_seq(); w.reuse_gone_up_to(g); }

#[test]
fn a_value_of_one_cell_and_a_value_of_several_come_back_whole() {
    let (mut w, r, _p) = pair(64);
    for (k, n) in [("one", 100usize), ("edge", 240), ("twice", (VAL_CAP * 2) as usize), ("ten", (VAL_CAP * 10) as usize)] {
        w.put(k, 3.5, &text(n, 'x')).unwrap();
        let c = r.get(k, SPINS, DEPTH).unwrap().unwrap_or_else(|| panic!("{k}: no cell"));
        assert_eq!((c.state, c.rev, c.value), (CellState::Live, 3.5, text(n, 'x')), "{k}");
    }
}

#[test]
fn a_big_structured_value_keeps_its_keys_in_order_its_floats_and_its_non_ascii_text() {
    let (mut w, r, _p) = pair(64);
    let v = json!({"zeta": [1, 2.5, -7, 1e300, "é€𝄞"], "alpha": {"deep": text(900, 'é'), "n": null, "t": true}, "mid": (0..120).map(|i| json!({"i": i, "s": "row"})).collect::<Vec<_>>()});
    assert!(graphlink_core::cbor::encode(&v).len() > VAL_CAP as usize * 3);
    w.put("big", 1.0, &v).unwrap();
    let c = r.get("big", SPINS, DEPTH).unwrap().unwrap();
    assert_eq!(c.value, v);
    let keys: Vec<&String> = c.value.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["zeta", "alpha", "mid"], "the order of the keys is part of the value");
}

#[test]
fn a_scan_shows_each_key_once_with_its_whole_value_and_none_of_the_pieces() {
    let (mut w, r, _p) = pair(64);
    w.put("a", 1.0, &json!("small")).unwrap();
    w.put("b", 2.0, &text(900, 'b')).unwrap();
    w.put("c", 3.0, &json!(3)).unwrap();
    let mut got: Vec<(String, J)> = r.scan("", SPINS, DEPTH).unwrap().into_iter().map(|(k, c)| (k, c.value)).collect();
    got.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(got, vec![("a".into(), json!("small")), ("b".into(), text(900, 'b')), ("c".into(), json!(3))]);
    assert_eq!(w.scan("", SPINS, DEPTH).unwrap().len(), 3, "the owner reads its own table the same way");
}

#[test]
fn a_big_value_replaced_by_a_smaller_one_a_small_one_and_a_bigger_one_is_each_time_the_last_written() {
    let (mut w, r, _p) = pair(64);
    for v in [text(2000, 'a'), text(700, 'b'), json!("tiny"), text(3000, 'c'), text(300, 'd'), json!(null), text(1000, 'e')] {
        w.put("k", 1.0, &v).unwrap();
        assert_eq!(r.get("k", SPINS, DEPTH).unwrap().unwrap().value, v);
    }
}

#[test]
fn the_slots_of_a_big_value_are_all_freed_when_it_is_replaced_or_goes_and_the_table_does_not_fill() {
    // 12 slots: a value of about 5 pieces and its head takes half of them. Rewriting it forever must never end in "the table is full".
    let (mut w, r, _p) = pair(12);
    for round in 0..40 {
        w.put("k", round as f64, &text(1100 + round, 'z')).unwrap();
        acked(&mut w);
        assert_eq!(r.get("k", SPINS, DEPTH).unwrap().unwrap().value, text(1100 + round, 'z'));
    }
    // it goes: every slot it held can be given to new keys (12 keys fit, a 13th does not)
    w.mark("k", 99.0, CellState::Gone, "").unwrap();
    acked(&mut w);
    for i in 0..12 { w.put(&format!("n{i}"), 1.0, &json!(i)).unwrap_or_else(|e| panic!("key {i} after the big value went: {e}")); }
    assert!(w.put("n12", 1.0, &json!(12)).unwrap_err().contains("full"));
}

#[test]
fn a_value_that_does_not_fit_even_in_pieces_is_the_table_is_full_error_and_the_cell_keeps_what_it_had() {
    let (mut w, r, _p) = pair(8);
    w.put("keep", 1.0, &text(700, 'k')).unwrap();       // a head and 3 pieces
    let e = w.put("huge", 2.0, &text(5000, 'h')).unwrap_err();
    assert!(e.contains("table is full"), "{e}");
    assert!(!e.contains("capacity") && !e.contains("bytes encoded"), "no mention of the size of a cell: {e}");
    assert_eq!(r.get("keep", SPINS, DEPTH).unwrap().unwrap().value, text(700, 'k'));
    assert_eq!(r.get("huge", SPINS, DEPTH).unwrap(), None, "a value that was not stored is not there");
    // the pieces the failed write had taken are given back once the reader has seen them go
    acked(&mut w);
    w.put("small1", 1.0, &json!(1)).unwrap();
    w.put("small2", 1.0, &json!(2)).unwrap();
    w.put("small3", 1.0, &json!(3)).unwrap();
    w.put("small4", 1.0, &json!(4)).unwrap();
}

#[test]
fn a_big_replacement_that_does_not_fit_leaves_the_old_big_value_readable() {
    let (mut w, r, _p) = pair(10);
    w.put("k", 1.0, &text(900, 'o')).unwrap();           // head + 4 pieces
    w.put("other", 1.0, &text(700, 'x')).unwrap();       // head + 3 pieces: 9 of 10
    assert!(w.put("k", 2.0, &text(4000, 'n')).is_err());
    assert_eq!(r.get("k", SPINS, DEPTH).unwrap().unwrap().value, text(900, 'o'));
    assert_eq!(r.get("other", SPINS, DEPTH).unwrap().unwrap().value, text(700, 'x'));
}

#[test]
fn small_values_are_stored_as_before_a_cell_per_key_and_nothing_else() {
    let (mut w, r, _p) = pair(16);
    for i in 0..10 { w.put(&format!("k{i}"), 1.0, &json!({"i": i})).unwrap(); }
    assert_eq!(r.scan("", SPINS, DEPTH).unwrap().len(), 10);
    // 16 slots, 10 used: 6 more fit, so no key took a second slot
    for i in 10..16 { w.put(&format!("k{i}"), 1.0, &json!(i)).unwrap(); }
    assert!(w.put("k16", 1.0, &json!(16)).is_err());
}

#[test]
fn a_value_over_a_cell_is_refused_as_before_when_the_key_cannot_name_its_pieces() {
    let p = path();
    let mut w = Cells::create(&p, 16, 3, VAL_CAP, 7).unwrap();   // a 3-byte key: the pieces cannot be named
    w.put("a", 1.0, &json!(1)).unwrap();
    assert!(w.put("a", 2.0, &text(900, 'x')).is_err());
}

#[test]
fn a_reader_never_sees_a_mixture_of_two_big_values_while_the_owner_rewrites_one() {
    let (mut w, r, _p) = pair(32);
    struct Done(Arc<AtomicBool>);
    impl Drop for Done { fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); } }   // the reader stops even if the writer panics
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let writer = std::thread::spawn(move || {
        let _done = Done(s2);
        let mut i = 0usize;
        let end = Instant::now() + Duration::from_millis(600);
        while Instant::now() < end {
            let c = (b'a' + (i % 26) as u8) as char;
            w.put("k", i as f64, &text(600 + (i % 7) * 211, c)).unwrap();
            if i % 3 == 0 { acked(&mut w); }
            i += 1;
        }
        w
    });
    let mut seen = 0;
    while !stop.load(Ordering::SeqCst) {
        match r.get("k", SPINS, DEPTH) {
            Ok(Some(c)) => {
                let s = c.value.as_str().expect("a string").to_string();
                let first = s.chars().next().unwrap();
                assert!(s.chars().all(|x| x == first), "a mixture of two values: {} chars", s.len());
                assert_eq!(s.chars().count(), 600 + ((c.rev as usize) % 7) * 211, "the value belongs to its revision");
                seen += 1;
            }
            Ok(None) => {}
            Err(e) => panic!("a read failed: {e}"),
        }
    }
    let _w = writer.join().expect("the writer panicked");
    assert!(seen > 10, "the reader read {seen} values while the writer rewrote");
}

#[test]
fn a_reader_that_attaches_later_reads_the_big_values_too() {
    let p = path();
    let mut w = Cells::create(&p, 32, 64, VAL_CAP, 7).unwrap();
    w.put("k", 1.0, &text(1500, 'q')).unwrap();
    let r = CellsReader::attach(&p).unwrap();
    assert_eq!(r.get("k", SPINS, DEPTH).unwrap().unwrap().value, text(1500, 'q'));
}

#[test]
fn a_write_the_table_cannot_hold_takes_no_slot_not_even_until_the_reader_has_seen_something() {
    let (mut w, _r, _p) = pair(8);
    w.put("keep", 1.0, &text(700, 'k')).unwrap();       // a head and 3 pieces: 4 of 8
    assert!(w.put("huge", 2.0, &text(5000, 'h')).unwrap_err().contains("table is full"));
    // nothing was taken by the failed write, so no acknowledgement is needed to use the 4 slots left
    for i in 0..4 { w.put(&format!("s{i}"), 1.0, &json!(i)).unwrap_or_else(|e| panic!("s{i}: {e}")); }
}
