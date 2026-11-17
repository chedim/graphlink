//! Frames and version negotiation (docs/frames.md): the unit of ordering of the graph link and the ops it carries, as plain data. No I/O: a frame comes from JSON
//! (`Frame::decode`) and goes back to JSON (`Frame::encode`); what carries it is a transport's business.
use std::collections::BTreeMap;

use serde_json::{json, Map, Value as J};

/// The protocol version this crate writes by default.
pub const VERSION: u32 = 1;

/// What a `sub` or `unsub` names: one entity, or every entity under a prefix.
#[derive(Clone, Debug, PartialEq)]
pub enum Target { Id(String), Prefix(String) }

/// What a query selects: the entities under a prefix, or the entities of a class.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryBy { Prefix(String), Class(String) }

/// What a `val` says about its node.
#[derive(Clone, Debug, PartialEq)]
pub enum ValBody { Props(J), Gone, Refused(String) }

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Hello { ns: String, build: String, epoch: f64, have: BTreeMap<String, f64>, support: Vec<u32> },
    Failed { support: Vec<u32>, why: String },
    Sub { target: Target, props: Option<Vec<String>>, limit: Option<u64> },
    Unsub { target: Target },
    Val { id: String, r: f64, body: ValBody },
    Read { id: String, props: Vec<String>, rid: String },
    Wr { wid: String, id: String, prop: String, value: J },
    /// `refused: None` is `ok`
    Res { wid: String, refused: Option<String> },
    Bye { why: String },
    /// a paged query, answered by `Page` with the same `qid`
    Query { qid: String, by: QueryBy, after: Option<String>, limit: Option<u64> },
    Page { qid: String, rows: Vec<J>, next: Option<String>, total: Option<u64> },
    /// after a reconnect: the revision held per subscribed id, so the owner sends only what differs
    Resync { have: BTreeMap<String, f64> },
    /// the peer is hidden: the owner stops sending `val` and keeps only revisions; `Resync` wakes it
    Pause { have: BTreeMap<String, f64> },
    /// an op this version does not know, kept as it came
    Unknown(J),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame { pub v: u32, pub n: u64, pub ack: u64, pub ops: Vec<Op> }

/// The outcome of comparing the versions two peers speak.
#[derive(Clone, Debug, PartialEq)]
pub enum Negotiated { Speak(u32), Failed(Op) }

/// The highest version both lists contain; with none in common, the `failed` op to answer with (our list, sorted).
pub fn negotiate(ours: &[u32], theirs: &[u32]) -> Negotiated {
    let mut o = ours.to_vec(); o.sort_unstable(); o.dedup();
    let mut t = theirs.to_vec(); t.sort_unstable(); t.dedup();
    match o.iter().rev().find(|v| t.contains(v)) {
        Some(v) => Negotiated::Speak(*v),
        None => Negotiated::Failed(Op::Failed { why: format!("no protocol version in common: we speak {o:?}, you speak {t:?}"), support: o }),
    }
}

fn text(m: &Map<String, J>, op: &str, key: &str) -> Result<String, String> {
    m.get(key).and_then(J::as_str).map(str::to_string).ok_or_else(|| format!("`{op}` needs `{key}` (a string)"))
}
fn number(m: &Map<String, J>, op: &str, key: &str) -> Result<f64, String> {
    m.get(key).and_then(J::as_f64).ok_or_else(|| format!("`{op}` needs `{key}` (a number)"))
}
fn strings(v: &J, op: &str, key: &str) -> Result<Vec<String>, String> {
    v.as_array().and_then(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect()).ok_or_else(|| format!("`{op}` needs `{key}` (a list of strings)"))
}
fn target(m: &Map<String, J>, op: &str) -> Result<Target, String> {
    match (m.get("id").and_then(J::as_str), m.get("prefix").and_then(J::as_str)) {
        (Some(id), None) => Ok(Target::Id(id.to_string())),
        (None, Some(p)) => Ok(Target::Prefix(p.to_string())),
        _ => Err(format!("`{op}` needs exactly one of `id` and `prefix`")),
    }
}
fn have(m: &Map<String, J>, op: &str) -> Result<BTreeMap<String, f64>, String> {
    m.get("have").and_then(J::as_object).ok_or_else(|| format!("`{op}` needs `have` (an object)"))?.iter()
        .map(|(k, v)| v.as_f64().map(|r| (k.clone(), r)).ok_or_else(|| format!("`{op}` `have` maps ids to numbers"))).collect()
}
fn put_have(m: &mut Map<String, J>, h: &BTreeMap<String, f64>) { m.insert("have".into(), J::Object(h.iter().map(|(k, r)| (k.clone(), json!(r))).collect())); }
fn put_target(m: &mut Map<String, J>, t: &Target) {
    match t { Target::Id(i) => { m.insert("id".into(), json!(i)); } Target::Prefix(p) => { m.insert("prefix".into(), json!(p)); } }
}

