//! The seat as the tests play it: it makes the control rings and its cell table first and sends its hello before the peer starts, the way the seat's driver does (the same rig as graphlink-claude's tests).
#![allow(dead_code)]
use graphlink_core::frame::{Op, VERSION};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_shm::cells::{CellState, Cells, CellsReader};
use graphlink_shm::{remove_region, Shm};
use graphlink_store_bridge::peer::{Numbers, Peer};
use serde_json::Value as J;

pub const NUMBERS: Numbers = Numbers { slots: 256, key_cap: 128, val_cap: 4096, epoch: 1, spins: 1_000_000, depth: 32 };
pub const NS: &str = "lib";
pub fn key(eid: &str, prop: &str) -> String { format!("{eid}\u{1}{prop}") }

pub struct FakeSeat { pub name: String, pub ctl: Shm, pub table: Cells, pub link: Link, pub events: Vec<Event> }

impl FakeSeat {
    pub fn start(tag: &str) -> FakeSeat {
        let name = format!("sb{}{}", std::process::id() % 100000, tag);
        for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{name}-{r}")); }
        let mut ctl = Shm::create(format!("/gl-{name}-ctl"), 65536).expect("rings");
        let table = Cells::create(format!("/gl-{name}-sc"), NUMBERS.slots, NUMBERS.key_cap, NUMBERS.val_cap, 1).expect("seat table");
        let mut link = Link::new(Me { ns: "membrane".into(), build: "fake seat".into(), epoch: 1.0, support: vec![VERSION], initiator: true });
        ctl.send(&link.open()).unwrap();
        FakeSeat { name, ctl, table, link, events: Vec::new() }
    }
    pub fn peer(&self) -> Peer { Peer::attach(&self.name, NS, &NUMBERS).expect("peer attaches") }
    /// the seat's link takes whatever the peer sent and answers it
    pub fn pump(&mut self) {
        while let Some(f) = self.ctl.recv().unwrap() {
            let (events, reply) = self.link.receive(f);
            if let Some(r) = reply { self.ctl.send(&r).unwrap(); }
            self.events.extend(events);
        }
    }
    /// the seat sends a write for a node of the peer's namespace
    pub fn write(&mut self, wid: &str, id: &str, prop: &str, value: J) {
        let frame = self.link.send(vec![Op::Wr { wid: wid.into(), id: id.into(), prop: prop.into(), value }]).expect("the link is open");
        self.ctl.send(&frame).unwrap();
    }
    pub fn reader(&self) -> CellsReader { CellsReader::attach(format!("/gl-{}-pc", self.name)).expect("the peer's table") }
}
impl Drop for FakeSeat { fn drop(&mut self) { for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{}-{r}", self.name)); } } }

pub fn cell(r: &CellsReader, eid: &str, prop: &str) -> Option<J> {
    r.get(&key(eid, prop), 1_000_000, 32).unwrap().filter(|c| c.state == CellState::Live).map(|c| c.value)
}
pub fn rev(r: &CellsReader, eid: &str, prop: &str) -> Option<f64> { r.get(&key(eid, prop), 1_000_000, 32).unwrap().map(|c| c.rev) }
pub fn state(r: &CellsReader, eid: &str, prop: &str) -> Option<CellState> { r.get(&key(eid, prop), 1_000_000, 32).unwrap().map(|c| c.state) }
