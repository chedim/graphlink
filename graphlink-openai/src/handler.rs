//! The turn handler (docs/openai-peer.md): the seat's chat requests answered by calls to an OpenAI-compatible chat completions server, one `curl` per request (`<mid>-<n>`).
//! Sans-I/O like the peer: `tick` is called by whoever owns the thread and never blocks (except at the end of a call, for as long as `term_wait` allows).
//! The API keeps nothing, so there is nothing to hold between rounds: round n is a call that carries the whole history the seat wrote, and its reply is Reply n.
use crate::http::{Ev, Run, STATUS_MARK};
use crate::peer::{Peer, Seen};
use crate::wire::{build_body, Msg, Stream};
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
/// How much of curl's stderr, or of a server's error body, an error reply carries.
const TAIL: usize = 2000;

#[derive(Clone, Debug)]
pub struct Config {
    /// The namespace this peer owns.
    pub ns: String,
    pub curl_bin: String,
    /// The server's base: the call goes to `<base_url>/chat/completions`.
    pub base_url: String,
    pub model: String,
    /// The NAME of the environment variable that holds the API key; `None` is a server that wants none (no `Authorization` header).
    pub key_env: Option<String>,
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

struct Active {
    run: Run,
    /// the request node this call answers
    req: String,
    stream: Stream,
    published: Vec<String>,
    status: Option<u16>,
    /// what curl's stdout said that is not the stream (the body of an error)
    body: String,
    /// the end of curl's stderr
    tail: String,
    closed: u8,
    done: bool,
}

struct Dying { run: Run, deadline: Instant }

pub struct Handler {
    cfg: Config,
    subscribed: bool,
    started: HashSet<String>,
    /// request node -> its id, for the requests that were answered: when the seat retires the request, the reply and its chunks are retired
    finished: HashMap<String, String>,
    active: HashMap<String, Active>,
    dying: Vec<Dying>,
    reclaim_warned: bool,
}

struct Req { rid: String, msgs: Vec<Msg>, specs: Vec<J>, max_new: Option<u64> }
enum Read { Wait, Aborted, Failed(String), Ready(Req) }
enum Next { Go, Over, Kill }

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

fn read_request(peer: &Peer, eid: &str, rid: &str) -> Result<Read, String> {
    if truthy(peer.read(eid, "abort")?.as_ref()) { return Ok(Read::Aborted); }
    let ids = need!(peer, eid, "messages");
    let mut msgs = Vec::new();
    for mid in ids.as_array().into_iter().flatten().filter_map(J::as_str) {
        let role = need!(peer, mid, "role"); let content = need!(peer, mid, "content");
        let Some(text) = content.as_str() else { return Ok(Read::Failed(format!("{mid} content: it must be a string"))) };
        msgs.push(Msg { role: role.as_str().unwrap_or("").to_string(), content: text.to_string(), tool_calls: maybe!(peer, mid, "tool_calls") });
    }
    let mut specs = Vec::new();
    if let Some(ids) = maybe!(peer, eid, "tools") {
        for tid in ids.as_array().into_iter().flatten().filter_map(J::as_str) { specs.push(need!(peer, tid, "spec")); }
    }
    let max_new = maybe!(peer, eid, "settings").and_then(|s| s["max_new"].as_u64());
    Ok(Read::Ready(Req { rid: rid.to_string(), msgs, specs, max_new }))
}

impl Handler {
    pub fn new(cfg: Config) -> Handler {
        Handler { cfg, subscribed: false, started: HashSet::new(), finished: HashMap::new(), active: HashMap::new(), dying: Vec::new(), reclaim_warned: false }
    }

    fn reply(&self, rid: &str) -> String { format!("{}::Reply:{rid}", self.cfg.ns) }

    /// The key, from the variable the owner named (never from an option); `None` when the owner named none.
    fn key(&self) -> Result<Option<String>, String> {
        let Some(name) = &self.cfg.key_env else { return Ok(None) };
        match std::env::var(name) { Ok(k) if !k.is_empty() => Ok(Some(k)), _ => Err(format!("the environment variable {name}, which holds the API key, is not set in the environment of this peer")) }
    }

