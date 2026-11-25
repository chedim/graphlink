//! The turn handler (docs/claude-peer.md): the seat's chat requests answered by `claude` children, one child per turn (the mid of the request id `<mid>-<n>`).
//! Sans-I/O like the peer: `tick` is called by whoever owns the thread and never blocks (except at the end of a child, for as long as `term_wait` allows).
//! Round 1 starts the child; a round that ends in tool calls leaves the child HOLDING them (the MCP shim waits); round n+1, whose last messages are the tool results, hands them
//! to the held calls and the child goes on; the next text or tool call is Reply n+1.
use crate::peer::{Peer, Seen};
use crate::session::{Ev, Session};
use graphlink_core::frame::Target;
use graphlink_core::link::Event;
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const REQUEST: &str = "membrane::LlmRequest:";
const MSG: &str = "membrane::LlmMsg:";
const TOOL: &str = "membrane::LlmTool:";
/// The models this peer serves, published on `<ns>::Models:main` (`names`, `backend`) so a seat routes these names here without a `cloud_models` entry: `claude` is the
/// owner's `--model` (what a request with no model runs too), the others are the CLI's own aliases, handed to it as `--model <alias>`.
pub const MODELS: [&str; 4] = ["claude", "sonnet", "opus", "haiku"];
/// The backend a seat files these models under (its `cloud_models` name for a Claude Code peer).
pub const BACKEND: &str = "claude-peer";
/// The header of a CBOR text string of 256 to 65535 bytes: a cell's value cap holds the whole item, so a chunk is `val_cap` minus this.
const CBOR_TEXT_HEADER: usize = 3;
/// The seat reads a reply of up to four chunks (packs/chat_membrama.c1f, `chunks_in`).
pub const MAX_CHUNKS: usize = 4;
/// How much of a child's stderr an error reply carries (the end of it).
const STDERR_TAIL: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Auth { ApiKey, Login }

#[derive(Clone, Debug)]
pub struct Config {
    /// The namespace this peer owns.
    pub ns: String,
    pub claude_bin: String,
    pub model: String,
    pub auth: Auth,
    /// How long to wait for a child to be gone after a SIGTERM.
    pub term_wait: Duration,
    /// This program, which Claude spawns as its MCP server (`<exe> mcp --socket <path>`).
    pub exe: PathBuf,
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

struct Round { n: u32, rid: String, text: String, published: Vec<String>, usage: BTreeMap<String, J>, done: bool }
impl Round {
    fn new(n: u32, rid: String) -> Round { Round { n, rid, text: String::new(), published: Vec::new(), usage: BTreeMap::new(), done: false } }
}

struct Active {
    session: Session,
    req: String,
    round: Round,
    /// the calls the shim holds in this step: its ids
    held: Vec<u64>,
    /// the same calls as the reply says them: name and arguments, in the order they arrived
    calls: Vec<J>,
    /// the round ended in tool calls and the child waits for their results
    awaiting: bool,
    closed: u8,
    tail: String,
}

struct Dying { session: Session, deadline: Instant }

pub struct Handler {
    cfg: Config,
    subscribed: bool,
    started: HashSet<String>,
    /// request node -> its id, for the requests that were answered: when the seat retires the request, the reply and its chunks are retired
    finished: HashMap<String, String>,
    active: HashMap<String, Active>,
    dying: Vec<Dying>,
    reclaim_warned: bool,
    /// the models node is published (once the link is open)
    announced: bool,
}

/// the characters kept at each end of a tool result handed to the CLI: the number the seat's door used (`mc_w_cap`, 8192, operator 2026-11-13), moved here
const RESULT_CAP: usize = 8192;

struct Req { rid: String, mid: String, n: u32, model: Option<String>, system: String, user: Option<String>, specs: Vec<J>, results: Vec<String> }
enum Read { Wait, Aborted, Failed(String), Ready(Req) }

/// The alias the CLI runs for the model a request names: none or `claude` is the owner's `--model`; another of `MODELS` is itself; anything else is refused.
pub fn alias(owners: &str, asked: Option<&str>) -> Result<String, String> {
    match asked {
        None | Some("claude") => Ok(owners.to_string()),
        Some(m) if MODELS.contains(&m) => Ok(m.to_string()),
        Some(m) => Err(format!("this peer does not serve the model {m}: it serves {}", MODELS.join(", "))),
    }
}

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
    let Some((mid, n)) = rid.rsplit_once('-').and_then(|(m, n)| n.parse::<u32>().ok().map(|n| (m.to_string(), n))) else {
        return Ok(Read::Failed(format!("the request id {rid} is not <turn>-<round>")));
    };
    let ids = need!(peer, eid, "messages");
    let mut msgs: Vec<(String, String)> = Vec::new();
    for mid_ in ids.as_array().into_iter().flatten().filter_map(J::as_str) {
        let role = need!(peer, mid_, "role"); let content = need!(peer, mid_, "content");
        let Some(text) = content.as_str() else { return Ok(Read::Failed(format!("{mid_} content: it must be a string"))) };
        msgs.push((role.as_str().unwrap_or("").to_string(), text.to_string()));
    }
    let mut specs = Vec::new();
    if let Some(ids) = maybe!(peer, eid, "tools") {
        for tid in ids.as_array().into_iter().flatten().filter_map(J::as_str) { specs.push(need!(peer, tid, "spec")); }
    }
    let model = match maybe!(peer, eid, "model") { None | Some(J::Null) => None, Some(J::String(m)) => Some(m), Some(other) => return Ok(Read::Failed(format!("{eid} model: it must be a string, not {other}"))) };
    let system = msgs.iter().filter(|(r, _)| r == "system").map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n\n");
    let user = msgs.iter().rev().find(|(r, _)| r == "user").map(|(_, t)| t.clone());
    let mut results: Vec<String> = msgs.iter().rev().take_while(|(r, _)| r == "tool").map(|(_, t)| t.clone()).collect();
    results.reverse();
    Ok(Read::Ready(Req { rid: rid.to_string(), mid, n, model, system, user, specs, results }))
}

