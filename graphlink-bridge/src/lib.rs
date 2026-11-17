//! graphlink-bridge: TCP outward, shared memory inward. It joins a seat to a remote peer so that the seat needs no socket: to the seat the bridge is an ordinary local peer (the control rings and
//! cell table a seat makes for a declared peer), and what the bridge says about itself is the remote's own namespace.
//!
//!   graphlink-bridge connect <addr> --peer <name> --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n>
//!   (optional: --token-file <f> the token the seat minted for this peer, put in the hello to the seat; --remote-token-file <f> the token the remote asks for, put in the hello to the remote;
//!    --secret-file <f> check every remote hello: its token must verify and name the remote's namespace; required with --allow-remote)
//!   graphlink-bridge listen  <addr> --peer <name> ... the same ... [--allow-remote]   wait for the remote to connect (a loopback address unless --allow-remote)
//!
//! The seat makes `/gl-<name>-ctl` (control rings) and `/gl-<name>-sc` (its own cell table) first; the bridge attaches the rings, connects to `<addr>`, learns the remote's namespace from its
//! `hello`, makes `/gl-<name>-pc` (the bridge's cell table: the remote's values, as the seat reads them) and answers the seat's `hello` with that namespace. Toward the remote the bridge says the
//! seat's namespace. What the seat asks (`sub`, `unsub`, `read`, `query`, `wr`) goes to the remote; what the remote answers goes back: values become cells, receipts and pages go over the rings.
//! A node outside the remote's namespace is dropped both ways. The other way round, a remote's `sub` of a node of the seat's namespace goes to the seat over the rings, the seat answers it by writing
//! its own cell table, and the bridge reads those cells and sends the remote a `val` for each new one (`gone` for a node that is gone); a remote's `wr` goes to the seat and the seat's receipt back.
//! The cell table's slot count, key and value capacities, epoch, and the reader's spin and depth limits are the owner's numbers: no defaults. After `listen` says `listening <addr>` the first remote
//! to connect is the one the bridge serves.
//! When either end goes, the bridge says `bye` to the other and ends. `run()` is the whole program (it reads the process's arguments and exits the process): the `graphlink-bridge` binary calls it, and so can
//! a host that wants to ship the bridge as its own binary.
use std::collections::{BTreeMap, BTreeSet};
use std::process::exit;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use graphlink_auth::check_hello;
use graphlink_core::frame::{Frame, Op, Target, ValBody, VERSION};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_net::{Listener, Tcp};
use graphlink_shm::cells::{Cells, CellsReader, CellState};
use graphlink_shm::Shm;
use serde_json::{json, Value as J};

const OPTIONS: [&str; 7] = ["--peer", "--slots", "--key-cap", "--val-cap", "--epoch", "--spins", "--depth"];

fn usage(msg: &str) -> ! {
    eprintln!("{msg}\nusage:\n  graphlink-bridge connect|listen <addr> --peer <name> --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n> [--allow-remote (listen)]");
    exit(2)
}

struct Config { token_file: Option<String>, remote_token_file: Option<String>, secret_file: Option<String>, listen: bool, addr: String, peer: String, slots: u64, key_cap: u32, val_cap: u32, epoch: u64, spins: u64, depth: usize }

