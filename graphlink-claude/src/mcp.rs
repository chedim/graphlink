//! The stdio MCP server Claude Code spawns (`graphlink-claude mcp --socket <path>`), named `seat` in the config the peer hands the child. It speaks newline-delimited JSON-RPC on
//! stdin/stdout and nothing else: the tools come from the peer and each call is HELD until the peer answers it, over a local unix socket (JSON lines both ways):
//!   shim -> peer  `{"op":"list","id":n}`  -> `{"id":n,"tools":[<OpenAI function specs>]}`
//!   shim -> peer  `{"op":"call","id":n,"name":..,"arguments":{..}}`  -> later `{"id":n,"text":"..","error":false}`
//! There is no network and no timeout here: a call lasts as long as the seat takes to run the tool (the peer ends the child when the seat retires the turn).
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

type Pending = Arc<Mutex<HashMap<u64, Sender<J>>>>;

fn rpc_result(id: &J, result: J) -> J { json!({"jsonrpc": "2.0", "id": id, "result": result}) }
fn rpc_error(id: &J, code: i64, message: &str) -> J { json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}) }

pub fn run(socket: &str) -> Result<(), String> {
    let sock = UnixStream::connect(socket).map_err(|e| format!("cannot reach the peer at {socket}: {e}"))?;
    let writer = Arc::new(Mutex::new(sock.try_clone().map_err(|e| e.to_string())?));
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    // the peer's answers go to whoever asked
    let p = pending.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(sock).lines() {
            let Ok(line) = line else { break };
            let Ok(m) = serde_json::from_str::<J>(&line) else { continue };
            if let Some(tx) = m["id"].as_u64().and_then(|id| p.lock().unwrap().remove(&id)) { let _ = tx.send(m); }
        }
        std::process::exit(0);   // the peer is gone: so is the run
    });
    let next = Arc::new(Mutex::new(0u64));
    let ask = {
        let (writer, pending, next) = (writer.clone(), pending.clone(), next.clone());
        move |mut m: J| -> Result<J, String> {
            let id = { let mut n = next.lock().unwrap(); *n += 1; *n };
            m["id"] = json!(id);
            let (tx, rx) = channel();
            pending.lock().unwrap().insert(id, tx);
            writeln!(writer.lock().unwrap(), "{m}").map_err(|e| format!("the peer: {e}"))?;
            rx.recv().map_err(|_| "the peer is gone".to_string())
        }
    };
    let ask = Arc::new(ask);
    let say = {
        let out = out.clone();
        move |v: J| { let mut o = out.lock().unwrap(); let _ = writeln!(o, "{v}"); let _ = o.flush(); }
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() { continue; }
        let Ok(req) = serde_json::from_str::<J>(&line) else { continue };
        let id = req.get("id").cloned();
        let method = req["method"].as_str().unwrap_or("").to_string();
        let Some(id) = id else { continue };   // a notification (notifications/initialized...) has no answer
        match method.as_str() {
            "initialize" => say(rpc_result(&id, json!({
                "protocolVersion": req["params"]["protocolVersion"].clone(),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "seat", "version": env!("CARGO_PKG_VERSION")},
            }))),
            "ping" => say(rpc_result(&id, json!({}))),
            "tools/list" => match ask(json!({"op": "list"})) {
                Ok(m) => {
                    let tools: Vec<J> = m["tools"].as_array().into_iter().flatten().map(|t| {
                        let f = &t["function"];
                        json!({"name": f["name"], "description": f["description"], "inputSchema": if f["parameters"].is_object() { f["parameters"].clone() } else { json!({"type": "object", "properties": {}}) }})
                    }).collect();
                    say(rpc_result(&id, json!({"tools": tools})));
                }
                Err(e) => say(rpc_error(&id, -32603, &e)),
            },
            "tools/call" => {
                // a call is held on its own thread: Claude makes several in one step and they are answered in any order
                let (ask, say, out) = (ask.clone(), say.clone(), ());
                let _ = out;
                let name = req["params"]["name"].clone(); let args = req["params"]["arguments"].clone();
                std::thread::spawn(move || match ask(json!({"op": "call", "name": name, "arguments": if args.is_null() { json!({}) } else { args }})) {
                    Ok(m) => say(rpc_result(&id, json!({"content": [{"type": "text", "text": m["text"].as_str().unwrap_or("")}], "isError": m["error"] == json!(true)}))),
                    Err(e) => say(rpc_error(&id, -32603, &e)),
                });
            }
            _ => say(rpc_error(&id, -32601, &format!("method not found: {method}"))),
        }
    }
    Ok(())   // stdin closed: Claude ended
}
