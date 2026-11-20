//! The wire of the Chat Completions API, the pure part: the body of a call built from the seat's messages, and the accumulation of the server's stream (server-sent events).
use serde_json::{json, Map, Value as J};
use std::collections::BTreeMap;

/// One message of the seat's history (`membrane::LlmMsg:<mid>:<i>`): `tool_calls` is what the seat wrote on an assistant message, `[{type, function: {name, arguments}}]`, without ids.
#[derive(Clone, Debug, PartialEq)]
pub struct Msg { pub role: String, pub content: String, pub tool_calls: Option<J> }

/// The body of one call. The API keeps nothing, so every call carries the whole history. The seat's assistant calls have no ids and the API wants one per call and the same on the tool
/// message that answers it: the ids are made here, `call_<message index>_<call index>`, and each `tool` message takes the next id not yet answered (the seat writes the results in the order of the calls).
pub fn build_body(model: &str, msgs: &[Msg], specs: &[J], max_new: Option<u64>) -> Result<J, String> {
    let mut out: Vec<J> = Vec::new();
    let mut pending: std::collections::VecDeque<String> = Default::default();
    for (i, m) in msgs.iter().enumerate() {
        match m.role.as_str() {
            "tool" => {
                let Some(id) = pending.pop_front() else { return Err(format!("message {i} is a tool result and no tool call is waiting for one")) };
                out.push(json!({"role": "tool", "tool_call_id": id, "content": m.content}));
            }
            "assistant" if m.tool_calls.is_some() => {
                let mut calls = Vec::new();
                for (j, c) in m.tool_calls.as_ref().and_then(J::as_array).into_iter().flatten().enumerate() {
                    let f = &c["function"];
                    let Some(name) = f["name"].as_str() else { return Err(format!("message {i}: tool call {j} has no function name")) };
                    let args = match &f["arguments"] { J::String(s) => s.clone(), J::Null => "{}".to_string(), other => other.to_string() };
                    let id = format!("call_{i}_{j}");
                    pending.push_back(id.clone());
                    calls.push(json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}}));
                }
                out.push(json!({"role": "assistant", "content": m.content, "tool_calls": calls}));
            }
            role => out.push(json!({"role": role, "content": m.content})),
        }
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("messages".into(), J::Array(out));
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    if !specs.is_empty() { body.insert("tools".into(), J::Array(specs.to_vec())); }
    if let Some(n) = max_new { body.insert("max_tokens".into(), json!(n)); }
    Ok(J::Object(body))
}

/// The usage of a call as the seat reads it: `read` the prompt tokens the model read fresh (the prompt less what came from the cache), `loaded` the cached part, `generated` the completion.
/// A key whose count the server did not give is left out; none at all is `None`.
pub fn usage_of(u: &J) -> Option<J> {
    let (prompt, cached, gen) = (u["prompt_tokens"].as_u64(), u["prompt_tokens_details"]["cached_tokens"].as_u64(), u["completion_tokens"].as_u64());
    let mut o = Map::new();
    if let Some(p) = prompt { o.insert("read".into(), json!(p.saturating_sub(cached.unwrap_or(0)))); }
    if let Some(c) = cached { o.insert("loaded".into(), json!(c)); }
    if let Some(g) = gen { o.insert("generated".into(), json!(g)); }
    if o.is_empty() { None } else { Some(J::Object(o)) }
}

/// What the server's stream has said so far.
#[derive(Default, Debug)]
pub struct Stream {
    pub text: String,
    /// by the call's index in the stream: (name, the arguments' JSON text so far)
    calls: BTreeMap<u64, (String, String)>,
    pub finish: Option<String>,
    pub usage: Option<J>,
    /// `[DONE]` came
    pub done: bool,
}

impl Stream {
    /// One `data:` payload. Returns whether the text grew; an error object or a payload that is not JSON is an error.
    pub fn feed(&mut self, data: &str) -> Result<bool, String> {
        let data = data.trim();
        if data == "[DONE]" { self.done = true; return Ok(false); }
        let v: J = serde_json::from_str(data).map_err(|_| format!("the stream sent a line that is not JSON: {}", data.chars().take(200).collect::<String>()))?;
        if v.get("error").map(|e| !e.is_null()).unwrap_or(false) {
            let e = &v["error"];
            return Err(format!("the server's stream carried an error: {}", e["message"].as_str().map(String::from).unwrap_or_else(|| e.to_string())));
        }
        let mut grew = false;
        if let Some(c) = v["choices"].as_array().and_then(|a| a.first()) {
            if let Some(t) = c["delta"]["content"].as_str() { if !t.is_empty() { self.text.push_str(t); grew = true; } }
            for tc in c["delta"]["tool_calls"].as_array().into_iter().flatten() {
                let e = self.calls.entry(tc["index"].as_u64().unwrap_or(0)).or_default();
                if let Some(n) = tc["function"]["name"].as_str() { e.0.push_str(n); }
                if let Some(a) = tc["function"]["arguments"].as_str() { e.1.push_str(a); }
            }
            if let Some(f) = c["finish_reason"].as_str() { self.finish = Some(f.to_string()); }
        }
        if v["usage"].is_object() { if let Some(u) = usage_of(&v["usage"]) { self.usage = Some(u); } }
        Ok(grew)
    }

    /// The calls as the seat reads them, `[{name, arguments}]` with the arguments as a JSON value; arguments that are not JSON are an error, not a guess.
    pub fn calls(&self) -> Result<Vec<J>, String> {
        self.calls.values().map(|(name, args)| {
            let arguments = if args.trim().is_empty() { json!({}) } else {
                serde_json::from_str::<J>(args).map_err(|_| format!("the arguments of the call to {name} are not JSON: {}", args.chars().take(200).collect::<String>()))?
            };
            Ok(json!({"name": name, "arguments": arguments}))
        }).collect()
    }
}
