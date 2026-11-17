//! graphlink-bridge: TCP outward, shared memory inward. It joins a seat to a remote peer so that the seat needs no socket: to the seat the bridge is an ordinary local peer (the control rings and
//! cell table a seat makes for a declared peer), and what the bridge says about itself is the remote's own namespace.
//!
//!   graphlink-bridge connect <addr> --peer <name> --slots <n> --key-cap <n> --val-cap <n> --epoch <n>
//!
//! The seat makes `/gl-<name>-ctl` (control rings) and `/gl-<name>-sc` (its own cell table) first; the bridge attaches the rings, connects to `<addr>`, learns the remote's namespace from its
//! `hello`, makes `/gl-<name>-pc` (the bridge's cell table: the remote's values, as the seat reads them) and answers the seat's `hello` with that namespace. Toward the remote the bridge says the
//! seat's namespace. What the seat asks (`sub`, `unsub`, `read`, `query`, `wr`) goes to the remote; what the remote answers goes back: values become cells, receipts and pages go over the rings.
//! A node outside the remote's namespace is dropped both ways. The cell table's slot count, key and value capacities and epoch are the owner's numbers: no defaults.
//! When either end goes, the bridge says `bye` to the other and ends.
use std::collections::BTreeMap;
use std::process::exit;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use graphlink_core::frame::{Frame, Op, ValBody, VERSION};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_net::Tcp;
use graphlink_shm::cells::{Cells, CellState};
use graphlink_shm::Shm;
use serde_json::{json, Value as J};

const OPTIONS: [&str; 5] = ["--peer", "--slots", "--key-cap", "--val-cap", "--epoch"];

fn usage(msg: &str) -> ! {
    eprintln!("{msg}\nusage:\n  graphlink-bridge connect <addr> --peer <name> --slots <n> --key-cap <n> --val-cap <n> --epoch <n>");
    exit(2)
}

struct Config { addr: String, peer: String, slots: u64, key_cap: u32, val_cap: u32, epoch: u64 }

fn parse(args: &[String]) -> Config {
    if args.first().map(String::as_str) != Some("connect") { usage("the command is `connect`"); }
    let addr = args.get(1).filter(|a| !a.starts_with("--")).unwrap_or_else(|| usage("connect needs an address")).clone();
    let mut given: BTreeMap<&str, String> = BTreeMap::new();
    let mut i = 2;
    while i < args.len() {
        let Some(opt) = OPTIONS.iter().find(|o| **o == args[i]) else { usage(&format!("unknown option {}", args[i])) };
        given.insert(opt, args.get(i + 1).unwrap_or_else(|| usage(&format!("{opt} needs a value"))).clone());
        i += 2;
    }
    let missing: Vec<&str> = OPTIONS.iter().copied().filter(|o| !given.contains_key(o)).collect();
    if !missing.is_empty() { usage(&format!("missing: {} (the numbers are the owner's: there are no defaults)", missing.join(", "))); }
    fn num<T: std::str::FromStr>(g: &BTreeMap<&str, String>, o: &str) -> T { g[o].parse().unwrap_or_else(|_| usage(&format!("{o} is not a number: {}", g[o]))) }
    Config { addr, peer: given["--peer"].clone(), slots: num(&given, "--slots"), key_cap: num(&given, "--key-cap"), val_cap: num(&given, "--val-cap"), epoch: num(&given, "--epoch") }
}

fn fail(msg: &str) -> ! { eprintln!("graphlink-bridge: {msg}"); exit(1) }

fn me(ns: &str, initiator: bool) -> Me {
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    Me { ns: ns.into(), build: format!("graphlink-bridge {}", env!("CARGO_PKG_VERSION")), epoch, support: vec![VERSION], initiator }
}

fn nap() { std::thread::sleep(Duration::from_millis(5)); }

/// the remote's cell table as the seat reads it: one cell per prop and one for the node (`<id>` U+0001 `<prop>`, the node's own with an empty prop)
struct Published { cells: Cells, last: BTreeMap<String, (f64, J, CellState)> }

impl Published {
    fn put(&mut self, key: String, rev: f64, state: CellState, value: J) {
        let (old, same) = match self.last.get(&key) { Some((r, v, s)) => (*r, *s == state && *v == value), None => (0.0, false) };
        if same { return; }
        let rev = if rev > old { rev } else { old + 1.0 };   // a revision only grows
        let wrote = match state {
            CellState::Live => self.cells.put(&key, rev, &value),
            CellState::Gone => self.cells.mark(&key, rev, CellState::Gone, ""),
            CellState::Refused => self.cells.mark(&key, rev, CellState::Refused, value.as_str().unwrap_or("")),
        };
        if let Err(why) = wrote {
            // a value that does not fit its slot becomes a cell that says so, never an old value left in place
            if self.cells.mark(&key, rev, CellState::Refused, &why).is_err() { eprintln!("graphlink-bridge: cannot publish {key:?}: {why}"); }
        }
        self.last.insert(key, (rev, value, state));
    }

    fn val(&mut self, id: &str, r: f64, body: ValBody) {
        let ekey = format!("{id}\u{1}");
        match body {
            ValBody::Props(J::Object(props)) => {
                self.put(ekey, r, CellState::Live, json!(true));
                for (k, v) in props { self.put(format!("{id}\u{1}{k}"), r, CellState::Live, v); }
            }
            ValBody::Props(_) => {}
            ValBody::Gone => self.put(ekey, r, CellState::Gone, J::Null),
            ValBody::Refused(why) => self.put(ekey, r, CellState::Refused, json!(why)),
        }
    }
}