impl Op {
    fn decode(j: &J) -> Result<Op, String> {
        let m = j.as_object().ok_or("an op is a JSON object")?;
        let name = m.get("op").and_then(J::as_str).ok_or("an op needs `op` (a string)")?;
        Ok(match name {
            "hello" => {
                let support: Vec<u32> = m.get("support").and_then(J::as_array).map(|a| a.iter().filter_map(|x| x.as_u64().map(|x| x as u32)).collect()).unwrap_or_default();
                if support.is_empty() { return Err("`hello` needs `support`, a non-empty list of versions".into()); }
                let have = have(m, name)?;
                Op::Hello { ns: text(m, name, "ns")?, build: text(m, name, "build")?, epoch: number(m, name, "epoch")?, have, support }
            }
            "failed" => {
                let support = m.get("support").and_then(J::as_array).map(|a| a.iter().filter_map(|x| x.as_u64().map(|x| x as u32)).collect()).ok_or("`failed` needs `support` (a list)")?;
                Op::Failed { support, why: text(m, name, "why")? }
            }
            "sub" => Op::Sub {
                target: target(m, name)?,
                props: match m.get("props") { Some(p) => Some(strings(p, name, "props")?), None => None },
                limit: m.get("limit").and_then(J::as_u64),
            },
            "unsub" => Op::Unsub { target: target(m, name)? },
            "val" => {
                let body = if let Some(p) = m.get("props") { ValBody::Props(p.clone()) }
                    else if m.get("gone").and_then(J::as_bool) == Some(true) { ValBody::Gone }
                    else if let Some(why) = m.get("refused").and_then(J::as_str) { ValBody::Refused(why.to_string()) }
                    else { return Err("`val` needs `props`, `gone` or `refused`".into()); };
                Op::Val { id: text(m, name, "id")?, r: number(m, name, "r")?, body }
            }
            "read" => Op::Read { id: text(m, name, "id")?, props: strings(m.get("props").unwrap_or(&J::Null), name, "props")?, rid: text(m, name, "rid")? },
            "wr" => Op::Wr { wid: text(m, name, "wid")?, id: text(m, name, "id")?, prop: text(m, name, "prop")?, value: m.get("value").cloned().ok_or("`wr` needs `value`")? },
            "res" => {
                let refused = match (m.get("refused").and_then(J::as_str), m.get("ok").and_then(J::as_bool)) {
                    (Some(why), _) => Some(why.to_string()),
                    (None, Some(true)) => None,
                    _ => return Err("`res` needs `ok` or `refused`".into()),
                };
                Op::Res { wid: text(m, name, "wid")?, refused }
            }
            "bye" => Op::Bye { why: text(m, name, "why")? },
            "q" => {
                let by = match (m.get("prefix").and_then(J::as_str), m.get("class").and_then(J::as_str)) {
                    (Some(p), None) => QueryBy::Prefix(p.to_string()),
                    (None, Some(c)) => QueryBy::Class(c.to_string()),
                    _ => return Err("`q` needs exactly one of `prefix` and `class`".into()),
                };
                Op::Query { qid: text(m, name, "qid")?, by, after: m.get("after").and_then(J::as_str).map(str::to_string), limit: m.get("limit").and_then(J::as_u64) }
            }
            "page" => Op::Page {
                qid: text(m, name, "qid")?,
                rows: m.get("rows").and_then(J::as_array).cloned().ok_or("`page` needs `rows` (an array)")?,
                next: m.get("next").and_then(J::as_str).map(str::to_string),
                total: m.get("total").and_then(J::as_u64),
            },
            "resync" => Op::Resync { have: have(m, name)? },
            "pause" => Op::Pause { have: have(m, name)? },
            _ => Op::Unknown(j.clone()),
        })
    }

