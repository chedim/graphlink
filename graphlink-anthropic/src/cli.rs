//! The command line of `graphlink-anthropic serve`, the program the seat starts from its `peers` fact. Every option and number is the owner's and has no default (the regions'
//! sizes must equal what the seat declared). The API key is never an option: `--key-env` names the environment variable of this process that holds it.
use crate::peer::Numbers;
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub struct Serve {
    /// The peer's name in the seat's `peers` fact (the regions are `/gl-<name>-ctl`, `-sc`, `-pc`).
    pub peer: String,
    /// The namespace this peer owns (`<ns>::Reply:<id>`, `<ns>::Chunk:<id>:<k>`).
    pub ns: String,
    /// The `curl` executable that makes the HTTP call.
    pub curl_bin: String,
    /// The API's address without the path (the call goes to `<base-url>/v1/messages`).
    pub base_url: String,
    pub model: String,
    /// The API requires `max_tokens` on every call: this is the owner's number.
    pub max_tokens: u64,
    /// The value of the `anthropic-version` header.
    pub version: String,
    /// The NAME of the environment variable that holds the API key.
    pub key_env: String,
    pub numbers: Numbers,
    /// The nap between two turns of the loop, in milliseconds.
    pub nap_ms: u64,
    /// How long to wait, after a SIGTERM, for a `curl` to be gone, in milliseconds.
    pub term_wait_ms: u64,
    pub die_with_parent: bool,
    /// The file holding the token the seat made for this peer (`<data dir>/peers/<name>.token`).
    pub token_file: Option<String>,
}

const VALUED: [&str; 17] = ["--peer", "--ns", "--curl-bin", "--base-url", "--model", "--max-tokens", "--anthropic-version", "--key-env", "--slots", "--key-cap", "--val-cap", "--epoch", "--spins", "--depth", "--nap-ms", "--term-wait-ms", "--token-file"];

pub fn parse(args: &[String]) -> Result<Serve, String> {
    let mut got: HashMap<&str, &str> = HashMap::new();
    let mut die_with_parent = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--die-with-parent" { die_with_parent = true; i += 1; continue; }
        let Some(opt) = VALUED.iter().find(|o| **o == a) else { return Err(format!("unknown option {a}")) };
        let Some(v) = args.get(i + 1) else { return Err(format!("{opt} needs a value")) };
        got.insert(opt, v);
        i += 2;
    }
    let text = |o: &str| got.get(o).filter(|v| !v.is_empty()).map(|v| v.to_string()).ok_or_else(|| format!("missing {o} (no default: it is the owner's choice)"));
    fn num<T: std::str::FromStr>(got: &HashMap<&str, &str>, o: &str) -> Result<T, String> {
        let v = got.get(o).ok_or_else(|| format!("missing {o} (no default: it is the owner's number)"))?;
        v.parse::<T>().map_err(|_| format!("{o}: {v:?} is not a number"))
    }
    let max_tokens: u64 = num(&got, "--max-tokens")?;
    if max_tokens == 0 { return Err("--max-tokens: the API needs at least 1".into()); }
    Ok(Serve {
        peer: text("--peer")?, ns: text("--ns")?, curl_bin: text("--curl-bin")?, base_url: text("--base-url")?, model: text("--model")?, max_tokens,
        version: text("--anthropic-version")?, key_env: text("--key-env")?,
        numbers: Numbers { slots: num(&got, "--slots")?, key_cap: num(&got, "--key-cap")?, val_cap: num(&got, "--val-cap")?, epoch: num(&got, "--epoch")?, spins: num(&got, "--spins")?, depth: num(&got, "--depth")? },
        nap_ms: num(&got, "--nap-ms")?, term_wait_ms: num(&got, "--term-wait-ms")?, die_with_parent, token_file: got.get("--token-file").map(|v| v.to_string()),
    })
}
