//! The bridge, toward a seat over shared memory and toward a remote peer over TCP. The tests play the seat as the seat's peer driver does: make the control rings and the seat's cell table, run a
//! Link over the rings as the initiator, and read the bridge's cell table; the remote is the `graphlink` tool serving a file of nodes (built by the same workspace: `cargo test --workspace`).
#![cfg(unix)]
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use graphlink_core::frame::{Op, QueryBy, Target, VERSION};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_shm::cells::{Cells, CellState, CellsReader};
use graphlink_shm::Shm;
use serde_json::{json, Value as J};

const BRIDGE: &str = env!("CARGO_BIN_EXE_graphlink-bridge");
const SPINS: u64 = 1000;
const DEPTH: usize = 8;

fn cli_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(BRIDGE);
    p.set_file_name("graphlink");
    p
}

struct Kill(Child);
impl Drop for Kill { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }

fn unique() -> u64 { static N: AtomicU64 = AtomicU64::new(0); N.fetch_add(1, Ordering::SeqCst) }
/// a peer name short enough for a region name (the longest is `/gl-<name>-ctl`, 31 bytes at most)
fn peer_name() -> String { format!("b{}x{}", std::process::id() % 100000, unique()) }

/// removes the region names of a peer when the test ends
struct Names(String);
impl Drop for Names { fn drop(&mut self) { for part in ["ctl", "sc", "pc"] { let _ = graphlink_shm::remove_region(&format!("/gl-{}-{part}", self.0)); } } }

struct Serve { _child: Kill, addr: String }
fn nodes_file() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("graphlink-bridge-{}-{}.json", std::process::id(), unique()));
    std::fs::write(&p, json!({"kitchen::Lamp:desk": {"power": "off", "level": 3}, "hall::Door:front": {"locked": true}}).to_string()).unwrap();
    p
}
fn serve() -> Serve {
    let mut child = Command::new(cli_bin()).args(["serve", "127.0.0.1:0", "--ns", "kitchen", "--nodes"]).arg(nodes_file()).stdout(Stdio::piped()).spawn()
        .expect("the `graphlink` tool is not built: run `cargo test --workspace`");
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    Serve { addr: line.trim().strip_prefix("listening ").unwrap().to_string(), _child: Kill(child) }
}