fn parse(args: &[String]) -> Config {
    let listen = match args.first().map(String::as_str) { Some("connect") => false, Some("listen") => true, _ => usage("the command is `connect` or `listen`") };
    let addr = args.get(1).filter(|a| !a.starts_with("--")).unwrap_or_else(|| usage("connect and listen need an address")).clone();
    let mut allow_remote = false;
    let mut optional: BTreeMap<&str, String> = BTreeMap::new();
    let mut given: BTreeMap<&str, String> = BTreeMap::new();
    let mut i = 2;
    while i < args.len() {
        if listen && args[i] == "--allow-remote" { allow_remote = true; i += 1; continue; }
        if let Some(o) = ["--token-file", "--remote-token-file", "--secret-file"].iter().find(|o| **o == args[i]) {
            optional.insert(o, args.get(i + 1).unwrap_or_else(|| usage(&format!("{o} needs a value"))).clone());
            i += 2;
            continue;
        }
        let Some(opt) = OPTIONS.iter().find(|o| **o == args[i]) else { usage(&format!("unknown option {}", args[i])) };
        given.insert(opt, args.get(i + 1).unwrap_or_else(|| usage(&format!("{opt} needs a value"))).clone());
        i += 2;
    }
    let missing: Vec<&str> = OPTIONS.iter().copied().filter(|o| !given.contains_key(o)).collect();
    if !missing.is_empty() { usage(&format!("missing: {} (the numbers are the owner's: there are no defaults)", missing.join(", "))); }
    fn num<T: std::str::FromStr>(g: &BTreeMap<&str, String>, o: &str) -> T { g[o].parse().unwrap_or_else(|_| usage(&format!("{o} is not a number: {}", g[o]))) }
    if listen && !allow_remote {
        use std::net::ToSocketAddrs;
        let loopback = addr.to_socket_addrs().map(|mut a| a.all(|x| x.ip().is_loopback())).unwrap_or(false);
        if !loopback { usage(&format!("{addr} is not a loopback address: a bridge listens on one only with --allow-remote")); }
    }
    if allow_remote && !optional.contains_key("--secret-file") { usage("--allow-remote needs --secret-file: a bridge open beyond loopback must check every remote's token"); }
    Config { token_file: optional.get("--token-file").cloned(), remote_token_file: optional.get("--remote-token-file").cloned(), secret_file: optional.get("--secret-file").cloned(), listen, addr, peer: given["--peer"].clone(), slots: num(&given, "--slots"), key_cap: num(&given, "--key-cap"), val_cap: num(&given, "--val-cap"), epoch: num(&given, "--epoch"), spins: num(&given, "--spins"), depth: num(&given, "--depth") }
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
fn from_remote(events: Vec<Event>, remote_ns: &str, seat_ns: &str, table: &mut Published, subs: &mut Subs, to_seat: &mut Vec<Op>, to_remote: &mut Vec<Op>) -> bool {
    let mine = |id: &str| id.split_once("::").map_or(false, |(n, _)| n == remote_ns);
    let seats = |id: &str| id.split_once("::").map_or(false, |(n, _)| n == seat_ns);
    let mut end = false;
    for ev in events {
        match ev {
            // the remote reads the seat: only nodes of the seat's namespace go to the seat
            Event::Op(Op::Sub { target, props, limit }) => {
                if matches!(&target, Target::Id(i) if !seats(i)) { continue; }
                subs.add(target.clone(), props.clone());
                to_seat.push(Op::Sub { target, props, limit });
            }
            Event::Op(Op::Unsub { target }) => { subs.remove(&target); to_seat.push(Op::Unsub { target }); }
            Event::Op(Op::Wr { wid, id, prop, value }) => {
                if seats(&id) { to_seat.push(Op::Wr { wid, id, prop, value }); }
                else { to_remote.push(Op::Res { wid, refused: Some(format!("not a node of {seat_ns}: {id}")) }); }
            }
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

/// what the remote subscribed to, and which cell revisions of the seat's table it has been sent
#[derive(Default)]
struct Subs { list: Vec<(Target, Option<Vec<String>>)>, sent: BTreeMap<String, f64>, down: BTreeSet<String> }

fn covers(t: &Target, id: &str) -> bool { match t { Target::Id(i) => i == id, Target::Prefix(p) => id.starts_with(p.as_str()) } }

impl Subs {
    fn add(&mut self, target: Target, props: Option<Vec<String>>) {
        self.forget(&target);   // a subscription again sends the values again
        self.list.retain(|(t, _)| *t != target);
        self.list.push((target, props));
    }
    fn remove(&mut self, target: &Target) { self.list.retain(|(t, _)| t != target); self.forget(target); }
    fn forget(&mut self, target: &Target) {
        self.sent.retain(|k, _| !covers(target, k.split('\u{1}').next().unwrap_or("")));
        self.down.retain(|id| !covers(target, id));
    }

    /// the seat's cells for what the remote subscribed to: a `val` for each node with a cell newer than the last one sent, `gone` (or `refused`) once for a node that is not live
    fn relay(&mut self, reader: &CellsReader, spins: u64, depth: usize, out: &mut Vec<Op>) {
        let mut ids: BTreeSet<String> = BTreeSet::new();
        for (t, _) in &self.list {
            match t {
                Target::Id(i) => { ids.insert(i.clone()); }
                Target::Prefix(p) => { if let Ok(rows) = reader.scan(p, spins, depth) { ids.extend(rows.into_iter().filter_map(|(k, _)| k.split_once('\u{1}').map(|(id, _)| id.to_string()))); } }
            }
        }
        for id in ids {
            match reader.get(&format!("{id}\u{1}"), spins, depth) {
                Ok(Some(c)) if c.state != CellState::Live => {
                    if self.down.insert(id.clone()) {
                        let body = if c.state == CellState::Gone { ValBody::Gone } else { ValBody::Refused(c.value.as_str().unwrap_or("refused").to_string()) };
                        out.push(Op::Val { id, r: c.rev, rid: None, body });
                    }
                    continue;
                }
                Ok(Some(_)) => { if self.down.remove(&id) { self.sent.retain(|k, _| !k.starts_with(&format!("{id}\u{1}"))); } }
                Ok(None) => continue,
                Err(e) => { eprintln!("graphlink-bridge: cell of {id}: {e}"); continue; }
            }
            let named: Vec<&Option<Vec<String>>> = self.list.iter().filter(|(t, _)| covers(t, &id)).map(|(_, p)| p).collect();
            let all = named.iter().any(|p| p.is_none());
            let wanted: Vec<&String> = named.iter().filter_map(|p| p.as_ref()).flatten().collect();
            let rows = match reader.scan(&format!("{id}\u{1}"), spins, depth) { Ok(r) => r, Err(e) => { eprintln!("graphlink-bridge: cells of {id}: {e}"); continue } };
            let (mut props, mut rev) = (serde_json::Map::new(), 0f64);
            for (key, c) in rows {
                let Some((_, prop)) = key.split_once('\u{1}').map(|(i, p)| (i.to_string(), p.to_string())) else { continue };
                if prop.is_empty() || c.state != CellState::Live || !(all || wanted.iter().any(|w| w.as_str() == prop)) { continue; }
                if self.sent.get(&key).map_or(true, |r| c.rev > *r) { self.sent.insert(key, c.rev); rev = rev.max(c.rev); props.insert(prop.to_string(), c.value); }
            }
            if !props.is_empty() { out.push(Op::Val { id, r: rev, rid: None, body: ValBody::Props(J::Object(props)) }); }
        }
    }
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = parse(&args);

    // the seat's end first: its rings, and the hello it has already sent
    let mut ctl = Shm::attach(format!("/gl-{}-ctl", cfg.peer)).unwrap_or_else(|e| fail(&format!("cannot attach the seat's control rings: {e}")));
    let seat_hello: Frame = loop {
        match ctl.recv() { Ok(Some(f)) => break f, Ok(None) => { if ctl.is_closed() { exit(0); } nap(); } Err(e) => fail(&format!("the seat's rings: {e}")) }
    };
    let seat_ns = seat_hello.ops.iter().find_map(|o| if let Op::Hello { ns, .. } = o { Some(ns.clone()) } else { None }).unwrap_or_else(|| fail("the seat's first frame is not a hello"));
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("cannot read {path}: {e}"))).trim().to_string();
    let secret: Option<Vec<u8>> = cfg.secret_file.as_deref().map(|p| read(p).into_bytes());
    let remote_token: Option<String> = cfg.remote_token_file.as_deref().map(read);
    let seat_token: Option<String> = cfg.token_file.as_deref().map(read);

    // toward the remote. `listen`: serve the first remote whose hello is good (one that fails its check is told `refused: auth` and dropped, and the bridge listens on); `connect`: dial once.
    let listener = if cfg.listen {
        let l = Listener::bind(cfg.addr.as_str()).unwrap_or_else(|e| fail(&format!("cannot listen on {}: {e}", cfg.addr)));
        println!("listening {}", l.local_addr().unwrap());
        use std::io::Write;
        std::io::stdout().flush().ok();
        Some(l)
    } else { None };
    let (mut tcp, mut remote, remote_ns, early) = loop {
        let mut tcp = match &listener {
            Some(l) => loop {
                match l.accept() { Ok(Some(t)) => break t, Ok(None) => { if ctl.is_closed() { exit(0); } nap(); } Err(e) => fail(&format!("accept: {e}")) }
            },
            None => Tcp::connect(cfg.addr.as_str()).unwrap_or_else(|e| fail(&format!("cannot connect to {}: {e}", cfg.addr))),
        };
        // say the seat's namespace (and the token the owner gave for the remote), learn the remote's
        let mut link = Link::new(me(&seat_ns, !cfg.listen));
        if let Some(t) = &remote_token { link = link.with_token(t.clone()); }
        let _ = tcp.send(&link.open());
        let mut early: Vec<Event> = vec![];
        let verdict: Result<String, String> = loop {
            match tcp.recv() {
                Ok(Some(f)) => {
                    let (events, reply) = link.receive(f);
                    if let Some(r) = reply { let _ = tcp.send(&r); }
                    if let Some(Event::Failed { why }) = events.iter().find(|e| matches!(e, Event::Failed { .. })) { break Err(format!("the remote refused: {why}")); }
                    let opened = events.iter().find_map(|e| if let Event::Opened { peer_ns, peer_token, .. } = e { Some((peer_ns.clone(), peer_token.clone())) } else { None });
                    early.extend(events.into_iter().filter(|e| matches!(e, Event::Op(_) | Event::Closed { .. })));
                    if let Some((ns, token)) = opened {
                        // with a secret the remote's hello must carry a token that verifies and names the hello's namespace (the seat's own is never granted)
                        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
                        match secret.as_deref().map(|s| check_hello(token.as_deref(), &ns, &[seat_ns.as_str()], s, now)) {
                            Some(Err(why)) => { if let Ok(f) = link.send(vec![Op::Bye { why: "refused: auth".into() }]) { let _ = tcp.send(&f); } break Err(format!("hello of {ns:?} refused: {why}")); }
                            _ => break Ok(ns),
                        }
                    }
                }
                Ok(None) => { if tcp.is_closed() || ctl.is_closed() { if ctl.is_closed() { exit(0); } break Err("the remote went before its hello".into()); } nap(); }
                Err(e) => break Err(e),
            }
        };
        match verdict {
            Ok(ns) => break (tcp, link, ns, early),
            Err(why) if cfg.listen => { eprintln!("graphlink-bridge: {why}"); }
            Err(why) => fail(&why),
        }
    };

    // toward the seat: the remote's table, then the answer to the seat's hello in the remote's name
    let cells = Cells::create(format!("/gl-{}-pc", cfg.peer), cfg.slots, cfg.key_cap, cfg.val_cap, cfg.epoch).unwrap_or_else(|e| fail(&format!("cannot make the cell table: {e}")));
    let mut table = Published { cells, last: BTreeMap::new() };
    let reader = CellsReader::attach(format!("/gl-{}-sc", cfg.peer)).unwrap_or_else(|e| fail(&format!("cannot attach the seat's cell table: {e}")));
    let mut subs = Subs::default();
    let mut seat = Link::new(me(&remote_ns, false));
    if let Some(t) = &seat_token { seat = seat.with_token(t.clone()); }
    let _ = ctl.send(&seat.open());
    let (events, reply) = seat.receive(seat_hello);
    if let Some(r) = reply { let _ = ctl.send(&r); }
    let mut to_remote: Vec<Op> = vec![];
    let mut to_seat: Vec<Op> = vec![];
    let mut done = events.iter().any(|e| matches!(e, Event::Failed { .. } | Event::Closed { .. }));
    if from_remote(early, &remote_ns, &seat_ns, &mut table, &mut subs, &mut to_seat, &mut to_remote) { done = true; }

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
                    Event::Op(Op::Res { wid, refused }) => to_remote.push(Op::Res { wid, refused }),
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
            if from_remote(events, &remote_ns, &seat_ns, &mut table, &mut subs, &mut to_seat, &mut to_remote) { done = true; }
        }
        if tcp.is_closed() { done = true; }
        subs.relay(&reader, cfg.spins, cfg.depth, &mut to_remote);
        if !to_remote.is_empty() { if let Ok(f) = remote.send(std::mem::take(&mut to_remote)) { let _ = tcp.send(&f); } }
        if !to_seat.is_empty() { if let Ok(f) = seat.send(std::mem::take(&mut to_seat)) { let _ = ctl.send(&f); } }
        if !done { nap(); }
    }

    // either end went: tell the other and end
    if let Ok(f) = remote.send(vec![Op::Bye { why: "the seat went".into() }]) { let _ = tcp.send(&f); }
    if let Ok(f) = seat.send(vec![Op::Bye { why: "the remote went".into() }]) { let _ = ctl.send(&f); }
    ctl.flush();
}