    /// What an error may say: the API key, if it is in the text, is not.
    fn mask(&self, text: &str) -> String {
        match self.key() { Ok(Some(k)) => text.replace(&k, "[key]"), _ => text.to_string() }
    }

    /// Takes what the seat wrote, starts the calls whose requests are complete, moves what the servers said into replies; returns the peer's events.
    pub fn tick(&mut self, peer: &mut Peer) -> Result<Vec<Event>, String> {
        let events = peer.poll()?;
        let seen_removals = peer.before();
        if !self.subscribed && peer.is_open() {
            for p in [REQUEST, MSG, TOOL] { peer.subscribe(Target::Prefix(p.into()), None)?; }
            self.subscribed = true;
        }
        let ids: BTreeMap<String, String> = peer.scan(REQUEST)?.into_iter().map(|(eid, _, _)| { let id = eid[REQUEST.len()..].to_string(); (eid, id) }).collect();
        // a request the seat retired: its reply and chunks are retired, and a call still going for it is ended
        let retired: Vec<String> = self.finished.keys().filter(|e| !ids.contains_key(*e)).cloned().collect();
        for e in retired {
            if let Some(id) = self.finished.remove(&e) { peer.retire(&self.reply(&id))?; peer.retire_prefix(&format!("{}::Chunk:{id}:", self.cfg.ns))?; }
            self.started.remove(&e);
        }
        let gone: Vec<String> = self.active.iter().filter(|(_, a)| !ids.contains_key(&a.req)).map(|(r, _)| r.clone()).collect();
        for r in gone { self.end(&r); }
        let aborted: Vec<String> = self.active.iter().filter(|(_, a)| truthy(peer.read(&a.req, "abort").ok().flatten().as_ref())).map(|(r, _)| r.clone()).collect();
        for r in aborted { self.finish(peer, &r, "aborted", None, None, None, None)?; self.end(&r); }
        for (eid, rid) in ids {
            if self.started.contains(&eid) { continue; }
            match read_request(peer, &eid, &rid)? {
                Read::Wait => {}
                Read::Aborted => { self.mark(&eid, &rid); self.finish(peer, &rid, "aborted", None, None, None, None)?; }
                Read::Failed(why) => { self.mark(&eid, &rid); self.finish(peer, &rid, "error", None, None, None, Some(&why))?; }
                Read::Ready(req) => { self.mark(&eid, &rid); if let Err(why) = self.begin(peer, &eid, req) { self.finish(peer, &rid, "error", None, None, None, Some(&why))?; } }
            }
        }
        let rids: Vec<String> = self.active.keys().cloned().collect();
        for r in rids { self.drain(peer, &r)?; }
        self.reap();
        if let Err(e) = peer.after(seen_removals) { if !self.reclaim_warned { self.reclaim_warned = true; eprintln!("graphlink-openai: reclaiming slots: {e}"); } }
        Ok(events)
    }

    fn mark(&mut self, eid: &str, rid: &str) { self.started.insert(eid.to_string()); self.finished.insert(eid.to_string(), rid.to_string()); }

    /// A request that is complete: its call starts.
    fn begin(&mut self, peer: &mut Peer, eid: &str, req: Req) -> Result<(), String> {
        let body = build_body(&self.cfg.model, &req.msgs, &req.specs, req.max_new)?;
        let key = self.key()?;
        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));
        let run = Run::spawn(&self.cfg.curl_bin, &url, key.as_deref(), &body.to_string())?;
        let r = self.reply(&req.rid);
        peer.publish(&r, "chunks", &json!(0))?;
        peer.publish(&r, "state", &json!("running"))?;
        self.active.insert(req.rid.clone(), Active { run, req: eid.to_string(), stream: Stream::default(), published: Vec::new(), status: None, body: String::new(), tail: String::new(), closed: 0, done: false });
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