impl Handler {
    pub fn new(cfg: Config) -> Handler {
        Handler { cfg, subscribed: false, started: HashSet::new(), finished: HashMap::new(), active: HashMap::new(), dying: Vec::new(), reclaim_warned: false, announced: false }
    }

    fn reply(&self, rid: &str) -> String { format!("{}::Reply:{rid}", self.cfg.ns) }

    /// What an error may say: the API key, if it is in the text, is not.
    fn mask(&self, text: &str) -> String {
        match std::env::var("ANTHROPIC_API_KEY") { Ok(k) if !k.is_empty() => text.replace(&k, "[key]"), _ => text.to_string() }
    }

    /// Takes what the seat wrote, starts the runs that are complete, moves the children's output into replies; returns the peer's events.
    pub fn tick(&mut self, peer: &mut Peer) -> Result<Vec<Event>, String> {
        let events = peer.poll()?;
        let seen_removals = peer.before();
        if !self.subscribed && peer.is_open() {
            for p in [REQUEST, MSG, TOOL] { peer.subscribe(Target::Prefix(p.into()), None)?; }
            self.subscribed = true;
        }
        if !self.announced && peer.is_open() {
            let node = format!("{}::Models:main", self.cfg.ns);
            peer.publish(&node, "names", &json!(MODELS))?;
            peer.publish(&node, "backend", &json!(BACKEND))?;
            self.announced = true;
        }
        let ids: BTreeMap<String, String> = peer.scan(REQUEST)?.into_iter().map(|(eid, _, _)| { let id = eid[REQUEST.len()..].to_string(); (eid, id) }).collect();
        // a request the seat retired: its reply and chunks are retired, and a run waiting on it is ended
        let retired: Vec<String> = self.finished.keys().filter(|e| !ids.contains_key(*e)).cloned().collect();
        for e in retired {
            if let Some(id) = self.finished.remove(&e) { peer.retire(&self.reply(&id))?; peer.retire_prefix(&format!("{}::Chunk:{id}:", self.cfg.ns))?; }
            self.started.remove(&e);
        }
        let gone: Vec<String> = self.active.iter().filter(|(_, a)| !ids.contains_key(&a.req)).map(|(m, _)| m.clone()).collect();
        for m in gone { self.end(&m); }
        // an abort of the request a run is on
        let aborted: Vec<String> = self.active.iter().filter(|(_, a)| peer.read(&a.req, "abort").ok().flatten().as_ref().map(|v| truthy(Some(v))).unwrap_or(false)).map(|(m, _)| m.clone()).collect();
        for m in aborted {
            if let Some(a) = self.active.get(&m) { if !a.round.done { let rid = a.round.rid.clone(); self.finish(peer, &rid, "aborted", None, None, None, None)?; } }
            self.end(&m);
        }
        for (eid, rid) in ids {
            if self.started.contains(&eid) { continue; }
            match read_request(peer, &eid, &rid)? {
                Read::Wait => {}
                Read::Aborted => { self.mark(&eid, &rid); self.finish(peer, &rid, "aborted", None, None, None, None)?; }
                Read::Failed(why) => { self.mark(&eid, &rid); self.finish(peer, &rid, "error", None, None, None, Some(&why))?; }
                Read::Ready(req) => { self.mark(&eid, &rid); if let Err(why) = self.begin(peer, &eid, req) { self.finish(peer, &rid, "error", None, None, None, Some(&why))?; } }
            }
        }
        let mids: Vec<String> = self.active.keys().cloned().collect();
        for m in mids { self.drain(peer, &m)?; }
        self.reap();
        if let Err(e) = peer.after(seen_removals) { if !self.reclaim_warned { self.reclaim_warned = true; eprintln!("graphlink-claude: reclaiming slots: {e}"); } }
        Ok(events)
    }