    fn encode(&self) -> J {
        let mut m = Map::new();
        let op = |name: &str, m: &mut Map<String, J>| { let mut o = Map::new(); o.insert("op".into(), json!(name)); o.extend(std::mem::take(m)); J::Object(o) };
        match self {
            Op::Hello { ns, build, epoch, have, support } => {
                m.insert("ns".into(), json!(ns)); m.insert("build".into(), json!(build)); m.insert("epoch".into(), json!(epoch));
                put_have(&mut m, have); m.insert("support".into(), json!(support));
                op("hello", &mut m)
            }
            Op::Failed { support, why } => { m.insert("support".into(), json!(support)); m.insert("why".into(), json!(why)); op("failed", &mut m) }
            Op::Sub { target, props, limit } => {
                put_target(&mut m, target);
                if let Some(p) = props { m.insert("props".into(), json!(p)); }
                if let Some(l) = limit { m.insert("limit".into(), json!(l)); }
                op("sub", &mut m)
            }
            Op::Unsub { target } => { put_target(&mut m, target); op("unsub", &mut m) }
            Op::Val { id, r, body } => {
                m.insert("id".into(), json!(id)); m.insert("r".into(), json!(r));
                match body { ValBody::Props(p) => { m.insert("props".into(), p.clone()); } ValBody::Gone => { m.insert("gone".into(), json!(true)); } ValBody::Refused(w) => { m.insert("refused".into(), json!(w)); } }
                op("val", &mut m)
            }
            Op::Read { id, props, rid } => { m.insert("id".into(), json!(id)); m.insert("props".into(), json!(props)); m.insert("rid".into(), json!(rid)); op("read", &mut m) }
            Op::Wr { wid, id, prop, value } => { m.insert("wid".into(), json!(wid)); m.insert("id".into(), json!(id)); m.insert("prop".into(), json!(prop)); m.insert("value".into(), value.clone()); op("wr", &mut m) }
            Op::Res { wid, refused } => {
                m.insert("wid".into(), json!(wid));
                match refused { Some(w) => { m.insert("refused".into(), json!(w)); } None => { m.insert("ok".into(), json!(true)); } }
                op("res", &mut m)
            }
            Op::Bye { why } => { m.insert("why".into(), json!(why)); op("bye", &mut m) }
            Op::Query { qid, by, after, limit } => {
                m.insert("qid".into(), json!(qid));
                match by { QueryBy::Prefix(p) => { m.insert("prefix".into(), json!(p)); } QueryBy::Class(c) => { m.insert("class".into(), json!(c)); } }
                if let Some(a) = after { m.insert("after".into(), json!(a)); }
                if let Some(l) = limit { m.insert("limit".into(), json!(l)); }
                op("q", &mut m)
            }
            Op::Page { qid, rows, next, total } => {
                m.insert("qid".into(), json!(qid)); m.insert("rows".into(), json!(rows));
                if let Some(n) = next { m.insert("next".into(), json!(n)); }
                if let Some(t) = total { m.insert("total".into(), json!(t)); }
                op("page", &mut m)
            }
            Op::Resync { have } => { put_have(&mut m, have); op("resync", &mut m) }
            Op::Pause { have } => { put_have(&mut m, have); op("pause", &mut m) }
            Op::Unknown(j) => j.clone(),
        }
    }
}

impl Frame {
    pub fn decode(j: &J) -> Result<Frame, String> {
        let m = j.as_object().ok_or("a frame is a JSON object")?;
        let num = |k: &str| m.get(k).and_then(J::as_u64).ok_or_else(|| format!("a frame needs `{k}`, a non-negative integer"));
        let ops = m.get("ops").and_then(J::as_array).ok_or("a frame needs `ops` (an array)")?.iter().map(Op::decode).collect::<Result<Vec<_>, _>>()?;
        Ok(Frame { v: num("v")? as u32, n: num("n")?, ack: num("ack")?, ops })
    }
    pub fn encode(&self) -> J { json!({"v": self.v, "n": self.n, "ack": self.ack, "ops": self.ops.iter().map(Op::encode).collect::<Vec<_>>()}) }
}
