//! Frames and version negotiation (docs/frames.md).
use std::collections::BTreeMap;

use graphlink_core::frame::{negotiate, Frame, Negotiated, Op, QueryBy, Target, ValBody, VERSION};
use serde_json::json;

fn roundtrip(f: &Frame) -> Frame { Frame::decode(&f.encode()).expect("decodes") }

#[test]
fn a_frame_with_every_op_survives_encode_then_decode() {
    let f = Frame { v: VERSION, n: 7, ack: 4, ops: vec![
        Op::Hello { ns: "kitchen".into(), build: "b1".into(), epoch: 3.0, have: BTreeMap::from([("kitchen::Lamp:desk".to_string(), 12.0)]), support: vec![1] },
        Op::Failed { support: vec![1, 2], why: "no common version".into() },
        Op::Sub { target: Target::Id("kitchen::Lamp:desk".into()), props: Some(vec!["power".into()]), limit: None },
        Op::Sub { target: Target::Prefix("kitchen::".into()), props: None, limit: Some(50) },
        Op::Unsub { target: Target::Id("kitchen::Lamp:desk".into()) },
        Op::Val { id: "kitchen::Lamp:desk".into(), r: 13.0, rid: None, body: ValBody::Props(json!({"power": "on"})) },
        Op::Val { id: "kitchen::Lamp:desk".into(), r: 13.0, rid: Some("r1".into()), body: ValBody::Props(json!({"level": 3})) },
        Op::Val { id: "kitchen::Lamp:old".into(), r: 2.0, rid: None, body: ValBody::Gone },
        Op::Val { id: "kitchen::Safe:1".into(), r: 1.0, rid: None, body: ValBody::Refused("not yours".into()) },
        Op::Read { id: "kitchen::Lamp:desk".into(), props: vec!["level".into()], rid: "r1".into() },
        Op::Wr { wid: "w1".into(), id: "kitchen::Lamp:desk".into(), prop: "power".into(), value: json!("off") },
        Op::Res { wid: "w1".into(), refused: None },
        Op::Res { wid: "w2".into(), refused: Some("limit".into()) },
        Op::Bye { why: "done".into() },
        Op::Query { qid: "q1".into(), by: QueryBy::Prefix("kitchen::Lamp:".into()), after: Some("kitchen::Lamp:desk".into()), limit: Some(20) },
        Op::Query { qid: "q2".into(), by: QueryBy::Class("Lamp".into()), after: None, limit: None },
        Op::Page { qid: "q1".into(), rows: vec![json!({"id": "kitchen::Lamp:bed"})], next: Some("kitchen::Lamp:bed".into()), total: Some(3) },
        Op::Page { qid: "q2".into(), rows: vec![], next: None, total: None },
        Op::Resync { have: BTreeMap::from([("kitchen::Lamp:desk".to_string(), 12.0)]) },
        Op::Pause { have: BTreeMap::new() },
    ] };
    assert_eq!(roundtrip(&f), f);
}

#[test]
fn the_wire_shape_is_the_one_in_the_spec() {
    let f = Frame { v: 1, n: 7, ack: 4, ops: vec![Op::Sub { target: Target::Id("a::B:c".into()), props: Some(vec!["x".into()]), limit: None }] };
    assert_eq!(f.encode(), json!({"v": 1, "n": 7, "ack": 4, "ops": [{"op": "sub", "id": "a::B:c", "props": ["x"]}]}));
    let g = Frame { v: 1, n: 8, ack: 7, ops: vec![Op::Res { wid: "w".into(), refused: Some("no".into()) }, Op::Val { id: "e".into(), r: 1.0, rid: Some("r9".into()), body: ValBody::Gone }] };
    assert_eq!(g.encode(), json!({"v": 1, "n": 8, "ack": 7, "ops": [{"op": "res", "wid": "w", "refused": "no"}, {"op": "val", "id": "e", "r": 1.0, "rid": "r9", "gone": true}]}));
}

#[test]
fn an_unknown_op_and_unknown_fields_are_ignored_not_an_error() {
    let wire = json!({"v": 1, "n": 1, "ack": 0, "extra": true, "ops": [
        {"op": "teleport", "to": "mars"},
        {"op": "bye", "why": "x", "future_field": 9}
    ]});
    let f = Frame::decode(&wire).unwrap();
    assert_eq!(f.ops, vec![Op::Unknown(json!({"op": "teleport", "to": "mars"})), Op::Bye { why: "x".into() }]);
    assert_eq!(f.encode()["ops"][0], json!({"op": "teleport", "to": "mars"}), "an unknown op is passed on as it came");
}