    fn mark(&mut self, eid: &str, rid: &str) { self.started.insert(eid.to_string()); self.finished.insert(eid.to_string(), rid.to_string()); }

    /// A request that is complete: round 1 starts the child, a later round answers the calls it holds.
    fn begin(&mut self, peer: &mut Peer, eid: &str, req: Req) -> Result<(), String> {
        if req.n == 1 {
            if self.active.contains_key(&req.mid) { return Err(format!("the turn {} already has a run", req.mid)); }
            let Some(user) = req.user.clone() else { return Err("the first round has no user message".into()) };
            let mut cfg = self.cfg.clone();
            cfg.model = alias(&self.cfg.model, req.model.as_deref())?;
            let session = Session::spawn(&cfg, &req.system, &user, req.specs.clone())?;
            self.start_round(peer, &req.rid)?;
            self.active.insert(req.mid.clone(), Active { session, req: eid.to_string(), round: Round::new(1, req.rid.clone()), held: Vec::new(), calls: Vec::new(), awaiting: false, closed: 0, tail: String::new() });
            return Ok(());
        }
        let Some(a) = self.active.get_mut(&req.mid) else {
            return Err(format!("no run for the turn {}: a held tool call lives only in the run this peer started (it may have been restarted), so round {} cannot continue it", req.mid, req.n));
        };
        if !a.awaiting || a.round.n + 1 != req.n { return Err(format!("round {} does not follow round {} of the turn {}", req.n, a.round.n, req.mid)); }
        if req.results.len() != a.held.len() {
            return Err(format!("round {} ends with {} tool results but the run holds {} calls", req.n, req.results.len(), a.held.len()));
        }
        for (id, text) in a.held.iter().zip(req.results.iter()) { a.session.answer(*id, &crate::cap::cap_result(text, RESULT_CAP), false)?; }
        a.held.clear(); a.calls.clear(); a.awaiting = false;
        a.req = eid.to_string(); a.round = Round::new(req.n, req.rid.clone());
        self.start_round(peer, &req.rid)
    }

    fn start_round(&self, peer: &mut Peer, rid: &str) -> Result<(), String> {
        let r = self.reply(rid);
        peer.publish(&r, "chunks", &json!(0))?;
        peer.publish(&r, "state", &json!("running"))
    }

    /// The last words of a reply. `state` is written last: the seat reads the rest once it says done.
    fn finish(&self, peer: &mut Peer, rid: &str, state: &str, chunks: Option<usize>, usage: Option<J>, tool_calls: Option<(&str, Vec<J>)>, error: Option<&str>) -> Result<(), String> {
        let r = self.reply(rid);
        if let Some(c) = chunks { peer.publish(&r, "chunks", &json!(c))?; } else { peer.publish(&r, "chunks", &json!(0))?; }
        if let Some((finish, calls)) = &tool_calls {
            if !calls.is_empty() { peer.publish(&r, "tool_calls", &J::Array(calls.clone()))?; }
            peer.publish(&r, "finish", &json!(finish))?;
        }
        if let Some(u) = usage { peer.publish(&r, "usage", &u)?; }
        if let Some(e) = error { peer.publish(&r, "error", &json!(self.mask(e)))?; }
        peer.publish(&r, "state", &json!(state))
    }

    /// A run ends: SIGTERM, and the child is reaped as it goes.
    fn end(&mut self, mid: &str) {
        if let Some(a) = self.active.remove(mid) {
            a.session.term();
            self.dying.push(Dying { session: a.session, deadline: Instant::now() + self.cfg.term_wait });
        }
    }

