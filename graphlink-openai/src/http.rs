//! The call, made by the system `curl` (no HTTP library: nothing new to build or fetch). The key and every header go in curl's configuration on its STDIN, never in its argv (which any
//! process can read), and the body goes in a file in a folder only this user can open; the folder is removed with the run.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

/// What the child said: a line of its stdout or stderr, or the end of one of them.
pub enum Ev { Out(String), Err(String), Closed }

/// After the body, curl writes this line and the status code (`-w`); a body that is not a stream (an error) is every other line that is not `data:`.
pub const STATUS_MARK: &str = "glstatus:";

/// A value inside a double-quoted curl configuration line: backslash, quote and line breaks escaped, so no value can end its own line or start another.
fn quoted(v: &str) -> String {
    let mut o = String::from("\"");
    for c in v.chars() { match c { '\\' => o.push_str("\\\\"), '"' => o.push_str("\\\""), '\n' => o.push_str("\\n"), '\r' => o.push_str("\\r"), '\t' => o.push_str("\\t"), c => o.push(c) } }
    o.push('"');
    o
}

/// curl's configuration (read from stdin with `-K -`): the url, the method, the headers (the key among them), the body file and the line that ends the output with the status.
pub fn curl_config(url: &str, key: Option<&str>, body_file: &str) -> String {
    let mut c = String::new();
    c.push_str(&format!("url = {}\nrequest = \"POST\"\n", quoted(url)));
    c.push_str("header = \"Content-Type: application/json\"\nheader = \"Accept: text/event-stream\"\nheader = \"Expect:\"\n");
    if let Some(k) = key { c.push_str(&format!("header = {}\n", quoted(&format!("Authorization: Bearer {k}")))); }
    c.push_str(&format!("data-binary = {}\n", quoted(&format!("@{body_file}"))));
    c.push_str(&format!("write-out = {}\n", quoted(&format!("\n{STATUS_MARK}%{{http_code}}\n"))));
    c
}

/// curl's argv: silent but for errors, unbuffered, the configuration from stdin. Nothing secret, nothing of the call.
pub fn curl_argv(_body_file: &str) -> Vec<String> { ["--silent", "--show-error", "--no-buffer", "-K", "-"].iter().map(|s| s.to_string()).collect() }

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

pub struct Run { pub child: Child, pub rx: Receiver<Ev>, dir: PathBuf }

impl Run {
    pub fn spawn(curl_bin: &str, url: &str, key: Option<&str>, body: &str) -> Result<Run, String> {
        let dir = std::env::temp_dir().join(format!("glo-{}-{}", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        let fail = |e: String| { let _ = std::fs::remove_dir_all(&dir); e };
        let file = dir.join("body");
        let file_s = file.to_string_lossy().to_string();
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file).and_then(|mut f| f.write_all(body.as_bytes())).map_err(|e| fail(format!("cannot write the body: {e}")))?;
        let mut child = Command::new(curl_bin).args(curl_argv(&file_s)).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().map_err(|e| fail(format!("cannot start {curl_bin}: {e}")))?;
        {
            let mut stdin = child.stdin.take().expect("piped");
            let _ = stdin.write_all(curl_config(url, key, &file_s).as_bytes());   // dropped at the end of this block: curl sees the end of its configuration
        }
        let (tx, rx) = channel();
        pump(child.stdout.take().expect("piped"), tx.clone(), Ev::Out);
        pump(child.stderr.take().expect("piped"), tx, Ev::Err);
        Ok(Run { child, rx, dir })
    }

    /// SIGTERM, never SIGKILL.
    pub fn term(&self) { unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM); } }
}

impl Drop for Run { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); } }
