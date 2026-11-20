//! One call of the API as a `curl` child: the key and the headers go to it as a config on its stdin (`-K -`), never on its command line, and the body from a file in a folder
//! only this process can read. Its stdout (the server's stream, line by line) and stderr come back as events. No dependency, no timeout of its own: the seat ends a turn.
use crate::handler::Config;
use serde_json::Value as J;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

pub enum Ev { Out(String), Err(String), Closed }

pub struct Call { pub child: Child, pub rx: Receiver<Ev>, dir: PathBuf }

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A value of curl's config file: in double quotes, with its escapes.
fn quoted(v: &str) -> Result<String, String> {
    if v.chars().any(|c| c.is_control()) { return Err("a header value has a control character".into()); }
    Ok(format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")))
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

impl Call {
    pub fn start(cfg: &Config, key: &str, body: &J) -> Result<Call, String> {
        let dir = std::env::temp_dir().join(format!("gla-{}-{}", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        let fail = |e: String| { let _ = std::fs::remove_dir_all(&dir); e };
        let file = dir.join("body.json");
        std::fs::write(&file, body.to_string()).map_err(|e| fail(format!("cannot write the request body: {e}")))?;
        let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
        let conf = format!(
            "url = {}\nheader = {}\nheader = {}\nheader = \"content-type: application/json\"\nheader = \"accept: text/event-stream\"\ndata-binary = {}\n",
            quoted(&url).map_err(&fail)?, quoted(&format!("x-api-key: {key}")).map_err(&fail)?, quoted(&format!("anthropic-version: {}", cfg.version)).map_err(&fail)?,
            quoted(&format!("@{}", file.display())).map_err(&fail)?);
        let mut child = Command::new(&cfg.curl_bin).args(["-sS", "-N", "-K", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().map_err(|e| fail(format!("cannot start {}: {e}", cfg.curl_bin)))?;
        {
            let mut stdin = child.stdin.take().expect("piped");
            let _ = stdin.write_all(conf.as_bytes());   // dropped at the end of this block: the pipe is closed
        }
        let (tx, rx) = channel();
        pump(child.stdout.take().expect("piped"), tx.clone(), Ev::Out);
        pump(child.stderr.take().expect("piped"), tx, Ev::Err);
        Ok(Call { child, rx, dir })
    }

    /// SIGTERM, never SIGKILL.
    pub fn term(&self) { unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM); } }
}

impl Drop for Call { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); } }
