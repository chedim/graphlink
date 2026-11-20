//! One `claude` child for one turn, and the local socket its MCP shim reaches the peer by. Threads only move bytes into a channel (`Ev`); the handler decides.
use crate::handler::{Auth, Config};
use serde_json::{json, Value as J};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// What happened to the child: a line of its stdout or stderr, a stream that closed (two of them: the child is over), a tool call the shim holds.
pub enum Ev { Out(String), Err(String), Closed, Call { id: u64, name: String, args: J } }

pub struct Session {
    pub child: Child,
    pub rx: Receiver<Ev>,
    writer: Arc<Mutex<Option<UnixStream>>>,
    dir: PathBuf,
}

/// The credentials removed from the child's environment, as packs/chat_turn.c1f does: the ones that outrank the key or stand in for it, and in login mode the key itself.
/// A login token (CLAUDE_CODE_OAUTH_TOKEN) is removed only in key mode: the login legitimately uses it.
pub fn env_removed(auth: Auth) -> Vec<&'static str> {
    let mut v = vec!["ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"];
    match auth { Auth::ApiKey => v.push("CLAUDE_CODE_OAUTH_TOKEN"), Auth::Login => v.push("ANTHROPIC_API_KEY") }
    v
}

/// The child's arguments: the flags of the seat's own claude rung (packs/chat_turn.c1f, `argv`), less `--include-partial-messages` (the peer reads whole messages) and
/// `--max-turns` (the seat ends a turn by not writing the next round), plus the model alias.
pub fn argv(cfg: &Config, mcp_json: &str, system: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--tools", "", "--strict-mcp-config", "--mcp-config"].iter().map(|s| s.to_string()).collect();
    a.push(mcp_json.to_string());
    for s in ["--allowedTools", "mcp__seat__*", "--setting-sources", "", "--no-session-persistence", "--model"] { a.push(s.to_string()); }
    a.push(cfg.model.clone());
    a.push("--system-prompt".into()); a.push(system.to_string());
    a
}

fn pump<R: std::io::Read + Send + 'static>(r: R, tx: Sender<Ev>, wrap: fn(String) -> Ev) {
    std::thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => { if tx.send(wrap(String::from_utf8_lossy(&buf).trim_end().to_string())).is_err() { return; } }
            }
        }
        let _ = tx.send(Ev::Closed);
    });
}

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Session {
    /// Starts the child: the system prompt as `--system-prompt`, the user's text as the one stream-json line on stdin (then closed), the seat's tools behind the shim.
    pub fn spawn(cfg: &Config, system: &str, user: &str, specs: Vec<J>) -> Result<Session, String> {
        let dir = std::env::temp_dir().join(format!("glc-{}-{}", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        let fail = |e: String| { let _ = std::fs::remove_dir_all(&dir); e };
        let sock = dir.join("s");
        let sock_s = sock.to_string_lossy().to_string();
        if sock_s.len() > 100 { return Err(fail(format!("the socket path {sock_s} is too long for a unix socket: set TMPDIR to a shorter folder"))); }
        let listener = UnixListener::bind(&sock).map_err(|e| fail(format!("cannot bind {sock_s}: {e}")))?;
        let (tx, rx) = channel();
        let writer: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
        let (w2, tx2, specs) = (writer.clone(), tx.clone(), Arc::new(specs));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let Ok(clone) = conn.try_clone() else { continue };
                *w2.lock().unwrap() = Some(clone);
                for line in BufReader::new(conn).lines() {
                    let Ok(line) = line else { break };
                    let Ok(m) = serde_json::from_str::<J>(&line) else { continue };
                    let Some(id) = m["id"].as_u64() else { continue };
                    match m["op"].as_str() {
                        Some("list") => { if let Some(s) = w2.lock().unwrap().as_mut() { let _ = writeln!(s, "{}", json!({"id": id, "tools": *specs})); } }
                        Some("call") => { let _ = tx2.send(Ev::Call { id, name: m["name"].as_str().unwrap_or("").to_string(), args: m["arguments"].clone() }); }
                        _ => {}
                    }
                }
            }
        });
        let mcp = json!({"mcpServers": {"seat": {"type": "stdio", "command": cfg.exe.to_string_lossy(), "args": ["mcp", "--socket", sock_s]}}}).to_string();
        let mut cmd = Command::new(&cfg.claude_bin);
        cmd.args(argv(cfg, &mcp, system));
        for k in env_removed(cfg.auth) { cmd.env_remove(k); }
        cmd.env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");   // the CLI's own switch the seat's rung sets: no auto-memory keyed to the folder
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| fail(format!("cannot start {}: {e}", cfg.claude_bin)))?;
        let line = json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": user}]}});
        {
            let mut stdin = child.stdin.take().expect("piped");
            let _ = writeln!(stdin, "{line}");   // dropped at the end of this block: the pipe is closed, the child sees one message
        }
        pump(child.stdout.take().expect("piped"), tx.clone(), Ev::Out);
        pump(child.stderr.take().expect("piped"), tx, Ev::Err);
        Ok(Session { child, rx, writer, dir })
    }

    /// The result of a held call, for the shim to hand to Claude.
    pub fn answer(&self, id: u64, text: &str, error: bool) -> Result<(), String> {
        let mut g = self.writer.lock().unwrap();
        let Some(s) = g.as_mut() else { return Err("the tool shim is not connected".into()) };
        writeln!(s, "{}", json!({"id": id, "text": text, "error": error})).map_err(|e| format!("the tool shim: {e}"))
    }

    /// SIGTERM, never SIGKILL.
    pub fn term(&self) { unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM); } }
}

impl Drop for Session { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); } }
