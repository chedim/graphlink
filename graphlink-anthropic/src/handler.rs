//! The turn handler (docs/anthropic-peer.md): the seat's chat requests answered by calls of the Messages API, ONE call per request (the id `<mid>-<n>` is one round).
//! Sans-I/O like the peer: `tick` is called by whoever owns the thread and never blocks (except at the end of a call, for as long as `term_wait` allows).
//! The API keeps no history, so a round is independent of the others: the call carries the whole conversation the request names (src/wire.rs).
use crate::http::{Call, Ev};
use crate::peer::{Peer, Seen};
use crate::wire::{self, Msg};
use graphlink_core::frame::Target;
use graphlink_core::link::Event;
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

const REQUEST: &str = "membrane::LlmRequest:";
const MSG: &str = "membrane::LlmMsg:";
const TOOL: &str = "membrane::LlmTool:";
/// The header of a CBOR text string of 256 to 65535 bytes: a cell's value cap holds the whole item, so a chunk is `val_cap` minus this.
const CBOR_TEXT_HEADER: usize = 3;
/// The seat reads a reply of up to four chunks (packs/chat_membrama.c1f, `chunks_in`).
pub const MAX_CHUNKS: usize = 4;
/// How much of curl's stderr, or of a body that is not a stream, an error reply carries (the end of it).
const TAIL: usize = 2000;

#[derive(Clone, Debug)]
pub struct Config {
    /// The namespace this peer owns.
    pub ns: String,
    pub curl_bin: String,
    /// The API's address without the path.
    pub base_url: String,
    pub model: String,
    /// The owner's number: the API requires it on every call.
    pub max_tokens: u64,
    /// The `anthropic-version` header.
    pub version: String,
    /// The NAME of the environment variable that holds the API key.
    pub key_env: String,
    /// How long to wait for a curl to be gone after a SIGTERM.
    pub term_wait: Duration,
}

