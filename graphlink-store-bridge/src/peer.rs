//! The GraphLink side: this program as a peer of the seat over the shared-memory regions the seat made (docs/store-bridge.md; copied from graphlink-claude's src/peer.rs, which was copied from membrama's: it
//! should move into a shared crate and be used by all of them). The seat makes the control rings `/gl-<name>-ctl` and its cell table `/gl-<name>-sc` and sends its hello first; the peer attaches both, answers, and makes
//! its own cell table `/gl-<name>-pc` (it is the only writer; the seat maps it read-only). Sans-I/O: `poll` is called by whoever owns the thread; nothing here blocks or starts a thread.
//! Unlike the claude peer this one reads nothing of the seat's table and writes only what changed (`publish` skips a value it already shows, so an unchanged cell keeps its revision).
use graphlink_core::frame::{Op, VERSION};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_shm::cells::{CellState, Cells, CellsReader, Reclaim};
use graphlink_shm::Shm;
use serde_json::Value as J;
use std::collections::{HashMap, HashSet};

/// The owner's numbers for the regions (the `peers` fact declares the same ones to the seat).
#[derive(Clone, Debug, PartialEq)]
pub struct Numbers { pub slots: u64, pub key_cap: u32, pub val_cap: u32, pub epoch: u64, pub spins: u64, pub depth: usize }

pub struct Peer {
    ctl: Shm,
    link: Link,
    own: Cells,
    seat: CellsReader,
    revs: HashMap<String, f64>,
    shown: HashMap<String, J>,
    nodes: HashSet<String>,
    reclaim: Reclaim,
    hello_sent: bool,
    spins: u64,
    depth: usize,
}

fn cell_key(eid: &str, prop: &str) -> String { format!("{eid}\u{1}{prop}") }

impl Peer {
    /// The token the seat made for this peer; it goes in the hello (the seat refuses a hello without one). The token names no grants, so the seat does not forward a write of its own to this peer's nodes.
    pub fn with_token(mut self, token: String) -> Peer { self.link = self.link.with_token(token); self }

    pub fn attach(name: &str, ns: &str, n: &Numbers) -> Result<Peer, String> {
        let ctl = Shm::attach(format!("/gl-{name}-ctl"))?;
        let _ = graphlink_shm::remove_region(&format!("/gl-{name}-pc"));   // an old table of a peer that died
        let own = Cells::create(format!("/gl-{name}-pc"), n.slots, n.key_cap, n.val_cap, n.epoch)?;
        let seat = CellsReader::attach(format!("/gl-{name}-sc"))?;
        let link = Link::new(Me { ns: ns.into(), build: format!("graphlink-store-bridge {}", env!("CARGO_PKG_VERSION")), epoch: n.epoch as f64, support: vec![VERSION], initiator: false });
        Ok(Peer { ctl, link, own, seat, revs: HashMap::new(), shown: HashMap::new(), nodes: HashSet::new(), reclaim: Reclaim::new(), hello_sent: false, spins: n.spins, depth: n.depth })
    }

    /// Takes the frames that arrived, answers what the link answers, and returns the events. A write the seat forwards is refused at once (a read-only peer): the refusal goes back in the same turn.
    pub fn poll(&mut self) -> Result<Vec<Event>, String> {
        let mut events = Vec::new();
        while let Some(frame) = self.ctl.recv().map_err(|e| format!("the control rings: {e:?}"))? {
            let (evs, reply) = self.link.receive(frame);
            if let Some(r) = reply { self.ctl.send(&r).map_err(|e| format!("the control rings: {e:?}"))?; }
            events.extend(evs);
        }
        if !self.hello_sent && events.iter().any(|e| matches!(e, Event::Opened { .. })) {
            let hello = self.link.open();
            self.ctl.send(&hello).map_err(|e| format!("the control rings: {e:?}"))?;
            self.hello_sent = true;
        }
        let refusals: Vec<Op> = events.iter().filter_map(|e| match e {
            Event::Op(Op::Wr { wid, id, .. }) => Some(Op::Res { wid: wid.clone(), refused: Some(format!("{id} is read-only: it is a window of a store that the seat cannot write to")) }),
            _ => None,
        }).collect();
        if !refusals.is_empty() && self.link.is_open() {
            let frame = self.link.send(refusals)?;
            self.ctl.send(&frame).map_err(|e| format!("the control rings: {e:?}"))?;
        }
        Ok(events)
    }

    pub fn is_open(&self) -> bool { self.link.is_open() }
    /// Whether the seat's end of the control rings has been dropped (the seat stopped): the peer should end.
    pub fn is_closed(&self) -> bool { self.ctl.is_closed() }

    /// Writes one cell of this peer's table when its value is not the one already shown. The first cell of a node also makes the node's own cell (the node's id and an empty prop), which the seat's driver reads first.
    /// On an error nothing is recorded as shown, so the next turn tries again.
    pub fn publish(&mut self, eid: &str, prop: &str, value: &J) -> Result<(), String> {
        let key = cell_key(eid, prop);
        if self.shown.get(&key) == Some(value) { return Ok(()); }
        if !self.nodes.contains(eid) {
            self.put_cell(&cell_key(eid, ""), &J::Bool(true))?;
            self.nodes.insert(eid.to_string());
        }
        self.put_cell(&key, value)?;
        self.shown.insert(key, value.clone());
        Ok(())
    }

    fn put_cell(&mut self, key: &str, value: &J) -> Result<(), String> {
        let rev = self.revs.get(key).copied().unwrap_or(0.0) + 1.0;
        self.own.put(key, rev, value)?;
        self.revs.insert(key.to_string(), rev);
        Ok(())
    }

    /// A node this peer owns goes away: the node's own cell and every cell of it become Gone, and their slots can be reused once the seat has acknowledged.
    pub fn retire(&mut self, eid: &str) -> Result<(), String> {
        if !self.nodes.contains(eid) { return Ok(()); }
        let prefix = cell_key(eid, "");
        let mut keys: Vec<String> = self.revs.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        keys.sort();
        for k in keys {
            let rev = self.revs[&k] + 1.0;
            self.own.mark(&k, rev, CellState::Gone, "")?;
            self.revs.insert(k.clone(), rev);
            self.shown.remove(&k);
        }
        self.nodes.remove(eid);
        Ok(())
    }

    /// The ids of the nodes this peer shows now.
    pub fn nodes(&self) -> Vec<String> { let mut v: Vec<String> = self.nodes.iter().cloned().collect(); v.sort(); v }

    /// Before the turn: the count of the seat's removals, to acknowledge after it.
    pub fn before(&self) -> u64 { Reclaim::before(&self.seat) }
    /// After the turn: say that the seat's removals up to `seen` have been seen, and reuse the slots of this table the seat has acknowledged.
    pub fn after(&mut self, seen: u64) -> Result<(), String> { self.reclaim.after(seen, &self.seat, &mut self.own, self.spins, self.depth) }
}
