//! Shared by the tests that play the seat (it makes the control rings and its cell table first and sends its hello before the peer starts, as the seat's driver does) and
//! the Messages API (`FakeApi`: a std TcpListener that speaks just enough HTTP for the real `curl` of the machine; NOT the real API).
#![allow(dead_code)]
use graphlink_anthropic::handler::{Config, Handler};
use graphlink_anthropic::peer::{Numbers, Peer};
use graphlink_core::frame::VERSION;
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_shm::cells::{CellState, Cells, CellsReader, Reclaim};
use graphlink_shm::{remove_region, Shm};
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const NUMBERS: Numbers = Numbers { slots: 128, key_cap: 128, val_cap: 4096, epoch: 1, spins: 1_000_000, depth: 32 };
pub const KEY: &str = "sk-ant-test-SECRET-123";
pub const KEY_ENV: &str = "GLA_TEST_KEY";
pub const MAX_TOKENS: u64 = 321;
pub fn key(eid: &str, prop: &str) -> String { format!("{eid}\u{1}{prop}") }

/// the environment the peer has: the key in the variable the owner names
pub fn env_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var(KEY_ENV, KEY));
}

pub fn config(api: &FakeApi) -> Config {
    env_once();
    Config { ns: "anthropic".into(), curl_bin: "curl".into(), base_url: api.url(), model: "claude-test".into(), max_tokens: MAX_TOKENS, version: "2023-06-01".into(), key_env: KEY_ENV.into(), term_wait: Duration::from_secs(5) }
}

// ---------------------------------------------------------------- the fake API

pub struct Seen { pub headers: HashMap<String, String>, pub body: J }
pub enum Reply { Sse(Vec<(String, J)>), Raw(u16, String), Hang }
type Script = Box<dyn Fn(usize, &J) -> Reply + Send + Sync>;

pub struct FakeApi { port: u16, pub seen: Arc<Mutex<Vec<Seen>>>, pub open: Arc<AtomicUsize> }

fn read_request(s: &mut TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let (head_end, len) = loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 { return None; }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
            let len = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
            break (p + 4, len);
        }
    };
    while buf.len() < head_end + len {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 { break; }
        buf.extend_from_slice(&tmp[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let headers = head.lines().skip(1).filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string())).collect();
    let body = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or(J::Null);
    Some(Seen { headers, body })
}