    fn reap(&mut self) {
        self.dying.retain_mut(|d| match d.session.child.try_wait() {
            Ok(Some(_)) => false,
            _ => { if Instant::now() > d.deadline { eprintln!("graphlink-claude: a claude child (pid {}) is still there after SIGTERM; not waiting for it (never SIGKILL)", d.session.child.id()); false } else { true } }
        });
    }

    /// The seat's end is gone: every child gets a SIGTERM, and this waits (at most `term_wait`) for them to be gone.
    pub fn shutdown(&mut self) {
        let mids: Vec<String> = self.active.keys().cloned().collect();
        for m in mids { self.end(&m); }
        let end = Instant::now() + self.cfg.term_wait;
        while !self.dying.is_empty() && Instant::now() < end {
            self.reap();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// What a child said since the last tick.
    fn drain(&mut self, peer: &mut Peer, mid: &str) -> Result<(), String> {
        loop {
            let Some(a) = self.active.get_mut(mid) else { return Ok(()) };
            let Ok(ev) = a.session.rx.try_recv() else { return Ok(()) };
            let over = self.event(peer, mid, ev)?;
            if over { self.end_natural(mid); return Ok(()); }
        }
    }

    /// The child is over (it said its last or its streams closed): it is reaped, not terminated.
    fn end_natural(&mut self, mid: &str) {
        if let Some(a) = self.active.remove(mid) {
            self.dying.push(Dying { session: a.session, deadline: Instant::now() + self.cfg.term_wait });
        }
    }

    /// One event of a run; true when the run is over.
    fn event(&mut self, peer: &mut Peer, mid: &str, ev: Ev) -> Result<bool, String> {
        let cap = (peer.val_cap() as usize).saturating_sub(CBOR_TEXT_HEADER);
        let a = self.active.get_mut(mid).expect("active");
        let rid = a.round.rid.clone();
        match ev {
            Ev::Err(line) => {
                a.tail.push_str(&line); a.tail.push('\n');
                if a.tail.len() > STDERR_TAIL { let mut from = a.tail.len() - STDERR_TAIL; while !a.tail.is_char_boundary(from) { from += 1; } a.tail = a.tail[from..].to_string(); }
                Ok(false)
            }
            Ev::Closed => {
                a.closed += 1;
                if a.closed < 2 { return Ok(false); }
                if a.round.done { return Ok(true); }   // it ended while waiting for a result: the next request says there is no run
                let rc = wait_code(&mut a.session.child, self.cfg.term_wait);
                let tail = a.tail.trim().to_string();
                let why = match rc {
                    Some(0) => format!("claude ended with no result line (rc 0){}", if tail.is_empty() { String::new() } else { format!(": {tail}") }),
                    Some(n) => format!("claude failed (rc {n}): {tail}"),
                    None => format!("claude ended without an exit code (killed by a signal): {tail}"),
                };
                let n = a.round.published.len();
                self.finish(peer, &rid, "error", Some(n), None, None, Some(&why))?;
                Ok(true)
            }
            Ev::Call { id, .. } if a.round.done && !a.awaiting => { let _ = a.session.answer(id, "this run has ended", true); Ok(false) }
            // the CLI calls MCP tools one at a time (measured), so a call that comes while the round has already ended with the first one is not the usual: it is answered at once with what to do, never held
            Ev::Call { id, .. } if a.round.done => { let _ = a.session.answer(id, "this run takes one tool call at a time: its first call of this step is being run; call this tool again after that result", true); Ok(false) }
            Ev::Call { id, name, args } => {
                a.held.push(id);
                a.calls.push(json!({"name": name, "arguments": if args.is_null() { json!({}) } else { args }}));
                self.maybe_calls(peer, mid, cap)
            }
            Ev::Out(line) => {
                let Ok(v) = serde_json::from_str::<J>(&line) else { return Ok(false) };
                match v["type"].as_str() {
                    Some("assistant") => {
                        let msg = &v["message"];
                        if let Some(id) = msg["id"].as_str() { if msg["usage"].is_object() { a.round.usage.insert(id.to_string(), msg["usage"].clone()); } }
                        for b in msg["content"].as_array().into_iter().flatten() {
                            match b["type"].as_str() {
                                Some("text") => { let t = b["text"].as_str().unwrap_or(""); if !t.is_empty() { if !a.round.text.is_empty() { a.round.text.push_str("\n\n"); } a.round.text.push_str(t); } }
                                _ => {}
                            }
                        }
                        if let Err(e) = self.publish_text(peer, mid, cap) { self.finish(peer, &rid, "error", None, None, None, Some(&e))?; return Ok(true); }
                        self.maybe_calls(peer, mid, cap)
                    }
                    Some("result") => {
                        if a.round.done { return Ok(false); }
                        if v["is_error"] == json!(true) {
                            let why = format!("claude reported an error: {}", v["result"].as_str().filter(|s| !s.is_empty()).or(v["subtype"].as_str()).unwrap_or("no reason given"));
                            let n = a.round.published.len();
                            self.finish(peer, &rid, "error", Some(n), None, None, Some(&why))?;
                            return Ok(true);
                        }
                        if a.round.text.is_empty() { if let Some(t) = v["result"].as_str() { a.round.text = t.to_string(); } }
                        if let Err(e) = self.publish_text(peer, mid, cap) { self.finish(peer, &rid, "error", None, None, None, Some(&e))?; return Ok(true); }
                        let a = self.active.get_mut(mid).expect("active");
                        a.round.done = true;
                        let (n, usage) = (a.round.published.len(), usage_of(&a.round.usage));
                        self.finish(peer, &rid, "done", Some(n), usage, Some(("stop", Vec::new())), None)?;
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            }
        }
    }

    /// The round's text into chunk cells (the one being filled is rewritten as it grows); more than the four chunks the seat reads is an error.
    fn publish_text(&mut self, peer: &mut Peer, mid: &str, cap: usize) -> Result<(), String> {
        let a = self.active.get_mut(mid).expect("active");
        let pieces = cut(&a.round.text, cap);
        if pieces.len() > MAX_CHUNKS { return Err(format!("the reply is {} bytes: more than the {MAX_CHUNKS} chunks of {cap} bytes the seat reads", a.round.text.len())); }
        let rid = a.round.rid.clone();
        for (k, p) in pieces.iter().enumerate() {
            if a.round.published.get(k) != Some(p) {
                peer.publish(&format!("{}::Chunk:{rid}:{k}", self.cfg.ns), "text", &json!(p))?;
                if a.round.published.len() <= k { a.round.published.push(p.clone()); } else { a.round.published[k] = p.clone(); }
            }
        }
        let n = a.round.published.len();
        peer.publish(&format!("{}::Reply:{rid}", self.cfg.ns), "chunks", &json!(n))
    }

    /// The round ends with the FIRST tool call the shim holds: the CLI calls MCP tools one at a time (with two `tool_use` blocks in one message the second call comes only after the first
    /// was answered), so waiting for every announced call waits for ever. The seat runs the call and its next round carries the result; the second call is that round's.
    fn maybe_calls(&mut self, peer: &mut Peer, mid: &str, _cap: usize) -> Result<bool, String> {
        let a = self.active.get_mut(mid).expect("active");
        if a.round.done || a.held.is_empty() { return Ok(false); }
        a.round.done = true; a.awaiting = true;
        let calls = std::mem::take(&mut a.calls);
        let (rid, n, usage) = (a.round.rid.clone(), a.round.published.len(), usage_of(&a.round.usage));
        self.finish(peer, &rid, "done", Some(n), usage, Some(("tool_calls", calls)), None)?;
        Ok(false)
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

/// The usage of a round: `read` the tokens the model read fresh (input, and written to the prompt cache), `loaded` the tokens served from the prompt cache, `generated` the
/// output tokens; each only when the CLI said it. Summed over the round's assistant messages (one per message id). None when no message carried usage.
pub fn usage_of(by_message: &BTreeMap<String, J>) -> Option<J> {
    let (mut read, mut loaded, mut gen) = (None::<u64>, None::<u64>, None::<u64>);
    let add = |slot: &mut Option<u64>, v: Option<u64>| if let Some(v) = v { *slot = Some(slot.unwrap_or(0) + v); };
    let mut any = false;
    for (k, u) in by_message {
        let _ = k;
        if !u.is_object() { continue; }
        any = true;
        let (i, w) = (u["input_tokens"].as_u64(), u["cache_creation_input_tokens"].as_u64());
        if i.is_some() || w.is_some() { add(&mut read, Some(i.unwrap_or(0) + w.unwrap_or(0))); }
        add(&mut loaded, u["cache_read_input_tokens"].as_u64());
        add(&mut gen, u["output_tokens"].as_u64());
    }
    if !any { return None; }
    let mut o = serde_json::Map::new();
    if let Some(v) = read { o.insert("read".into(), json!(v)); }
    if let Some(v) = loaded { o.insert("loaded".into(), json!(v)); }
    if let Some(v) = gen { o.insert("generated".into(), json!(v)); }
    if o.is_empty() { None } else { Some(J::Object(o)) }
}