#[test]
fn a_malformed_frame_is_a_decode_error_that_names_what_is_wrong() {
    for (wire, needle) in [
        (json!([1, 2]), "object"),
        (json!({"v": 1, "n": 1, "ack": 0}), "ops"),
        (json!({"v": 1, "n": 1, "ack": 0, "ops": [5]}), "object"),
        (json!({"v": 1, "n": 1, "ack": 0, "ops": [{"nope": 1}]}), "op"),
        (json!({"v": 1, "n": 1, "ack": 0, "ops": [{"op": "wr", "wid": "w", "id": "e"}]}), "prop"),
        (json!({"v": 1, "n": "x", "ack": 0, "ops": []}), "n"),
        (json!({"v": 1, "n": 1, "ack": 0, "ops": [{"op": "hello", "ns": "a", "build": "b", "epoch": 1, "have": {}, "support": []}]}), "support"),
    ] {
        let e = Frame::decode(&wire).unwrap_err();
        assert!(e.contains(needle), "{wire} -> {e}");
    }
}

#[test]
fn a_sub_names_an_id_or_a_prefix_never_both_and_never_neither() {
    assert!(Frame::decode(&json!({"v":1,"n":1,"ack":0,"ops":[{"op":"sub"}]})).is_err());
    assert!(Frame::decode(&json!({"v":1,"n":1,"ack":0,"ops":[{"op":"sub","id":"a","prefix":"b"}]})).is_err());
}

#[test]
fn the_two_peers_speak_the_highest_version_they_share() {
    assert_eq!(negotiate(&[1, 2, 3], &[2, 3, 4]), Negotiated::Speak(3));
    assert_eq!(negotiate(&[1], &[1]), Negotiated::Speak(1));
    assert_eq!(negotiate(&[3, 1, 2], &[2, 1]), Negotiated::Speak(2), "the order of a list does not matter");
}

#[test]
fn with_no_version_in_common_the_answer_is_a_failed_with_our_own_list() {
    match negotiate(&[1, 2], &[3, 4]) {
        Negotiated::Failed(op) => assert_eq!(op, Op::Failed { support: vec![1, 2], why: "no protocol version in common: we speak [1, 2], you speak [3, 4]".into() }),
        other => panic!("{other:?}"),
    }
    assert!(matches!(negotiate(&[1], &[]), Negotiated::Failed(_)), "an empty list shares nothing");
}

#[test]
fn a_hello_is_decodable_whatever_version_it_is_written_in() {
    let wire = json!({"v": 9, "n": 1, "ack": 0, "ops": [{"op": "hello", "ns": "dev", "build": "x", "epoch": 1.0, "have": {}, "support": [7, 9]}]});
    let f = Frame::decode(&wire).unwrap();
    assert_eq!(f.v, 9);
    assert!(matches!(&f.ops[0], Op::Hello { support, .. } if support == &vec![7, 9]));
}

#[test]
fn the_query_pause_and_resync_ops_have_the_wire_shape_of_the_spec() {
    let f = Frame { v: 1, n: 1, ack: 0, ops: vec![
        Op::Query { qid: "q".into(), by: QueryBy::Class("Lamp".into()), after: None, limit: Some(5) },
        Op::Page { qid: "q".into(), rows: vec![json!({"id": "a"})], next: None, total: Some(1) },
        Op::Resync { have: BTreeMap::from([("e".to_string(), 2.0)]) },
        Op::Pause { have: BTreeMap::new() },
    ] };
    assert_eq!(f.encode()["ops"], json!([
        {"op": "q", "qid": "q", "class": "Lamp", "limit": 5},
        {"op": "page", "qid": "q", "rows": [{"id": "a"}], "total": 1},
        {"op": "resync", "have": {"e": 2.0}},
        {"op": "pause", "have": {}},
    ]));
}

#[test]
fn a_query_names_a_prefix_or_a_class_never_both() {
    assert!(Frame::decode(&json!({"v":1,"n":1,"ack":0,"ops":[{"op":"q","qid":"q"}]})).is_err());
    assert!(Frame::decode(&json!({"v":1,"n":1,"ack":0,"ops":[{"op":"q","qid":"q","prefix":"a","class":"B"}]})).is_err());
}