    /// A call ends: SIGTERM, and curl is reaped as it goes.
    fn end(&mut self, rid: &str) {
        if let Some(a) = self.active.remove(rid) { a.run.term(); self.dying.push(Dying { run: a.run, deadline: Instant::now() + self.cfg.term_wait }); }
    }

    /// A call that is over by itself: reaped, not terminated.
    fn end_natural(&mut self, rid: &str) {
        if let Some(a) = self.active.remove(rid) { self.dying.push(Dying { run: a.run, deadline: Instant::now() + self.cfg.term_wait }); }
    }

    fn reap(&mut self) {
        self.dying.retain_mut(|d| match d.run.child.try_wait() {
            Ok(Some(_)) => false,
            _ => { if Instant::now() > d.deadline { eprintln!("graphlink-openai: a curl (pid {}) is still there after SIGTERM; not waiting for it (never SIGKILL)", d.run.child.id()); false } else { true } }
        });
    }

    /// The seat's end is gone: every call gets a SIGTERM, and this waits (at most `term_wait`) for the curls to be gone.
    pub fn shutdown(&mut self) {
        let rids: Vec<String> = self.active.keys().cloned().collect();
        for r in rids { self.end(&r); }
        let end = Instant::now() + self.cfg.term_wait;
        while !self.dying.is_empty() && Instant::now() < end { self.reap(); std::thread::sleep(Duration::from_millis(5)); }
    }

    fn drain(&mut self, peer: &mut Peer, rid: &str) -> Result<(), String> {
        loop {
            let Some(a) = self.active.get_mut(rid) else { return Ok(()) };
            let Ok(ev) = a.run.rx.try_recv() else { return Ok(()) };
            match self.event(peer, rid, ev)? {
                Next::Go => {}
                Next::Over => { self.end_natural(rid); return Ok(()); }
                Next::Kill => { self.end(rid); return Ok(()); }
            }
        }
    }

    fn fail(&mut self, peer: &mut Peer, rid: &str, why: &str) -> Result<Next, String> {
        let n = self.active.get(rid).map(|a| a.published.len());
        if let Some(a) = self.active.get_mut(rid) { a.done = true; }
        self.finish(peer, rid, "error", n, None, None, Some(why))?;
        Ok(Next::Kill)
    }

    fn event(&mut self, peer: &mut Peer, rid: &str, ev: Ev) -> Result<Next, String> {
        let cap = (peer.val_cap() as usize).saturating_sub(CBOR_TEXT_HEADER);
        let a = self.active.get_mut(rid).expect("active");
        match ev {
            Ev::Err(line) => { a.tail.push_str(&line); a.tail.push('\n'); a.tail = tail_of(&a.tail, TAIL); Ok(Next::Go) }
            Ev::Closed => {
                a.closed += 1;
                if a.closed < 2 { return Ok(Next::Go); }
                if a.done { return Ok(Next::Over); }
                if a.status.is_none() {
                    let rc = wait_code(&mut a.run.child, self.cfg.term_wait);
                    let tail = a.tail.trim().to_string();
                    let why = match rc {
                        Some(0) => format!("curl ended with no status line (rc 0){}", if tail.is_empty() { String::new() } else { format!(": {tail}") }),
                        Some(n) => format!("curl failed (rc {n}): {tail}"),
                        None => format!("curl ended without an exit code (killed by a signal): {tail}"),
                    };
                    return self.fail(peer, rid, &why);
                }
                self.complete(peer, rid)
            }
            Ev::Out(line) => {
                if let Some(data) = line.strip_prefix("data:") {
                    match a.stream.feed(data) {
                        Err(e) => return self.fail(peer, rid, &e),
                        Ok(grew) => {
                            if grew { if let Err(e) = self.publish_text(peer, rid, cap) { return self.fail(peer, rid, &e); } }
                            if self.active.get(rid).map(|a| a.stream.done).unwrap_or(false) { return self.complete(peer, rid); }
                        }
                    }
                } else if let Some(code) = line.strip_prefix(STATUS_MARK) {
                    a.status = code.trim().parse().ok().filter(|s: &u16| *s != 0);   // curl says 000 when it got no answer at all
                } else if !line.is_empty() && a.body.len() < 8 * TAIL {
                    a.body.push_str(&line); a.body.push('\n');
                }
                Ok(Next::Go)
            }
        }
    }

