//! Shared by the tests that play the seat (the same rig as graphlink-claude's tests/common) and a fake OpenAI-compatible server: a std TcpListener the real `curl` talks to on 127.0.0.1.
//! Nothing here is a real server: the shapes are the ones the Chat Completions streaming API is documented to send.
#![allow(dead_code)]
use graphlink_core::frame::VERSION;
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_openai::handler::{Config, Handler};
use graphlink_openai::peer::{Numbers, Peer};
use graphlink_shm::cells::{CellState, Cells, CellsReader, Reclaim};
use graphlink_shm::{remove_region, Shm};
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const NUMBERS: Numbers = Numbers { slots: 128, key_cap: 128, val_cap: 4096, epoch: 1, spins: 1_000_000, depth: 32 };
pub const KEY: &str = "sk-test-SECRET-123";
pub const KEY_ENV: &str = "GLO_TEST_API_KEY";
pub fn key(eid: &str, prop: &str) -> String { format!("{eid}\u{1}{prop}") }

pub fn env_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var(KEY_ENV, KEY));
}

// ---- the fake server -------------------------------------------------------------------------------------------------------------------------------------------

pub enum Reply {
    /// 200 with these `data:` payloads, then `[DONE]`, then the connection closes
    Sse(Vec<String>),
    /// 200 with these `data:` payloads, then the connection stays open until the client goes
    SseThenHang(Vec<String>),
    /// this status and this body
    Status(u16, String),
}

#[derive(Clone, Debug)]
pub struct Seen { pub path: String, pub headers: Vec<(String, String)>, pub body: J }
impl Seen { pub fn header(&self, name: &str) -> Option<&str> { self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()) } }

pub struct FakeApi { pub port: u16, pub seen: Arc<Mutex<Vec<Seen>>>, pub disconnected: Arc<Mutex<u32>> }

fn read_request(s: &mut TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") { if s.read(&mut byte).ok()? == 0 { return None; } buf.push(byte[0]); }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut lines = head.split("\r\n");
    let path = lines.next()?.split(' ').nth(1)?.to_string();
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(": ").map(|(k, v)| (k.to_string(), v.to_string()))).collect();
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("expect") && v.contains("100")) { let _ = s.write_all(b"HTTP/1.1 100 Continue\r\n\r\n"); }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    Some(Seen { path, headers, body: serde_json::from_slice(&body).unwrap_or(J::Null) })
}

impl FakeApi {
    /// answers the requests that come, in order, with the replies given (a request beyond them gets a 500)
    pub fn start(replies: Vec<Reply>) -> FakeApi {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let disconnected = Arc::new(Mutex::new(0u32));
        let (seen2, dis2) = (seen.clone(), disconnected.clone());
        let replies = Arc::new(Mutex::new(replies.into_iter().collect::<std::collections::VecDeque<_>>()));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let (seen, dis, replies) = (seen2.clone(), dis2.clone(), replies.clone());
                std::thread::spawn(move || {
                    let Some(req) = read_request(&mut s) else { return };
                    seen.lock().unwrap().push(req);
                    let reply = replies.lock().unwrap().pop_front();
                    let sse = |s: &mut TcpStream, items: &[String]| {
                        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
                        for i in items { let _ = s.write_all(format!("data: {i}\n\n").as_bytes()); let _ = s.flush(); }
                    };
                    match reply {
                        Some(Reply::Sse(items)) => { sse(&mut s, &items); let _ = s.write_all(b"data: [DONE]\n\n"); }
                        Some(Reply::SseThenHang(items)) => {
                            sse(&mut s, &items);
                            let mut b = [0u8; 16];
                            loop { match s.read(&mut b) { Ok(0) | Err(_) => break, Ok(_) => {} } }
                            *dis.lock().unwrap() += 1;
                        }
                        Some(Reply::Status(code, body)) => {
                            let _ = s.write_all(format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                        }
                        None => { let _ = s.write_all(b"HTTP/1.1 500 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); }
                    }
                });
            }
        });
        FakeApi { port, seen, disconnected }
    }
    pub fn base_url(&self) -> String { format!("http://127.0.0.1:{}/v1", self.port) }
    pub fn requests(&self) -> Vec<Seen> { self.seen.lock().unwrap().clone() }
    pub fn wait_requests(&self, n: usize) -> Vec<Seen> {
        let end = Instant::now() + Duration::from_secs(60);
        loop { let r = self.requests(); if r.len() >= n { return r; } assert!(Instant::now() < end, "the fake server saw {} requests, not {n}", r.len()); std::thread::sleep(Duration::from_millis(10)); }
    }
    pub fn wait_disconnect(&self) {
        let end = Instant::now() + Duration::from_secs(60);
        while *self.disconnected.lock().unwrap() == 0 { assert!(Instant::now() < end, "the client never went away"); std::thread::sleep(Duration::from_millis(10)); }
    }
}

// ---- the shapes the streaming API sends ---------------------------------------------------------------------------------------------------------------------------