/// The text cut into pieces of at most `cap` bytes, on character boundaries.
pub fn cut(text: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if cur.len() + c.len_utf8() > cap { out.push(std::mem::take(&mut cur)); }
        cur.push(c);
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

enum Block { Text, Tool { name: String, json: String }, Other }

struct Run {
    call: Call,
    req: String,
    text: String,
    published: Vec<String>,
    blocks: BTreeMap<u64, Block>,
    /// the token counts the stream gave, by the API's own names (a later value replaces an earlier one)
    usage: BTreeMap<String, u64>,
    stop: Option<String>,
    /// what came that is not a stream line (an error body)
    raw: String,
    tail: String,
    closed: u8,
    done: bool,
    changed: bool,
}

struct Dying { call: Call, deadline: Instant }

pub struct Handler {
    cfg: Config,
    subscribed: bool,
    started: HashSet<String>,
    /// request node -> its id, for the requests that were answered: when the seat retires the request, the reply and its chunks are retired
    finished: HashMap<String, String>,
    active: HashMap<String, Run>,
    dying: Vec<Dying>,
    reclaim_warned: bool,
}

struct Req { msgs: Vec<Msg>, specs: Vec<J> }
enum Read { Wait, Aborted, Failed(String), Ready(Req) }

fn truthy(v: Option<&J>) -> bool { matches!(v, Some(J::Bool(true))) }
fn refusal(eid: &str, prop: &str, why: &str) -> String {
    format!("{eid} {prop}: the seat's table could not store it ({why}); the value size of the peer's tables must be above the largest message or tool")
}
macro_rules! need {
    ($peer:expr, $eid:expr, $prop:expr) => {
        match $peer.seen($eid, $prop)? { Seen::Live(v) => v, Seen::Absent => return Ok(Read::Wait), Seen::Refused(why) => return Ok(Read::Failed(refusal($eid, $prop, &why))) }
    };
}
macro_rules! maybe {
    ($peer:expr, $eid:expr, $prop:expr) => {
        match $peer.seen($eid, $prop)? { Seen::Live(v) => Some(v), Seen::Absent => None, Seen::Refused(why) => return Ok(Read::Failed(refusal($eid, $prop, &why))) }
    };
}

fn read_request(peer: &Peer, eid: &str) -> Result<Read, String> {
    if truthy(peer.read(eid, "abort")?.as_ref()) { return Ok(Read::Aborted); }
    let ids = need!(peer, eid, "messages");
    let mut msgs = Vec::new();
    for m in ids.as_array().into_iter().flatten().filter_map(J::as_str) {
        let role = need!(peer, m, "role"); let content = need!(peer, m, "content");
        let Some(text) = content.as_str() else { return Ok(Read::Failed(format!("{m} content: it must be a string"))) };
        let calls = maybe!(peer, m, "tool_calls").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        msgs.push(Msg { role: role.as_str().unwrap_or("").to_string(), content: text.to_string(), tool_calls: calls });
    }
    let mut specs = Vec::new();
    if let Some(ids) = maybe!(peer, eid, "tools") {
        for tid in ids.as_array().into_iter().flatten().filter_map(J::as_str) { specs.push(need!(peer, tid, "spec")); }
    }
    Ok(Read::Ready(Req { msgs, specs }))
}

impl Handler {
    pub fn new(cfg: Config) -> Handler {
        Handler { cfg, subscribed: false, started: HashSet::new(), finished: HashMap::new(), active: HashMap::new(), dying: Vec::new(), reclaim_warned: false }
    }

    fn reply(&self, rid: &str) -> String { format!("{}::Reply:{rid}", self.cfg.ns) }
    fn key(&self) -> String { std::env::var(&self.cfg.key_env).unwrap_or_default() }

    /// What an error may say: the API key, if it is in the text, is not.
    fn mask(&self, text: &str) -> String {
        let k = self.key();
        if k.is_empty() { text.to_string() } else { text.replace(&k, "[key]") }
    }

    /// Takes what the seat wrote, starts the calls of the requests that are complete, moves the calls' output into replies; returns the peer's events.
    pub fn tick(&mut self, peer: &mut Peer) -> Result<Vec<Event>, String> {
        let events = peer.poll()?;
        let seen_removals = peer.before();
        if !self.subscribed && peer.is_open() {
            for p in [REQUEST, MSG, TOOL] { peer.subscribe(Target::Prefix(p.into()), None)?; }
            self.subscribed = true;
        }
        let ids: BTreeMap<String, String> = peer.scan(REQUEST)?.into_iter().map(|(eid, _, _)| { let id = eid[REQUEST.len()..].to_string(); (eid, id) }).collect();
        // a request the seat retired: its reply and chunks are retired, and a call waiting on it is ended
        let retired: Vec<String> = self.finished.keys().filter(|e| !ids.contains_key(*e)).cloned().collect();
        for e in retired {
            if let Some(id) = self.finished.remove(&e) { peer.retire(&self.reply(&id))?; peer.retire_prefix(&format!("{}::Chunk:{id}:", self.cfg.ns))?; }
            self.started.remove(&e);
        }
        let gone: Vec<String> = self.active.iter().filter(|(_, r)| !ids.contains_key(&r.req)).map(|(rid, _)| rid.clone()).collect();
        for rid in gone { self.end(&rid); }
        // an abort of the request a call is on
        let aborted: Vec<String> = self.active.iter().filter(|(_, r)| truthy(peer.read(&r.req, "abort").ok().flatten().as_ref())).map(|(rid, _)| rid.clone()).collect();
        for rid in aborted {
            if self.active.get(&rid).map(|r| !r.done).unwrap_or(false) { self.finish(peer, &rid, "aborted", None, None, None, None)?; }
            self.end(&rid);
        }
        for (eid, rid) in ids {
            if self.started.contains(&eid) { continue; }
            match read_request(peer, &eid)? {
                Read::Wait => {}
                Read::Aborted => { self.mark(&eid, &rid); self.finish(peer, &rid, "aborted", None, None, None, None)?; }
                Read::Failed(why) => { self.mark(&eid, &rid); self.finish(peer, &rid, "error", None, None, None, Some(&why))?; }
                Read::Ready(req) => { self.mark(&eid, &rid); if let Err(why) = self.begin(peer, &eid, &rid, req) { self.finish(peer, &rid, "error", None, None, None, Some(&why))?; } }
            }
        }
        let rids: Vec<String> = self.active.keys().cloned().collect();
        for r in rids { self.drain(peer, &r)?; }
        self.reap();
        if let Err(e) = peer.after(seen_removals) { if !self.reclaim_warned { self.reclaim_warned = true; eprintln!("graphlink-anthropic: reclaiming slots: {e}"); } }
        Ok(events)
    }

    fn mark(&mut self, eid: &str, rid: &str) { self.started.insert(eid.to_string()); self.finished.insert(eid.to_string(), rid.to_string()); }

    /// A request that is complete: the call is made.
    fn begin(&mut self, peer: &mut Peer, eid: &str, rid: &str, req: Req) -> Result<(), String> {
        let body = wire::body(&self.cfg.model, self.cfg.max_tokens, &req.msgs, &req.specs)?;
        let call = Call::start(&self.cfg, &self.key(), &body)?;
        let r = self.reply(rid);
        peer.publish(&r, "chunks", &json!(0))?;
        peer.publish(&r, "state", &json!("running"))?;
        self.active.insert(rid.to_string(), Run { call, req: eid.to_string(), text: String::new(), published: Vec::new(), blocks: BTreeMap::new(), usage: BTreeMap::new(), stop: None, raw: String::new(), tail: String::new(), closed: 0, done: false, changed: false });
        Ok(())
    }

    /// The last words of a reply. `state` is written last: the seat reads the rest once it says done.
    fn finish(&self, peer: &mut Peer, rid: &str, state: &str, chunks: Option<usize>, usage: Option<J>, tool_calls: Option<(&str, Vec<J>)>, error: Option<&str>) -> Result<(), String> {
        let r = self.reply(rid);
        peer.publish(&r, "chunks", &json!(chunks.unwrap_or(0)))?;
        if let Some((finish, calls)) = &tool_calls {
            if !calls.is_empty() { peer.publish(&r, "tool_calls", &J::Array(calls.clone()))?; }
            peer.publish(&r, "finish", &json!(finish))?;
        }
        if let Some(u) = usage { peer.publish(&r, "usage", &u)?; }
        if let Some(e) = error { peer.publish(&r, "error", &json!(self.mask(e)))?; }
        peer.publish(&r, "state", &json!(state))
    }

    /// A call ends: SIGTERM, and the curl is reaped as it goes.
    fn end(&mut self, rid: &str) {
        if let Some(r) = self.active.remove(rid) {
            r.call.term();
            self.dying.push(Dying { call: r.call, deadline: Instant::now() + self.cfg.term_wait });
        }
    }

    /// The curl is over (it said its last): it is reaped, not terminated.
    fn end_natural(&mut self, rid: &str) {
        if let Some(r) = self.active.remove(rid) { self.dying.push(Dying { call: r.call, deadline: Instant::now() + self.cfg.term_wait }); }
    }

    fn reap(&mut self) {
        self.dying.retain_mut(|d| match d.call.child.try_wait() {
            Ok(Some(_)) => false,
            _ => { if Instant::now() > d.deadline { eprintln!("graphlink-anthropic: a curl (pid {}) is still there after SIGTERM; not waiting for it (never SIGKILL)", d.call.child.id()); false } else { true } }
        });
    }

    /// The seat's end is gone: every call gets a SIGTERM, and this waits (at most `term_wait`) for them to be gone.
    pub fn shutdown(&mut self) {
        let rids: Vec<String> = self.active.keys().cloned().collect();
        for r in rids { self.end(&r); }
        let end = Instant::now() + self.cfg.term_wait;
        while !self.dying.is_empty() && Instant::now() < end {
            self.reap();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// What a call said since the last tick; the text it has so far is published once, at the end of the pass.
    fn drain(&mut self, peer: &mut Peer, rid: &str) -> Result<(), String> {
        loop {
            let Some(run) = self.active.get_mut(rid) else { return Ok(()) };
            let Ok(ev) = run.call.rx.try_recv() else { break };
            if self.event(peer, rid, ev)? { self.end_natural(rid); return Ok(()); }
        }
        if let Some(run) = self.active.get_mut(rid) {
            if run.changed && !run.done {
                run.changed = false;
                if let Err(e) = self.publish_text(peer, rid) { self.finish(peer, rid, "error", None, None, None, Some(&e))?; self.end(rid); }
            }
        }
        Ok(())
    }

    /// The round's text into chunk cells (the one being filled is rewritten as it grows); more than the four chunks the seat reads is an error.
    fn publish_text(&mut self, peer: &mut Peer, rid: &str) -> Result<(), String> {
        let cap = (peer.val_cap() as usize).saturating_sub(CBOR_TEXT_HEADER);
        let r = self.active.get_mut(rid).expect("active");
        let pieces = cut(&r.text, cap);
        if pieces.len() > MAX_CHUNKS { return Err(format!("the reply is {} bytes: more than the {MAX_CHUNKS} chunks of {cap} bytes the seat reads", r.text.len())); }
        for (k, p) in pieces.iter().enumerate() {
            if r.published.get(k) != Some(p) {
                peer.publish(&format!("{}::Chunk:{rid}:{k}", self.cfg.ns), "text", &json!(p))?;
                if r.published.len() <= k { r.published.push(p.clone()); } else { r.published[k] = p.clone(); }
            }
        }
        peer.publish(&format!("{}::Reply:{rid}", self.cfg.ns), "chunks", &json!(r.published.len()))
    }

    /// One event of a call; true when the call is over.
    fn event(&mut self, peer: &mut Peer, rid: &str, ev: Ev) -> Result<bool, String> {
        let term_wait = self.cfg.term_wait;
        let r = self.active.get_mut(rid).expect("active");
        match ev {
            Ev::Err(line) => { r.tail.push_str(&line); r.tail.push('\n'); r.tail = tail_of(&r.tail); Ok(false) }
            Ev::Closed => {
                r.closed += 1;
                if r.closed < 2 { return Ok(false); }
                if r.done { return Ok(true); }
                let rc = wait_code(&mut r.call.child, term_wait);
                let (raw, tail) = (r.raw.trim().to_string(), r.tail.trim().to_string());
                let n = r.published.len();
                let why = if !raw.is_empty() { api_error(&raw) }
                    else { match rc {
                        Some(0) => "the stream ended before the message was complete".to_string(),
                        Some(c) => format!("curl failed (rc {c}): {tail}"),
                        None => format!("curl ended without an exit code (killed by a signal): {tail}"),
                    } };
                self.finish(peer, rid, "error", Some(n), None, None, Some(&why))?;
                Ok(true)
            }
            Ev::Out(line) => {
                let line = line.trim_end();
                let Some(data) = line.strip_prefix("data:") else {
                    if !(line.is_empty() || line.starts_with("event:") || line.starts_with(':')) { r.raw.push_str(line); r.raw.push('\n'); r.raw = tail_of(&r.raw); }
                    return Ok(false);
                };
                let Ok(v) = serde_json::from_str::<J>(data.trim()) else { return Ok(false) };
                self.stream(peer, rid, &v)
            }
        }
    }

    /// One `data:` event of the stream (the Messages API's streaming events).
    fn stream(&mut self, peer: &mut Peer, rid: &str, v: &J) -> Result<bool, String> {
        let r = self.active.get_mut(rid).expect("active");
        let mut take = |u: &J| for k in ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"] { if let Some(n) = u[k].as_u64() { r.usage.insert(k.to_string(), n); } };
        match v["type"].as_str() {
            Some("message_start") => { take(&v["message"]["usage"]); Ok(false) }
            Some("content_block_start") => {
                let i = v["index"].as_u64().unwrap_or(0);
                let b = &v["content_block"];
                match b["type"].as_str() {
                    Some("text") => {
                        if !r.text.is_empty() { r.text.push_str("\n\n"); }
                        r.blocks.insert(i, Block::Text);
                    }
                    Some("tool_use") => { r.blocks.insert(i, Block::Tool { name: b["name"].as_str().unwrap_or("").to_string(), json: String::new() }); }
                    _ => { r.blocks.insert(i, Block::Other); }
                }
                Ok(false)
            }
            Some("content_block_delta") => {
                let i = v["index"].as_u64().unwrap_or(0);
                match (r.blocks.get_mut(&i), v["delta"]["type"].as_str()) {
                    (Some(Block::Text), Some("text_delta")) => { r.text.push_str(v["delta"]["text"].as_str().unwrap_or("")); r.changed = true; }
                    (Some(Block::Tool { json, .. }), Some("input_json_delta")) => json.push_str(v["delta"]["partial_json"].as_str().unwrap_or("")),
                    _ => {}
                }
                Ok(false)
            }
            Some("message_delta") => {
                if let Some(s) = v["delta"]["stop_reason"].as_str() { r.stop = Some(s.to_string()); }
                take(&v["usage"]);
                Ok(false)
            }
            Some("error") => {
                let e = &v["error"];
                let why = format!("the API said ({}): {}", e["type"].as_str().unwrap_or("error"), e["message"].as_str().unwrap_or("no message"));
                let n = r.published.len();
                self.finish(peer, rid, "error", Some(n), None, None, Some(&why))?;
                Ok(true)
            }
            Some("message_stop") => {
                let (stop, usage) = (r.stop.clone().unwrap_or_default(), usage_of(&r.usage));
                let calls: Result<Vec<J>, String> = r.blocks.values().filter_map(|b| match b { Block::Tool { name, json } => Some((name, json)), _ => None }).map(|(name, json)| {
                    match if json.trim().is_empty() { Ok(json!({})) } else { serde_json::from_str::<J>(json) } {
                        Ok(a @ J::Object(_)) => Ok(json!({"name": name, "arguments": a})),
                        _ => Err(format!("the arguments the model gave the tool {name} are not a JSON object")),
                    }
                }).collect();
                let n = r.published.len();
                let calls = match calls { Ok(c) => c, Err(why) => { self.finish(peer, rid, "error", Some(n), None, None, Some(&why))?; return Ok(true); } };
                if let Err(e) = self.publish_text(peer, rid) { self.finish(peer, rid, "error", None, None, None, Some(&e))?; return Ok(true); }
                let r = self.active.get_mut(rid).expect("active");
                r.done = true;
                let n = r.published.len();
                let finish = match stop.as_str() { "tool_use" if !calls.is_empty() => "tool_calls", "max_tokens" => "length", _ => "stop" };
                let calls = if finish == "tool_calls" { calls } else { Vec::new() };
                self.finish(peer, rid, "done", Some(n), usage, Some((finish, calls)), None)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// The end of a text, at most `TAIL` bytes, on a character boundary.
fn tail_of(s: &str) -> String {
    if s.len() <= TAIL { return s.to_string(); }
    let mut from = s.len() - TAIL;
    while !s.is_char_boundary(from) { from += 1; }
    s[from..].to_string()
}

/// What an error body says: the API's `error.type` and `error.message` when it is the API's shape, the body's end otherwise.
fn api_error(raw: &str) -> String {
    match serde_json::from_str::<J>(raw) {
        Ok(v) if v["error"]["message"].is_string() => format!("the API said ({}): {}", v["error"]["type"].as_str().unwrap_or("error"), v["error"]["message"].as_str().unwrap_or("")),
        _ => format!("the API answered with something that is not a stream: {raw}"),
    }
}

fn wait_code(child: &mut std::process::Child, wait: Duration) -> Option<i32> {
    let end = Instant::now() + wait;
    loop {
        if let Ok(Some(s)) = child.try_wait() { return s.code(); }
        if Instant::now() > end { unsafe { libc::kill(child.id() as i32, libc::SIGTERM); } return None; }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The usage of a round: `read` the tokens the model read fresh (input, and written to the prompt cache), `loaded` the tokens served from the prompt cache, `generated` the output
/// tokens; each only when the API said it. None when it gave no usage at all.
pub fn usage_of(u: &BTreeMap<String, u64>) -> Option<J> {
    let mut o = serde_json::Map::new();
    let (i, w) = (u.get("input_tokens"), u.get("cache_creation_input_tokens"));
    if i.is_some() || w.is_some() { o.insert("read".into(), json!(i.copied().unwrap_or(0) + w.copied().unwrap_or(0))); }
    if let Some(v) = u.get("cache_read_input_tokens") { o.insert("loaded".into(), json!(v)); }
    if let Some(v) = u.get("output_tokens") { o.insert("generated".into(), json!(v)); }
    if o.is_empty() { None } else { Some(J::Object(o)) }
}