/// the seat's end: the control rings and the seat's cell table made, a Link as the initiator, and the bridge's cell table to read once the bridge has made it
struct Seat { _names: Names, name: String, ctl: Shm, _sc: Cells, link: Link, events: Vec<Event>, bridge: Option<Kill>, pc: Option<CellsReader> }
impl Seat {
    fn start(remote: &Serve) -> Seat {
        let name = peer_name();
        let names = Names(name.clone());
        let mut ctl = Shm::create(format!("/gl-{name}-ctl"), 4096).unwrap();
        let sc = Cells::create(format!("/gl-{name}-sc"), 16, 64, 256, 1).unwrap();
        let mut link = Link::new(Me { ns: "membrane".into(), build: "t".into(), epoch: 1.0, support: vec![VERSION], initiator: true });
        ctl.send(&link.open()).unwrap();
        let bridge = Command::new(BRIDGE)
            .args(["connect", &remote.addr, "--peer", &name, "--slots", "64", "--key-cap", "128", "--val-cap", "1024", "--epoch", "1"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
        Seat { _names: names, name, ctl, _sc: sc, link, events: vec![], bridge: Some(Kill(bridge)), pc: None }
    }
    fn send(&mut self, ops: Vec<Op>) { let f = self.link.send(ops).expect("link open"); self.ctl.send(&f).unwrap(); }
    fn until(&mut self, ms: u64, done: impl Fn(&mut Seat) -> bool) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            while let Ok(Some(f)) = self.ctl.recv() {
                let (ev, reply) = self.link.receive(f);
                self.events.extend(ev);
                if let Some(r) = reply { self.ctl.send(&r).unwrap(); }
            }
            if self.pc.is_none() { self.pc = CellsReader::attach(format!("/gl-{}-pc", self.name)).ok(); }
            if done(self) { return true; }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
    fn cell(&self, key: &str) -> Option<J> {
        let c = self.pc.as_ref()?.get(key, SPINS, DEPTH).ok()??;
        if c.state == CellState::Live { Some(c.value) } else { None }
    }
    fn ops(&self) -> Vec<Op> { self.events.iter().filter_map(|e| if let Event::Op(o) = e { Some(o.clone()) } else { None }).collect() }
    /// what the bridge said on its standard error, for a failure message (this ends the bridge)
    fn stderr(&mut self) -> String {
        let mut s = String::new();
        if let Some(b) = self.bridge.as_mut() { let _ = b.0.kill(); let _ = b.0.wait(); if let Some(e) = b.0.stderr.as_mut() { let _ = e.read_to_string(&mut s); } }
        s
    }
}
fn opened(s: &mut Seat) -> bool { s.events.iter().any(|e| matches!(e, Event::Opened { .. })) }
fn key(id: &str, prop: &str) -> String { format!("{id}\u{1}{prop}") }

#[test]
fn the_seat_sees_the_bridge_as_the_remote_and_the_remotes_values_arrive_as_cells() {
    let remote = serve();
    let mut seat = Seat::start(&remote);
    assert!(seat.until(10000, opened), "the bridge never opened toward the seat: {}", seat.stderr());
    // the bridge says who the remote is: the seat sees the remote's own namespace
    let ns = seat.events.iter().find_map(|e| if let Event::Opened { peer_ns, .. } = e { Some(peer_ns.clone()) } else { None }).unwrap();
    assert_eq!(ns, "kitchen");
    seat.send(vec![Op::Sub { target: Target::Id("kitchen::Lamp:desk".into()), props: Some(vec!["power".into()]), limit: None }]);
    assert!(seat.until(10000, |s| s.cell(&key("kitchen::Lamp:desk", "power")) == Some(json!("off"))), "no cell: {}", seat.stderr());
    assert_eq!(seat.cell(&key("kitchen::Lamp:desk", "")), Some(json!(true)), "the node's own cell says it is live");
    assert_eq!(seat.cell(&key("kitchen::Lamp:desk", "level")), None, "only the props asked for are written");
}

#[test]
fn a_write_goes_to_the_remote_and_its_receipt_and_the_new_value_come_back() {
    let remote = serve();
    let mut seat = Seat::start(&remote);
    assert!(seat.until(10000, opened));
    seat.send(vec![Op::Sub { target: Target::Id("kitchen::Lamp:desk".into()), props: Some(vec!["power".into()]), limit: None }]);
    assert!(seat.until(10000, |s| s.cell(&key("kitchen::Lamp:desk", "power")).is_some()));
    seat.send(vec![Op::Wr { wid: "w1".into(), id: "kitchen::Lamp:desk".into(), prop: "power".into(), value: json!("on") }]);
    assert!(seat.until(10000, |s| s.ops().iter().any(|o| matches!(o, Op::Res { wid, refused: None } if wid == "w1"))), "no receipt: {}", seat.stderr());
    assert!(seat.until(10000, |s| s.cell(&key("kitchen::Lamp:desk", "power")) == Some(json!("on"))), "the pushed value never reached the cell");
}

#[test]
fn a_node_outside_the_remotes_namespace_never_becomes_a_cell() {
    let remote = serve();   // owns kitchen::..., its file also holds hall::Door:front
    let mut seat = Seat::start(&remote);
    assert!(seat.until(10000, opened));
    seat.send(vec![Op::Sub { target: Target::Prefix(String::new()), props: None, limit: None }]);
    assert!(seat.until(10000, |s| s.cell(&key("kitchen::Lamp:desk", "power")).is_some()));
    seat.until(500, |_| false);
    assert_eq!(seat.cell(&key("hall::Door:front", "locked")), None, "the bridge let a foreign node through");
    assert_eq!(seat.cell(&key("hall::Door:front", "")), None);
}

#[test]
fn rows_of_a_page_outside_the_remotes_namespace_are_dropped_and_the_page_reaches_the_seat() {
    let remote = serve();
    let mut seat = Seat::start(&remote);
    assert!(seat.until(10000, opened));
    seat.send(vec![Op::Query { qid: "q".into(), by: QueryBy::Prefix(String::new()), after: None, limit: None }]);
    assert!(seat.until(10000, |s| s.ops().iter().any(|o| matches!(o, Op::Page { .. }))), "{}", seat.stderr());
    let rows: Vec<J> = seat.ops().iter().filter_map(|o| if let Op::Page { rows, .. } = o { Some(rows.clone()) } else { None }).flatten().collect();
    assert_eq!(rows, vec![json!({"id": "kitchen::Lamp:desk"})], "{rows:?}");
}

#[test]
fn a_node_the_remote_does_not_have_becomes_a_gone_cell() {
    let remote = serve();
    let mut seat = Seat::start(&remote);
    assert!(seat.until(10000, opened));
    seat.send(vec![Op::Read { id: "kitchen::Lamp:nowhere".into(), props: vec![], rid: "r1".into() }]);
    let k = key("kitchen::Lamp:nowhere", "");
    let gone = |s: &mut Seat| s.pc.as_ref().and_then(|p| p.get(&k, SPINS, DEPTH).ok().flatten()).map_or(false, |c| c.state == CellState::Gone);
    assert!(seat.until(10000, gone), "{}", seat.stderr());
}

#[test]
fn when_the_remote_goes_away_the_bridge_tells_the_seat_and_ends() {
    let remote = serve();
    let mut seat = Seat::start(&remote);
    assert!(seat.until(10000, opened));
    drop(remote);
    assert!(seat.until(10000, |s| s.ctl.is_closed() || s.events.iter().any(|e| matches!(e, Event::Closed { .. }))), "the seat was never told");
    let end = Instant::now() + Duration::from_secs(10);
    let mut b = seat.bridge.take().unwrap();
    while Instant::now() < end { if b.0.try_wait().unwrap().is_some() { return; } std::thread::sleep(Duration::from_millis(10)); }
    panic!("the bridge did not end when the remote went");
}

#[test]
fn when_the_seat_closes_its_end_the_bridge_ends() {
    let remote = serve();
    let mut seat = Seat::start(&remote);
    assert!(seat.until(10000, opened));
    let mut b = seat.bridge.take().unwrap();
    drop(seat);   // the seat's ends close
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end { if b.0.try_wait().unwrap().is_some() { return; } std::thread::sleep(Duration::from_millis(10)); }
    panic!("the bridge did not end when the seat closed its end");
}

#[test]
fn a_missing_option_is_named_and_exits_2() {
    let o = Command::new(BRIDGE).args(["connect", "127.0.0.1:1", "--peer", "x"]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--slots"), "{}", String::from_utf8_lossy(&o.stderr));
}