/// what the remote said: a value of a node of its namespace becomes cells; a receipt and a page (its rows of its namespace only) go to the seat; the end of the link ends the bridge
fn from_remote(events: Vec<Event>, remote_ns: &str, table: &mut Published, to_seat: &mut Vec<Op>) -> bool {
    let mine = |id: &str| id.split_once("::").map_or(false, |(n, _)| n == remote_ns);
    let mut end = false;
    for ev in events {
        match ev {
            Event::Op(Op::Val { id, r, body, .. }) => { if mine(&id) { table.val(&id, r, body); } }
            Event::Op(Op::Res { wid, refused }) => to_seat.push(Op::Res { wid, refused }),
            Event::Op(Op::Page { qid, rows, next, total }) => {
                let rows = rows.into_iter().filter(|r| r.get("id").and_then(J::as_str).map_or(false, mine)).collect();
                to_seat.push(Op::Page { qid, rows, next, total });
            }
            Event::Failed { .. } | Event::Closed { .. } => end = true,
            _ => {}
        }
    }
    end
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = parse(&args);

    // the seat's end first: its rings, and the hello it has already sent
    let mut ctl = Shm::attach(format!("/gl-{}-ctl", cfg.peer)).unwrap_or_else(|e| fail(&format!("cannot attach the seat's control rings: {e}")));
    let mut tcp = Tcp::connect(cfg.addr.as_str()).unwrap_or_else(|e| fail(&format!("cannot connect to {}: {e}", cfg.addr)));
    let seat_hello: Frame = loop {
        match ctl.recv() { Ok(Some(f)) => break f, Ok(None) => { if ctl.is_closed() { exit(0); } nap(); } Err(e) => fail(&format!("the seat's rings: {e}")) }
    };
    let seat_ns = seat_hello.ops.iter().find_map(|o| if let Op::Hello { ns, .. } = o { Some(ns.clone()) } else { None }).unwrap_or_else(|| fail("the seat's first frame is not a hello"));

    // toward the remote: say the seat's namespace, learn the remote's
    let mut remote = Link::new(me(&seat_ns, true));
    let _ = tcp.send(&remote.open());
    let mut early: Vec<Event> = vec![];
    let remote_ns = loop {
        match tcp.recv() {
            Ok(Some(f)) => {
                let (events, reply) = remote.receive(f);
                if let Some(r) = reply { let _ = tcp.send(&r); }
                if let Some(Event::Failed { why }) = events.iter().find(|e| matches!(e, Event::Failed { .. })) { fail(&format!("the remote refused: {why}")); }
                let ns = events.iter().find_map(|e| if let Event::Opened { peer_ns, .. } = e { Some(peer_ns.clone()) } else { None });
                early.extend(events.into_iter().filter(|e| matches!(e, Event::Op(_))));
                if let Some(ns) = ns { break ns; }
            }
            Ok(None) => { if tcp.is_closed() || ctl.is_closed() { exit(0); } nap(); }
            Err(e) => fail(&e),
        }
    };

    // toward the seat: the remote's table, then the answer to the seat's hello in the remote's name
    let cells = Cells::create(format!("/gl-{}-pc", cfg.peer), cfg.slots, cfg.key_cap, cfg.val_cap, cfg.epoch).unwrap_or_else(|e| fail(&format!("cannot make the cell table: {e}")));
    let mut table = Published { cells, last: BTreeMap::new() };
    let mut seat = Link::new(me(&remote_ns, false));
    let _ = ctl.send(&seat.open());
    let (events, reply) = seat.receive(seat_hello);
    if let Some(r) = reply { let _ = ctl.send(&r); }
    let mut to_remote: Vec<Op> = vec![];
    let mut to_seat: Vec<Op> = vec![];
    let mut done = events.iter().any(|e| matches!(e, Event::Failed { .. } | Event::Closed { .. }));
    if from_remote(early, &remote_ns, &mut table, &mut to_seat) { done = true; }

    while !done {
        // what the seat asks
        while let Ok(Some(f)) = ctl.recv() {
            let (events, reply) = seat.receive(f);
            if let Some(r) = reply { let _ = ctl.send(&r); }
            for ev in events {
                match ev {
                    Event::Op(Op::Wr { wid, id, prop, value }) => {
                        if id.split_once("::").map_or(false, |(n, _)| n == remote_ns) { to_remote.push(Op::Wr { wid, id, prop, value }); }
                        else { to_seat.push(Op::Res { wid, refused: Some(format!("not a node of {remote_ns}: {id}")) }); }
                    }
                    Event::Op(op @ (Op::Sub { .. } | Op::Unsub { .. } | Op::Read { .. } | Op::Query { .. } | Op::Resync { .. } | Op::Pause { .. })) => to_remote.push(op),
                    Event::Failed { .. } | Event::Closed { .. } => done = true,
                    _ => {}
                }
            }
        }
        if ctl.is_closed() { done = true; }
        // what the remote says
        while let Ok(Some(f)) = tcp.recv() {
            let (events, reply) = remote.receive(f);
            if let Some(r) = reply { let _ = tcp.send(&r); }
            if from_remote(events, &remote_ns, &mut table, &mut to_seat) { done = true; }
        }
        if tcp.is_closed() { done = true; }
        if !to_remote.is_empty() { if let Ok(f) = remote.send(std::mem::take(&mut to_remote)) { let _ = tcp.send(&f); } }
        if !to_seat.is_empty() { if let Ok(f) = seat.send(std::mem::take(&mut to_seat)) { let _ = ctl.send(&f); } }
        if !done { nap(); }
    }

    // either end went: tell the other and end
    if let Ok(f) = remote.send(vec![Op::Bye { why: "the seat went".into() }]) { let _ = tcp.send(&f); }
    if let Ok(f) = seat.send(vec![Op::Bye { why: "the remote went".into() }]) { let _ = ctl.send(&f); }
    ctl.flush();
}
