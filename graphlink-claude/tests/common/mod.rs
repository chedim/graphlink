//! Shared by the tests that play the seat: it makes the control rings and its cell table first and sends its hello before the peer starts, the way the seat's driver does
//! (the same as membrama's tests/common). The fake `claude` is tests/fake_claude.py.
#![allow(dead_code)]
use graphlink_claude::handler::{Auth, Config, Handler};
use graphlink_claude::peer::{Numbers, Peer};
use graphlink_core::frame::VERSION;
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_shm::cells::{CellState, Cells, CellsReader, Reclaim};
use graphlink_shm::{remove_region, Shm};
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const NUMBERS: Numbers = Numbers { slots: 128, key_cap: 128, val_cap: 4096, epoch: 1, spins: 1_000_000, depth: 32 };
pub const KEY: &str = "sk-test-SECRET-123";
pub fn key(eid: &str, prop: &str) -> String { format!("{eid}\u{1}{prop}") }
pub fn fake_claude() -> String { concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_claude.py").to_string() }
pub fn log_dir() -> String { std::env::var("FAKE_CLAUDE_DIR").unwrap() }

/// the environment the peer has: the API key, and the credentials the peer must keep from the child (set once for the process)
pub fn env_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("gc-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FAKE_CLAUDE_DIR", &dir);
        std::env::set_var("ANTHROPIC_API_KEY", KEY);
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "tok");
        std::env::set_var("CLAUDE_CODE_USE_BEDROCK", "1");
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "oauth");
    });
}
/// what the fake claude logged for the run tagged `tag`
pub fn logged(tag: &str) -> J {
    for _ in 0..200 {
        if let Ok(t) = std::fs::read_to_string(format!("{}/{tag}.json", log_dir())) { if let Ok(v) = serde_json::from_str(&t) { return v; } }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the fake claude logged nothing for {tag}")
}

pub fn config(auth: Auth) -> Config {
    env_once();
    Config { ns: "claude".into(), claude_bin: fake_claude(), model: "sonnet".into(), auth, term_wait: Duration::from_secs(5), exe: env!("CARGO_BIN_EXE_graphlink-claude").into() }
}

pub struct FakeSeat { pub name: String, pub ctl: Shm, pub table: Cells, pub link: Link, revs: HashMap<String, f64>, rec: Reclaim }

impl FakeSeat {
    pub fn start(tag: &str) -> FakeSeat {
        let name = format!("gc{}{}", std::process::id() % 100000, tag);
        for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{name}-{r}")); }
        let mut ctl = Shm::create(format!("/gl-{name}-ctl"), 65536).expect("rings");
        let table = Cells::create(format!("/gl-{name}-sc"), NUMBERS.slots, NUMBERS.key_cap, NUMBERS.val_cap, 1).expect("seat table");
        let mut link = Link::new(Me { ns: "membrane".into(), build: "fake seat".into(), epoch: 1.0, support: vec![VERSION], initiator: true });
        ctl.send(&link.open()).unwrap();
        FakeSeat { name, ctl, table, link, revs: HashMap::new(), rec: Reclaim::new() }
    }
    pub fn peer(&self) -> Peer { Peer::attach(&self.name, "claude", &NUMBERS).expect("peer attaches") }
    pub fn run(&mut self, peer: &mut Peer) -> Vec<Event> {
        let mut seen = Vec::new();
        for _ in 0..20 {
            peer.poll().unwrap();
            while let Some(f) = self.ctl.recv().unwrap() {
                let (events, reply) = self.link.receive(f);
                if let Some(r) = reply { self.ctl.send(&r).unwrap(); }
                seen.extend(events);
            }
        }
        seen
    }
    /// the seat's link answers whatever the peer sent, once
    pub fn pump(&mut self) {
        while let Some(f) = self.ctl.recv().unwrap() {
            let (_, reply) = self.link.receive(f);
            if let Some(r) = reply { self.ctl.send(&r).unwrap(); }
        }
    }
    pub fn put(&mut self, eid: &str, prop: &str, value: J) {
        let k = key(eid, prop);
        let rev = self.revs.get(&k).copied().unwrap_or(0.0) + 1.0;
        self.table.put(&k, rev, &value).unwrap();
        self.revs.insert(k, rev);
    }
    pub fn retire(&mut self, eid: &str) {
        let prefix = format!("{eid}\u{1}");
        let keys: Vec<String> = self.revs.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        for k in keys { let rev = self.revs[&k] + 1.0; self.table.mark(&k, rev, CellState::Gone, "").unwrap(); self.revs.insert(k, rev); }
    }
    pub fn reader(&self) -> CellsReader { CellsReader::attach(format!("/gl-{}-pc", self.name)).expect("the peer's table") }

    // ---- the seat's chat request, as packs/chat_membrama.c1f writes it ----
    pub fn msg(&mut self, mid: &str, i: usize, role: &str, content: &str) {
        let e = format!("membrane::LlmMsg:{mid}:{i}");
        self.put(&e, "role", json!(role)); self.put(&e, "content", json!(content));
    }
    pub fn assistant_calls(&mut self, mid: &str, i: usize, calls: J) {
        let e = format!("membrane::LlmMsg:{mid}:{i}");
        self.put(&e, "role", json!("assistant")); self.put(&e, "content", json!(""));
        self.put(&e, "tool_calls", calls);
    }
    pub fn tools(&mut self, names: &[&str]) {
        for (i, n) in names.iter().enumerate() {
            self.put(&format!("membrane::LlmTool:door:{i}"), "spec", json!({"type": "function", "function": {"name": n, "description": format!("the {n} tool"), "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}}}));
        }
    }
    /// the request of round `n` over `messages` messages and `tools` tools
    pub fn request(&mut self, mid: &str, n: usize, messages: usize, tools: usize) {
        let e = format!("membrane::LlmRequest:{mid}-{n}");
        self.put(&e, "server", json!("claude"));
        self.put(&e, "messages", json!((0..messages).map(|i| format!("membrane::LlmMsg:{mid}:{i}")).collect::<Vec<_>>()));
        self.put(&e, "tools", json!((0..tools).map(|i| format!("membrane::LlmTool:door:{i}")).collect::<Vec<_>>()));
        self.put(&e, "settings", json!({"max_new": 100}));
    }
    /// round 1: a system message, a user message, the tools, the request
    pub fn round1(&mut self, mid: &str, user: &str, tools: &[&str]) {
        self.msg(mid, 0, "system", "You are the graph.");
        self.msg(mid, 1, "user", user);
        self.tools(tools);
        self.request(mid, 1, 2, tools.len());
    }
}
impl Drop for FakeSeat { fn drop(&mut self) { for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{}-{r}", self.name)); } } }

pub fn cell(r: &CellsReader, eid: &str, prop: &str) -> Option<J> {
    r.get(&key(eid, prop), 1_000_000, 32).unwrap().filter(|c| c.state == CellState::Live).map(|c| c.value)
}

pub struct Rig { pub seat: FakeSeat, pub peer: Peer, pub h: Handler }
impl Rig {
    pub fn new(tag: &str) -> Rig { Rig::with(tag, Auth::ApiKey) }
    pub fn with(tag: &str, auth: Auth) -> Rig {
        let mut seat = FakeSeat::start(tag); let mut peer = seat.peer(); seat.run(&mut peer);
        Rig { seat, peer, h: Handler::new(config(auth)) }
    }
    pub fn tick(&mut self) { self.h.tick(&mut self.peer).unwrap(); }
    /// ticks until `cond` holds on the peer's table (a minute at most: the fake is fast; a hung test is a failing one)
    pub fn until(&mut self, what: &str, cond: impl Fn(&CellsReader) -> bool) {
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            self.tick();
            if cond(&self.seat.reader()) { return; }
            assert!(Instant::now() < end, "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn until_state(&mut self, rid: &str, state: &str) {
        let e = format!("claude::Reply:{rid}"); let s = json!(state);
        self.until(&format!("{e} state {state}"), |r| cell(r, &e, "state") == Some(s.clone()));
    }
    pub fn ticks(&mut self, n: usize) { for _ in 0..n { self.tick(); std::thread::sleep(Duration::from_millis(10)); } }
}
