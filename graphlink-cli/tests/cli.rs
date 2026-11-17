//! The `graphlink` tool: `serve` owns a few nodes, `watch` prints them as they change, `write` changes one (the process boundary, over real TCP).
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use graphlink_core::frame::{Op, QueryBy, VERSION};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_net::Tcp;
use serde_json::{json, Value as J};

const BIN: &str = env!("CARGO_BIN_EXE_graphlink");

struct Serve { child: Child, addr: String }
impl Drop for Serve { fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); } }

fn nodes_file() -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!("graphlink-cli-{}-{}.json", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::write(&p, json!({"kitchen::Lamp:desk": {"power": "off", "level": 1}, "kitchen::Lamp:bed": {"power": "on"}, "hall::Door:front": {"locked": true}}).to_string()).unwrap();
    p
}

fn serve() -> Serve {
    let mut child = Command::new(BIN).args(["serve", "127.0.0.1:0", "--ns", "kitchen", "--nodes"]).arg(nodes_file()).stdout(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    let addr = line.trim().strip_prefix("listening ").unwrap_or_else(|| panic!("the first line was {line:?}")).to_string();
    Serve { child, addr }
}

struct Watch { child: Child, lines: BufReader<std::process::ChildStdout> }
impl Drop for Watch { fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); } }
impl Watch {
    fn start(addr: &str, args: &[&str]) -> Watch {
        let mut child = Command::new(BIN).arg("watch").arg(addr).args(args).stdout(Stdio::piped()).spawn().unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap());
        Watch { child, lines }
    }
    fn next(&mut self) -> J {
        let mut l = String::new();
        self.lines.read_line(&mut l).unwrap();
        serde_json::from_str(l.trim()).unwrap_or_else(|e| panic!("{e}: {l:?}"))
    }
}