    /// The stream is over (`[DONE]`, or the call ended with a status): the reply is `done`, or an error that says why.
    fn complete(&mut self, peer: &mut Peer, rid: &str) -> Result<Next, String> {
        let a = self.active.get(rid).expect("active");
        if a.done { return Ok(Next::Over); }
        if let Some(s) = a.status { if !(200..300).contains(&s) { let why = http_error(s, &a.body); return self.fail(peer, rid, &why); } }
        let calls = match a.stream.calls() { Ok(c) => c, Err(e) => return self.fail(peer, rid, &e) };
        if a.stream.finish.is_none() && !a.stream.done {
            let body = tail_of(a.body.trim(), TAIL);
            let why = format!("the server's answer was not a finished stream{}", if body.is_empty() { String::new() } else { format!(": {body}") });
            return self.fail(peer, rid, &why);
        }
        let finish = if !calls.is_empty() { "tool_calls".to_string() } else { match a.stream.finish.as_deref() { Some(f @ ("length" | "content_filter")) => f.to_string(), _ => "stop".to_string() } };
        let (n, usage) = (a.published.len(), a.stream.usage.clone());
        self.active.get_mut(rid).expect("active").done = true;
        self.finish(peer, rid, "done", Some(n), usage, Some((&finish, calls)), None)?;
        Ok(Next::Over)
    }

    /// The text into chunk cells (the one being filled is rewritten as it grows); more than the four chunks the seat reads is an error.
    fn publish_text(&mut self, peer: &mut Peer, rid: &str, cap: usize) -> Result<(), String> {
        let a = self.active.get_mut(rid).expect("active");
        let pieces = cut(&a.stream.text, cap);
        if pieces.len() > MAX_CHUNKS { return Err(format!("the reply is {} bytes: more than the {MAX_CHUNKS} chunks of {cap} bytes the seat reads", a.stream.text.len())); }
        for (k, p) in pieces.iter().enumerate() {
            if a.published.get(k) != Some(p) {
                peer.publish(&format!("{}::Chunk:{rid}:{k}", self.cfg.ns), "text", &json!(p))?;
                if a.published.len() <= k { a.published.push(p.clone()); } else { a.published[k] = p.clone(); }
            }
        }
        peer.publish(&format!("{}::Reply:{rid}", self.cfg.ns), "chunks", &json!(a.published.len()))
    }
}

/// The end of `s`, at most `n` bytes, on a character boundary.
fn tail_of(s: &str, n: usize) -> String {
    if s.len() <= n { return s.to_string(); }
    let mut from = s.len() - n;
    while !s.is_char_boundary(from) { from += 1; }
    s[from..].to_string()
}

/// What a server's error body says: its `error.message` when it is the usual JSON, else the body itself (the end of it).
fn http_error(status: u16, body: &str) -> String {
    let said = serde_json::from_str::<J>(body.trim()).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or_else(|| body.trim().to_string());
    format!("the server answered HTTP {status}{}", if said.is_empty() { String::new() } else { format!(": {}", tail_of(&said, TAIL)) })
}

fn wait_code(child: &mut std::process::Child, wait: Duration) -> Option<i32> {
    let end = Instant::now() + wait;
    loop {
        if let Ok(Some(s)) = child.try_wait() { return s.code(); }
        if Instant::now() > end { unsafe { libc::kill(child.id() as i32, libc::SIGTERM); } return None; }
        std::thread::sleep(Duration::from_millis(2));
    }
}