impl FakeApi {
    pub fn start(script: impl Fn(usize, &J) -> Reply + Send + Sync + 'static) -> FakeApi {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let open = Arc::new(AtomicUsize::new(0));
        let (seen2, open2) = (seen.clone(), open.clone());
        let script: Arc<Script> = Arc::new(Box::new(script));
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut s) = conn else { continue };
                let (seen, open, script) = (seen2.clone(), open2.clone(), script.clone());
                std::thread::spawn(move || {
                    let Some(req) = read_request(&mut s) else { return };
                    let idx = { let mut g = seen.lock().unwrap(); g.push(Seen { headers: req.headers, body: req.body.clone() }); g.len() - 1 };
                    open.fetch_add(1, Ordering::SeqCst);
                    match script(idx, &req.body) {
                        Reply::Sse(events) => {
                            let mut out = String::from("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n");
                            for (name, data) in events { out.push_str(&format!("event: {name}\ndata: {data}\n\n")); }
                            let _ = s.write_all(out.as_bytes());
                        }
                        Reply::Raw(code, body) => {
                            let _ = s.write_all(format!("HTTP/1.1 {code} X\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{body}").as_bytes());
                        }
                        Reply::Hang => {
                            let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n: waiting\n\n");
                            let mut b = [0u8; 64];
                            while matches!(s.read(&mut b), Ok(n) if n > 0) {}   // until the client goes
                        }
                    }
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        FakeApi { port, seen, open }
    }
    pub fn url(&self) -> String { format!("http://127.0.0.1:{}", self.port) }
    pub fn body(&self, i: usize) -> J { self.seen.lock().unwrap()[i].body.clone() }
    pub fn header(&self, i: usize, name: &str) -> Option<String> { self.seen.lock().unwrap()[i].headers.get(name).cloned() }
    pub fn calls(&self) -> usize { self.seen.lock().unwrap().len() }
}

/// the stream of a message: `usage` is the `message_start` usage, `out` the output tokens `message_delta` ends with
pub fn message_start(usage: J) -> (String, J) { ("message_start".into(), json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": [], "usage": usage}})) }
pub fn text_block(index: u64, parts: &[&str]) -> Vec<(String, J)> {
    let mut v = vec![("content_block_start".to_string(), json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}))];
    for p in parts { v.push(("content_block_delta".into(), json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": p}}))); }
    v.push(("content_block_stop".into(), json!({"type": "content_block_stop", "index": index})));
    v
}
pub fn tool_block(index: u64, id: &str, name: &str, json_parts: &[&str]) -> Vec<(String, J)> {
    let mut v = vec![("content_block_start".to_string(), json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}))];
    for p in json_parts { v.push(("content_block_delta".into(), json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": p}}))); }
    v.push(("content_block_stop".into(), json!({"type": "content_block_stop", "index": index})));
    v
}
pub fn end(stop: &str, out: u64) -> Vec<(String, J)> {
    vec![("message_delta".into(), json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": out}})),
         ("message_stop".into(), json!({"type": "message_stop"}))]
}
/// a whole plain answer
pub fn answer(text: &str) -> Reply {
    let mut v = vec![message_start(json!({"input_tokens": 5, "output_tokens": 1}))];
    v.extend(text_block(0, &[text]));
    v.extend(end("end_turn", 7));
    Reply::Sse(v)
}

// ---------------------------------------------------------------- the seat

pub struct FakeSeat { pub name: String, pub ctl: Shm, pub table: Cells, pub link: Link, revs: HashMap<String, f64>, rec: Reclaim }

impl FakeSeat {
    pub fn start(tag: &str) -> FakeSeat {
        let name = format!("ga{}{}", std::process::id() % 100000, tag);
        for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{name}-{r}")); }
        let mut ctl = Shm::create(format!("/gl-{name}-ctl"), 65536).expect("rings");
        let table = Cells::create(format!("/gl-{name}-sc"), NUMBERS.slots, NUMBERS.key_cap, NUMBERS.val_cap, 1).expect("seat table");
        let mut link = Link::new(Me { ns: "membrane".into(), build: "fake seat".into(), epoch: 1.0, support: vec![VERSION], initiator: true });
        ctl.send(&link.open()).unwrap();
        FakeSeat { name, ctl, table, link, revs: HashMap::new(), rec: Reclaim::new() }
    }
    pub fn peer(&self) -> Peer { Peer::attach(&self.name, "anthropic", &NUMBERS).expect("peer attaches") }
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

    // the seat's chat request, as packs/chat_membrama.c1f writes it
    pub fn msg(&mut self, mid: &str, i: usize, role: &str, content: &str) {
        let e = format!("membrane::LlmMsg:{mid}:{i}");
        self.put(&e, "role", json!(role)); self.put(&e, "content", json!(content));
    }
    pub fn assistant_calls(&mut self, mid: &str, i: usize, content: &str, calls: J) {
        let e = format!("membrane::LlmMsg:{mid}:{i}");
        self.put(&e, "role", json!("assistant")); self.put(&e, "content", json!(content));
        self.put(&e, "tool_calls", calls);
    }
    pub fn tools(&mut self, names: &[&str]) {
        for (i, n) in names.iter().enumerate() {
            self.put(&format!("membrane::LlmTool:door:{i}"), "spec", json!({"type": "function", "function": {"name": n, "description": format!("the {n} tool"), "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}}}));
        }
    }
    pub fn request(&mut self, mid: &str, n: usize, messages: usize, tools: usize) {
        let e = format!("membrane::LlmRequest:{mid}-{n}");
        self.put(&e, "server", json!("anthropic"));
        self.put(&e, "messages", json!((0..messages).map(|i| format!("membrane::LlmMsg:{mid}:{i}")).collect::<Vec<_>>()));
        self.put(&e, "tools", json!((0..tools).map(|i| format!("membrane::LlmTool:door:{i}")).collect::<Vec<_>>()));
        self.put(&e, "settings", json!({"max_new": 100}));
    }
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
pub fn text(r: &CellsReader, rid: &str) -> String {
    let n = cell(r, &format!("anthropic::Reply:{rid}"), "chunks").and_then(|v| v.as_u64()).unwrap_or(0);
    (0..n).map(|k| cell(r, &format!("anthropic::Chunk:{rid}:{k}"), "text").and_then(|v| v.as_str().map(String::from)).unwrap_or_default()).collect()
}

pub struct Rig { pub seat: FakeSeat, pub peer: Peer, pub h: Handler }
impl Rig {
    pub fn new(tag: &str, api: &FakeApi) -> Rig { Rig::with(tag, config(api)) }
    pub fn with(tag: &str, cfg: Config) -> Rig {
        let mut seat = FakeSeat::start(tag); let mut peer = seat.peer(); seat.run(&mut peer);
        Rig { seat, peer, h: Handler::new(cfg) }
    }
    pub fn tick(&mut self) { self.h.tick(&mut self.peer).unwrap(); }
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
        let e = format!("anthropic::Reply:{rid}"); let s = json!(state);
        self.until(&format!("{e} state {state}"), |r| cell(r, &e, "state") == Some(s.clone()));
    }
    pub fn ticks(&mut self, n: usize) { for _ in 0..n { self.tick(); std::thread::sleep(Duration::from_millis(10)); } }
}
