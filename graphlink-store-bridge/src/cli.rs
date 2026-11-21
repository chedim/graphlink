//! The command line of a program that serves a store (the example does): every number is the owner's and has no default (the regions' sizes must equal what the seat declared in its `peers` fact).
use crate::peer::Numbers;
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub struct Serve {
    /// The peer's name in the seat's `peers` fact (the regions are `/gl-<name>-ctl`, `-sc`, `-pc`).
    pub peer: String,
    /// The namespace this peer owns (`<ns>::<Model>:<id>`).
    pub ns: String,
    pub numbers: Numbers,
    /// The nap between two turns, in milliseconds: how often the store is looked at.
    pub nap_ms: u64,
    pub die_with_parent: bool,
    /// The file holding the token the seat made for this peer (`<data dir>/peers/<name>.token`).
    pub token_file: Option<String>,
}

const VALUED: [&str; 10] = ["--peer", "--ns", "--slots", "--key-cap", "--val-cap", "--epoch", "--spins", "--depth", "--nap-ms", "--token-file"];

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
    Ok(Serve {
        peer: text("--peer")?, ns: text("--ns")?,
        numbers: Numbers { slots: num(&got, "--slots")?, key_cap: num(&got, "--key-cap")?, val_cap: num(&got, "--val-cap")?, epoch: num(&got, "--epoch")?, spins: num(&got, "--spins")?, depth: num(&got, "--depth")? },
        nap_ms: num(&got, "--nap-ms")?, die_with_parent, token_file: got.get("--token-file").map(|v| v.to_string()),
    })
}
