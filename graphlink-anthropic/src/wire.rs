//! The seat's conversation as a Messages API body. The API keeps no history, so every round's call is built from the whole of it: the seat's `LlmMsg` nodes in order.
//! System messages become `system`; user and assistant text become text blocks; an assistant message's `tool_calls` become `tool_use` blocks, and the `tool` messages that follow
//! (the seat's carry no call id: they answer the calls in order) become `tool_result` blocks of ONE user message with the id of the call they answer.
use serde_json::{json, Value as J};

/// One message as the seat wrote it.
#[derive(Clone, Debug)]
pub struct Msg { pub role: String, pub content: String, pub tool_calls: Vec<J> }

fn input_of(call: &J) -> Result<J, String> {
    let name = call["function"]["name"].as_str().unwrap_or("?");
    match &call["function"]["arguments"] {
        J::Null => Ok(json!({})),
        J::String(s) if s.trim().is_empty() => Ok(json!({})),
        J::String(s) => match serde_json::from_str::<J>(s) { Ok(v @ J::Object(_)) => Ok(v), _ => Err(format!("the arguments of the call to {name} are not a JSON object")) },
        v @ J::Object(_) => Ok(v.clone()),
        _ => Err(format!("the arguments of the call to {name} are not a JSON object")),
    }
}

/// The tool specs (OpenAI function form) as the API's `tools`.
pub fn tools(specs: &[J]) -> Result<Vec<J>, String> {
    specs.iter().map(|s| {
        let f = &s["function"];
        let Some(name) = f["name"].as_str() else { return Err("a tool spec has no function name".to_string()) };
        Ok(json!({"name": name, "description": f["description"].as_str().unwrap_or(""), "input_schema": if f["parameters"].is_object() { f["parameters"].clone() } else { json!({"type": "object"}) }}))
    }).collect()
}

/// The body of the call: `max_tokens` is the owner's, `stream` is on.
pub fn body(model: &str, max_tokens: u64, msgs: &[Msg], specs: &[J]) -> Result<J, String> {
    let system = msgs.iter().filter(|m| m.role == "system").map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n\n");
    let mut out: Vec<J> = Vec::new();
    let mut owed: Vec<String> = Vec::new();      // the ids of the calls of the last assistant message that have no result yet
    let mut calls_total = 0usize;
    let mut results: Vec<J> = Vec::new();
    let flush = |out: &mut Vec<J>, owed: &mut Vec<String>, results: &mut Vec<J>, calls_total: usize| -> Result<(), String> {
        if calls_total != results.len() { return Err(format!("the last assistant message made {calls_total} tool calls but {} tool results follow it", results.len())); }
        if !results.is_empty() { out.push(json!({"role": "user", "content": std::mem::take(results)})); }
        owed.clear();
        Ok(())
    };
    for (i, m) in msgs.iter().enumerate() {
        match m.role.as_str() {
            "system" => {}
            "tool" => {
                let Some(id) = owed.get(results.len()).cloned() else { return Err("a tool message answers no tool call".into()) };
                results.push(json!({"type": "tool_result", "tool_use_id": id, "content": m.content}));
            }
            role => {
                flush(&mut out, &mut owed, &mut results, calls_total)?;
                calls_total = 0;
                if role == "assistant" {
                    let mut blocks = Vec::new();
                    if !m.content.is_empty() { blocks.push(json!({"type": "text", "text": m.content})); }
                    for (k, c) in m.tool_calls.iter().enumerate() {
                        let id = format!("toolu_{i}_{k}");
                        blocks.push(json!({"type": "tool_use", "id": id, "name": c["function"]["name"], "input": input_of(c)?}));
                        owed.push(id);
                    }
                    calls_total = owed.len();
                    if !blocks.is_empty() { out.push(json!({"role": "assistant", "content": blocks})); }
                } else {
                    out.push(json!({"role": "user", "content": [{"type": "text", "text": m.content}]}));
                }
            }
        }
    }
    flush(&mut out, &mut owed, &mut results, calls_total)?;
    let mut b = serde_json::Map::new();
    b.insert("model".into(), json!(model));
    b.insert("max_tokens".into(), json!(max_tokens));
    b.insert("stream".into(), json!(true));
    if !system.is_empty() { b.insert("system".into(), json!(system)); }
    b.insert("messages".into(), J::Array(out));
    let t = tools(specs)?;
    if !t.is_empty() { b.insert("tools".into(), J::Array(t)); }
    Ok(J::Object(b))
}
