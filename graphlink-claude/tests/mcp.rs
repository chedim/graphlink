//! The stdio MCP server Claude Code spawns (`graphlink-claude mcp --socket <path>`): it lists the seat's tools, holds each call until the peer answers it over the local socket.
use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

/// a peer end of the socket: serves the tools list, hands out calls, answers when told
fn serve_socket(path: &str, tools: J) -> (std::sync::mpsc::Receiver<J>, std::sync::Arc<std::sync::Mutex<Option<std::os::unix::net::UnixStream>>>) {
    let l = UnixListener::bind(path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let w = std::sync::Arc::new(std::sync::Mutex::new(None));
    let w2 = w.clone();
    std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        *w2.lock().unwrap() = Some(s.try_clone().unwrap());
        for line in BufReader::new(s).lines() {
            let m: J = serde_json::from_str(&line.unwrap()).unwrap();
            if m["op"] == "list" {
                let mut g = w2.lock().unwrap(); let s = g.as_mut().unwrap();
                writeln!(s, "{}", json!({"id": m["id"], "tools": tools})).unwrap();
            } else { tx.send(m).unwrap(); }
        }
    });
    (rx, w)
}

#[test]
fn the_shim_speaks_mcp_lists_the_tools_and_holds_a_call_until_it_is_answered() {
    let dir = std::env::temp_dir().join(format!("gc-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("s").to_string_lossy().to_string();
    let specs = json!([{"type": "function", "function": {"name": "echo", "description": "d", "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}}}]);
    let (calls, w) = serve_socket(&sock, specs);
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphlink-claude")).args(["mcp", "--socket", &sock]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |o: J| { writeln!(stdin, "{o}").unwrap(); stdin.flush().unwrap(); };
    let mut read = move || { let mut l = String::new(); out.read_line(&mut l).unwrap(); serde_json::from_str::<J>(&l).unwrap() };
    ask(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}));
    let r = read();
    assert_eq!(r["id"], json!(1));
    assert_eq!(r["result"]["protocolVersion"], json!("2025-03-26"), "the client's version is echoed");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    ask(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    ask(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let r = read();
    assert_eq!(r["result"]["tools"], json!([{"name": "echo", "description": "d", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]));
    ask(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "echo", "arguments": {"text": "hi"}}}));
    let c = calls.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    assert_eq!((c["name"].clone(), c["arguments"].clone()), (json!("echo"), json!({"text": "hi"})));
    // a second call while the first is held: calls are concurrent
    ask(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "echo", "arguments": {}}}));
    let c2 = calls.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    for (c, text) in [(&c2, "second"), (&c, "first")] {
        writeln!(w.lock().unwrap().as_mut().unwrap(), "{}", json!({"id": c["id"], "text": text})).unwrap();
    }
    let mut got = std::collections::HashMap::new();
    for _ in 0..2 { let r = read(); got.insert(r["id"].as_i64().unwrap(), r["result"]["content"][0]["text"].as_str().unwrap().to_string()); }
    assert_eq!(got[&3], "first"); assert_eq!(got[&4], "second");
    ask(json!({"jsonrpc": "2.0", "id": 5, "method": "nothing/known"}));
    assert_eq!(read()["error"]["code"], json!(-32601));
    drop(ask);
    let _ = child.wait();   // stdin closed: the shim ends
    let _ = std::fs::remove_dir_all(dir);
}