pub fn delta(text: &str) -> String { json!({"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]}).to_string() }
pub fn finish(reason: &str) -> String { json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}).to_string() }
pub fn usage(u: J) -> String { json!({"choices": [], "usage": u}).to_string() }
pub fn call_delta(index: u64, id: Option<&str>, name: Option<&str>, args: &str) -> String {
    let mut f = json!({"arguments": args});
    if let Some(n) = name { f["name"] = json!(n); }
    let mut c = json!({"index": index, "function": f});
    if let Some(i) = id { c["id"] = json!(i); c["type"] = json!("function"); }
    json!({"choices": [{"index": 0, "delta": {"tool_calls": [c]}, "finish_reason": null}]}).to_string()
}

// ---- the seat -----------------------------------------------------------------------------------------------------------------------------------------------------

pub fn config(base_url: &str) -> Config {
    env_once();
    Config { ns: "openai".into(), curl_bin: "curl".into(), base_url: base_url.into(), model: "gpt-test".into(), key_env: Some(KEY_ENV.into()), term_wait: Duration::from_secs(5) }
}

pub struct FakeSeat { pub name: String, pub ctl: Shm, pub table: Cells, pub link: Link, revs: HashMap<String, f64>, rec: Reclaim }

impl FakeSeat {
    pub fn start(tag: &str) -> FakeSeat {
        let name = format!("go{}{}", std::process::id() % 100000, tag);
        for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{name}-{r}")); }
        let mut ctl = Shm::create(format!("/gl-{name}-ctl"), 65536).expect("rings");
        let table = Cells::create(format!("/gl-{name}-sc"), NUMBERS.slots, NUMBERS.key_cap, NUMBERS.val_cap, 1).expect("seat table");
        let mut link = Link::new(Me { ns: "membrane".into(), build: "fake seat".into(), epoch: 1.0, support: vec![VERSION], initiator: true });
        ctl.send(&link.open()).unwrap();
        FakeSeat { name, ctl, table, link, revs: HashMap::new(), rec: Reclaim::new() }
    }
    pub fn peer(&self) -> Peer { Peer::attach(&self.name, "openai", &NUMBERS).expect("peer attaches") }
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

    // the seat's chat request, as packs/chat_membrama.c1f writes it: the assistant's calls are `{type, function: {name, arguments: <json text>}}` with no id
    pub fn msg(&mut self, mid: &str, i: usize, role: &str, content: &str) {
        let e = format!("membrane::LlmMsg:{mid}:{i}");
        self.put(&e, "role", json!(role)); self.put(&e, "content", json!(content));
    }
    pub fn assistant_calls(&mut self, mid: &str, i: usize, text: &str, calls: J) {
        let e = format!("membrane::LlmMsg:{mid}:{i}");
        self.put(&e, "role", json!("assistant")); self.put(&e, "content", json!(text));
        self.put(&e, "tool_calls", calls);
    }
    pub fn tools(&mut self, names: &[&str]) {
        for (i, n) in names.iter().enumerate() {
            self.put(&format!("membrane::LlmTool:door:{i}"), "spec", json!({"type": "function", "function": {"name": n, "description": format!("the {n} tool"), "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}}}));
        }
    }
    pub fn request(&mut self, mid: &str, n: usize, messages: usize, tools: usize, settings: J) {
        let e = format!("membrane::LlmRequest:{mid}-{n}");
        self.put(&e, "server", json!("openai"));
        self.put(&e, "messages", json!((0..messages).map(|i| format!("membrane::LlmMsg:{mid}:{i}")).collect::<Vec<_>>()));
        self.put(&e, "tools", json!((0..tools).map(|i| format!("membrane::LlmTool:door:{i}")).collect::<Vec<_>>()));
        self.put(&e, "settings", settings);
    }
    /// round 1: a system message, a user message, the tools, the request (settings: max_new 100)
    pub fn round1(&mut self, mid: &str, user: &str, tools: &[&str]) {
        self.msg(mid, 0, "system", "You are the graph.");
        self.msg(mid, 1, "user", user);
        self.tools(tools);
        self.request(mid, 1, 2, tools.len(), json!({"max_new": 100}));
    }
}
impl Drop for FakeSeat { fn drop(&mut self) { for r in ["ctl", "sc", "pc"] { let _ = remove_region(&format!("/gl-{}-{r}", self.name)); } } }

pub fn cell(r: &CellsReader, eid: &str, prop: &str) -> Option<J> {
    r.get(&key(eid, prop), 1_000_000, 32).unwrap().filter(|c| c.state == CellState::Live).map(|c| c.value)
}
pub fn text(r: &CellsReader, rid: &str) -> String {
    let n = cell(r, &format!("openai::Reply:{rid}"), "chunks").and_then(|v| v.as_u64()).unwrap_or(0);
    (0..n).map(|k| cell(r, &format!("openai::Chunk:{rid}:{k}"), "text").and_then(|v| v.as_str().map(String::from)).unwrap_or_default()).collect()
}

pub struct Rig { pub seat: FakeSeat, pub peer: Peer, pub h: Handler }
impl Rig {
    pub fn new(tag: &str, base_url: &str) -> Rig {
        let mut seat = FakeSeat::start(tag); let mut peer = seat.peer(); seat.run(&mut peer);
        Rig { seat, peer, h: Handler::new(config(base_url)) }
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
        let e = format!("openai::Reply:{rid}"); let s = json!(state);
        self.until(&format!("{e} state {state}"), |r| cell(r, &e, "state") == Some(s.clone()));
    }
    pub fn ticks(&mut self, n: usize) { for _ in 0..n { self.tick(); std::thread::sleep(Duration::from_millis(10)); } }
}