fn write(addr: &str, id: &str, prop: &str, value: &str) -> (bool, String) {
    let o = Command::new(BIN).args(["write", addr, id, prop, value]).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[test]
fn a_watcher_gets_the_current_value_and_then_each_change() {
    let s = serve();
    let mut w = Watch::start(&s.addr, &["kitchen::Lamp:desk"]);
    let first = w.next();
    assert_eq!((first["id"].as_str(), &first["props"]["power"], &first["props"]["level"]), (Some("kitchen::Lamp:desk"), &json!("off"), &json!(1)), "{first}");
    let (ok, out) = write(&s.addr, "kitchen::Lamp:desk", "power", "\"on\"");
    assert!(ok, "{out}");
    assert_eq!(out, "ok");
    let second = w.next();
    assert_eq!(second["props"]["power"], json!("on"), "{second}");
    assert!(second["r"].as_f64().unwrap() > first["r"].as_f64().unwrap(), "the revision goes up");
}

#[test]
fn a_prefix_watch_gets_every_node_under_it_and_not_the_others() {
    let s = serve();
    let mut w = Watch::start(&s.addr, &["--prefix", "kitchen::"]);
    let mut ids = vec![w.next()["id"].as_str().unwrap().to_string(), w.next()["id"].as_str().unwrap().to_string()];
    ids.sort();
    assert_eq!(ids, vec!["kitchen::Lamp:bed", "kitchen::Lamp:desk"]);
    write(&s.addr, "hall::Door:front", "locked", "false");
    write(&s.addr, "kitchen::Lamp:bed", "power", "\"off\"");
    assert_eq!(w.next()["id"], json!("kitchen::Lamp:bed"), "the hall door is not under the prefix");
}

#[test]
fn two_watchers_both_see_a_change_made_by_a_third_peer() {
    let s = serve();
    let (mut a, mut b) = (Watch::start(&s.addr, &["hall::Door:front"]), Watch::start(&s.addr, &["hall::Door:front"]));
    a.next(); b.next();
    assert!(write(&s.addr, "hall::Door:front", "locked", "false").0);
    assert_eq!(a.next()["props"]["locked"], json!(false));
    assert_eq!(b.next()["props"]["locked"], json!(false));
}

#[test]
fn a_write_to_a_node_that_does_not_exist_is_refused_with_a_reason_and_a_failing_exit() {
    let s = serve();
    let (ok, out) = write(&s.addr, "kitchen::Lamp:nope", "power", "\"on\"");
    assert!(!ok);
    assert!(out.starts_with("refused: ") && out.contains("no such node"), "{out}");
}

#[test]
fn a_value_that_is_not_json_is_a_usage_error_before_any_connection() {
    let o = Command::new(BIN).args(["write", "127.0.0.1:1", "e", "p", "{oops"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("JSON"), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn a_read_is_answered_by_a_val_carrying_its_rid_and_makes_no_subscription() {
    let s = serve();
    let mut t = Tcp::connect(&s.addr).unwrap();
    let mut link = Link::new(Me { ns: "probe".into(), build: "t".into(), epoch: 1.0, support: vec![VERSION], initiator: true });
    t.send(&link.open()).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    let mut events = vec![];
    let mut sent = false;
    while Instant::now() < end {
        while let Some(f) = t.recv().unwrap() { let (ev, reply) = link.receive(f); events.extend(ev); if let Some(r) = reply { t.send(&r).unwrap(); } }
        if link.is_open() && !sent {
            t.send(&link.send(vec![Op::Read { id: "kitchen::Lamp:desk".into(), props: vec!["level".into()], rid: "r7".into() }]).unwrap()).unwrap();
            sent = true;
        }
        if events.iter().any(|e| matches!(e, Event::Op(Op::Val { .. }))) { break; }
        std::thread::sleep(Duration::from_millis(5));
    }
    let val = events.iter().find_map(|e| if let Event::Op(Op::Val { id, rid, body, .. }) = e { Some((id.clone(), rid.clone(), body.clone())) } else { None }).expect("no val");
    assert_eq!((val.0.as_str(), val.1.as_deref()), ("kitchen::Lamp:desk", Some("r7")));
    assert_eq!(val.2, graphlink_core::frame::ValBody::Props(json!({"level": 1})), "only the props asked for");
}

/// connect a raw peer, open, send `ops` once open, and return the first op that satisfies `want`
fn ask(addr: &str, ops: Vec<Op>, want: impl Fn(&Op) -> bool) -> Op {
    let mut t = Tcp::connect(addr).unwrap();
    let mut link = Link::new(Me { ns: "probe".into(), build: "t".into(), epoch: 1.0, support: vec![VERSION], initiator: true });
    t.send(&link.open()).unwrap();
    let (mut sent, end) = (false, Instant::now() + Duration::from_secs(5));
    while Instant::now() < end {
        while let Some(f) = t.recv().unwrap() {
            let (events, reply) = link.receive(f);
            if let Some(r) = reply { t.send(&r).unwrap(); }
            for e in events { if let Event::Op(op) = e { if want(&op) { return op; } } }
        }
        if link.is_open() && !sent { t.send(&link.send(ops.clone()).unwrap()).unwrap(); sent = true; }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no answer")
}

#[test]
fn a_paged_query_by_prefix_and_by_class_answers_with_rows_a_next_and_a_total() {
    let s = serve();
    let first = ask(&s.addr, vec![Op::Query { qid: "q1".into(), by: QueryBy::Prefix("kitchen::".into()), after: None, limit: Some(1) }], |o| matches!(o, Op::Page { .. }));
    assert_eq!(first, Op::Page { qid: "q1".into(), rows: vec![json!({"id": "kitchen::Lamp:bed"})], next: Some("kitchen::Lamp:bed".into()), total: Some(2) });
    let second = ask(&s.addr, vec![Op::Query { qid: "q2".into(), by: QueryBy::Prefix("kitchen::".into()), after: Some("kitchen::Lamp:bed".into()), limit: Some(1) }], |o| matches!(o, Op::Page { .. }));
    assert_eq!(second, Op::Page { qid: "q2".into(), rows: vec![json!({"id": "kitchen::Lamp:desk"})], next: None, total: Some(1) });
    let by_class = ask(&s.addr, vec![Op::Query { qid: "q3".into(), by: QueryBy::Class("Door".into()), after: None, limit: None }], |o| matches!(o, Op::Page { .. }));
    assert_eq!(by_class, Op::Page { qid: "q3".into(), rows: vec![json!({"id": "hall::Door:front"})], next: None, total: Some(1) });
}

// ---- tokens: who may talk to a serving peer -------------------------------------------------------------------------------------------------------

fn secret_file() -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!("graphlink-cli-secret-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    // every file holds its own secret (64 hex characters made of the counter)
    std::fs::write(&p, format!("{:0>64x}", (std::process::id() as u64) << 20 | N.load(Ordering::SeqCst))).unwrap();
    p
}
fn mint(secret: &std::path::Path, ns: &str) -> String {
    let o = Command::new(BIN).args(["token", "mint", "--secret-file"]).arg(secret).args(["--sub", "tester", "--ns", ns, "--ttl", "300"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}
fn token_file(token: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!("graphlink-cli-token-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::write(&p, token).unwrap();
    p
}
fn serve_with_secret(secret: &std::path::Path) -> Serve {
    let mut child = Command::new(BIN).args(["serve", "127.0.0.1:0", "--ns", "kitchen", "--nodes"]).arg(nodes_file()).arg("--secret-file").arg(secret).stdout(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    Serve { child, addr: line.trim().strip_prefix("listening ").unwrap().to_string() }
}
fn write_as(addr: &str, extra: &[&str]) -> (bool, String) {
    let o = Command::new(BIN).args(["write", addr, "kitchen::Lamp:desk", "power", "\"on\""]).args(extra).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[test]
fn a_minted_token_names_its_namespace_and_needs_a_life() {
    let secret = secret_file();
    let t = mint(&secret, "garage");
    let c = graphlink_auth::verify(&t, std::fs::read_to_string(&secret).unwrap().trim().as_bytes(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()).unwrap();
    assert_eq!((c["sub"].as_str(), c["ns"].as_str()), (Some("tester"), Some("garage")));
    let o = Command::new(BIN).args(["token", "mint", "--secret-file"]).arg(&secret).args(["--sub", "x", "--ns", "garage"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--ttl"), "no default life");
}

#[test]
fn a_serving_peer_with_a_secret_takes_only_a_hello_whose_token_names_the_hellos_namespace() {
    let secret = secret_file();
    let s = serve_with_secret(&secret);
    let (ok, out) = write_as(&s.addr, &[]);
    assert!(!ok && out.contains("refused: auth"), "no token: {out}");
    let other = token_file(&mint(&secret, "garage"));
    let (ok, out) = write_as(&s.addr, &["--token-file", other.to_str().unwrap()]);   // the hello says graphlink-cli, the token says garage
    assert!(!ok && out.contains("refused: auth"), "another namespace: {out}");
    let good = token_file(&mint(&secret, "garage"));
    let (ok, out) = write_as(&s.addr, &["--token-file", good.to_str().unwrap(), "--ns", "garage"]);
    assert!(ok, "{out}");
    let forged = token_file(&mint(&secret_file(), "graphlink-cli"));
    let (ok, out) = write_as(&s.addr, &["--token-file", forged.to_str().unwrap()]);
    assert!(!ok && out.contains("refused: auth"), "another secret: {out}");
}

#[test]
fn a_peer_with_no_secret_asks_for_no_token() {
    let s = serve();
    assert!(write_as(&s.addr, &[]).0);
}
