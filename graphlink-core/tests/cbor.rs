//! The CBOR subset that JSON can hold (docs/shared-memory.md, section 3), against the example vectors of RFC 8949, appendix A.
use graphlink_core::cbor::{decode, encode};
use serde_json::{json, Value as J};

const DEPTH: usize = 64;

fn hex(s: &str) -> Vec<u8> { (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect() }

/// the value encodes to exactly these bytes and the bytes decode back to the same value
fn both(v: J, h: &str) {
    assert_eq!(encode(&v), hex(h), "encode {v}");
    assert_eq!(decode(&hex(h), DEPTH).unwrap(), v, "decode {h}");
}

#[test]
fn integers_take_the_shortest_form_as_in_the_rfc() {
    both(json!(0), "00"); both(json!(1), "01"); both(json!(10), "0a"); both(json!(23), "17"); both(json!(24), "1818"); both(json!(25), "1819");
    both(json!(100), "1864"); both(json!(1000), "1903e8"); both(json!(1000000), "1a000f4240"); both(json!(1000000000000u64), "1b000000e8d4a51000");
    both(json!(18446744073709551615u64), "1bffffffffffffffff");
    both(json!(-1), "20"); both(json!(-10), "29"); both(json!(-100), "3863"); both(json!(-1000), "3903e7");
    both(json!(i64::MIN), "3b7fffffffffffffff");
}

#[test]
fn floats_are_written_as_64_bit_and_stay_floats() {
    both(json!(1.1), "fb3ff199999999999a"); both(json!(1.0e300), "fb7e37e43c8800759c"); both(json!(-4.1), "fbc010666666666666");
    both(json!(1.0), "fb3ff0000000000000");
    assert_ne!(encode(&json!(1.0)), encode(&json!(1)), "a float and an int are different values");
}

#[test]
fn a_float_of_any_width_is_read() {
    assert_eq!(decode(&hex("f90000"), DEPTH).unwrap(), json!(0.0));
    assert_eq!(decode(&hex("f93c00"), DEPTH).unwrap(), json!(1.0));
    assert_eq!(decode(&hex("f93e00"), DEPTH).unwrap(), json!(1.5));
    assert_eq!(decode(&hex("f97bff"), DEPTH).unwrap(), json!(65504.0));
    assert_eq!(decode(&hex("f9c400"), DEPTH).unwrap(), json!(-4.0));
    assert_eq!(decode(&hex("fa47c35000"), DEPTH).unwrap(), json!(100000.0));
    assert_eq!(decode(&hex("fa7f7fffff"), DEPTH).unwrap(), json!(3.4028234663852886e38));
}

#[test]
fn simple_values_and_text() {
    both(J::Bool(false), "f4"); both(J::Bool(true), "f5"); both(J::Null, "f6");
    both(json!(""), "60"); both(json!("a"), "6161"); both(json!("IETF"), "6449455446"); both(json!("\"\\"), "62225c");
    both(json!("\u{00fc}"), "62c3bc"); both(json!("\u{6c34}"), "63e6b0b4"); both(json!("\u{10151}"), "64f0908591");
}

#[test]
fn arrays_and_maps() {
    both(json!([]), "80"); both(json!([1, 2, 3]), "83010203"); both(json!([1, [2, 3], [4, 5]]), "8301820203820405");
    both(J::Array((1..=25).map(|i| json!(i)).collect()), "98190102030405060708090a0b0c0d0e0f101112131415161718181819");
    both(json!({}), "a0"); both(json!({"a": 1, "b": [2, 3]}), "a26161016162820203"); both(json!(["a", {"b": "c"}]), "826161a161626163");
    both(json!({"a": "A", "b": "B", "c": "C", "d": "D", "e": "E"}), "a56161614161626142616361436164614461656145");
}

#[test]
fn map_keys_keep_the_order_they_were_written_in() {
    let v = json!({"b": 1, "a": 2});
    assert_eq!(encode(&v), hex("a2616201616102"), "b first, as written");
    let back = decode(&hex("a2616201616102"), DEPTH).unwrap();
    assert_eq!(back.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["b", "a"]);
    assert_ne!(encode(&json!({"a": 2, "b": 1})), encode(&v), "a reorder is different bytes");
}

#[test]
fn what_json_cannot_hold_is_refused_with_a_reason() {
    for (h, needle) in [
        ("c11a514b67b0", "tag"),
        ("4401020304", "byte string"),
        ("5f42010243030405ff", "byte string"),
        ("9f018202039f0405ffff", "indefinite"),
        ("f7", "simple"),
        ("f0", "simple"),
        ("fb7ff8000000000000", "NaN"),
        ("fb7ff0000000000000", "infinit"),
        ("f97c00", "infinit"),
        ("3bffffffffffffffff", "range"),
        ("a1010a", "key"),
        ("a2616101616102", "duplicate"),
        ("7801", "truncated"),
        ("62c328", "UTF-8"),
    ] {
        let e = decode(&hex(h), DEPTH).unwrap_err();
        assert!(e.to_lowercase().contains(&needle.to_lowercase()), "{h}: {e}");
    }
}

#[test]
fn bytes_that_stop_early_or_run_on_are_errors() {
    assert!(decode(&hex("1b0000"), DEPTH).unwrap_err().contains("truncated"));
    assert!(decode(&hex("8301"), DEPTH).unwrap_err().contains("truncated"));
    assert!(decode(&hex("0001"), DEPTH).unwrap_err().contains("trailing"));
    assert!(decode(&[], DEPTH).is_err());
}

#[test]
fn a_length_that_the_bytes_cannot_hold_is_refused_before_anything_is_allocated() {
    assert!(decode(&hex("9bffffffffffffffff"), DEPTH).is_err());
    assert!(decode(&hex("bbffffffffffffffff"), DEPTH).is_err());
    assert!(decode(&hex("7bffffffffffffffff"), DEPTH).is_err());
    assert!(decode(&hex("9a7fffffff"), DEPTH).is_err());
}

#[test]
fn nesting_deeper_than_the_limit_is_an_error_and_not_a_stack_overflow() {
    let deep = vec![0x81u8; 10_000].into_iter().chain([0x00]).collect::<Vec<u8>>();
    assert!(decode(&deep, 64).unwrap_err().contains("deep"));
    let ok = vec![0x81u8; 5].into_iter().chain([0x00]).collect::<Vec<u8>>();
    assert!(decode(&ok, 5).is_ok(), "five levels of array fit a limit of five");
    assert!(decode(&ok, 4).is_err());
}

#[test]
fn what_encode_writes_decode_reads_back() {
    for v in [json!(null), json!(-0.5), json!("ü水𐅑"), json!([[], {}, [{"k": [1, 2.5, "x", null, true]}]]), json!({"nested": {"a": {"b": [1, {"c": -7}]}}, "list": [0, 1, 2]})] {
        assert_eq!(decode(&encode(&v), DEPTH).unwrap(), v);
    }
}
